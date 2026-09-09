//! Label archive: `dir/labels/{id}.zpl` (raw bytes), `{id}.json` (serde
//! IR), `{id}.png` (render); `dir/meta.json` holds the next id and
//! timestamps; `dir/index.json` holds the per-label row index (meta +
//! diagnostic messages, written last - the persistence debounce refreshes
//! it). `open()` recovers the next id so restarts continue numbering;
//! `push()` writes each file temp+rename (atomic-ish) and commits with
//! `meta.json` last.
//!
//! Rolling retention (Task 21): `open_with` takes the `ZPL_ARCHIVE_KEEP`
//! label cap and `ZPL_ARCHIVE_MAX_MB` byte cap; `push` prunes oldest rows
//! first (files deleted, rows dropped from the in-memory index). The
//! filesystem is the ground truth: `open` reconciles the loaded index
//! against `labels/` - committed-but-unindexed labels are re-stubbed from
//! their files, indexed rows whose files are gone are dropped, and orphan
//! files with no committed row are deleted (a crash mid-push never leaks,
//! never loses a committed label).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zpl::ir::Label;

/// One archived label's indexed metadata (Task 21's `GET /api/labels`
/// serves these rows; the SSE `label` event carries one).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelRow {
    pub id: u64,
    pub ts_ms: u64,
    pub bytes: usize,
    pub diags: usize,
    pub width: u32,
    pub height: u32,
}

/// A row-index entry: the meta row plus the label's diagnostic messages
/// (the detail endpoint serves them; the list endpoint serves only
/// `row`). `row.diags` is the message count at archive time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub row: LabelRow,
    /// Human-readable diagnostic messages, parse order.
    pub messages: Vec<String>,
}

/// `dir/meta.json` contents (commit point of a push).
#[derive(Serialize, Deserialize)]
struct Meta {
    next_id: u64,
    created_ms: u64,
    last_push_ms: u64,
}

const META_FILE: &str = "meta.json";
const INDEX_FILE: &str = "index.json";

/// Default rolling-retention cap: keep the newest 10_000 labels.
pub const DEFAULT_KEEP: usize = 10_000;
/// Default rolling-retention byte cap: 1024 MiB (of raw label bytes; the
/// JSON/PNG companions are excluded - a best-effort knob, see `prune`).
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// The archive directory plus the next id to issue and the live row index.
pub struct Archive {
    pub dir: PathBuf,
    next_id: u64,
    created_ms: u64,
    /// Newest at the end; `push` appends, `prune` drains the front.
    rows: Vec<IndexEntry>,
    /// Sum of `row.bytes` for live rows; O(1) prune accounting.
    sum_bytes: u64,
    keep: usize,
    max_bytes: u64,
    /// Oldest-first prunes performed since this archive was opened.
    prunes: u64,
}

impl Archive {
    /// Open with the default retention caps (`DEFAULT_KEEP` /
    /// `DEFAULT_MAX_BYTES`).
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        Self::open_with(dir, DEFAULT_KEEP, DEFAULT_MAX_BYTES)
    }

    /// Open (creating if needed) the archive at `dir` with explicit
    /// retention caps (env `ZPL_ARCHIVE_KEEP` / `ZPL_ARCHIVE_MAX_MB` in
    /// main.rs; small values in tests). Recovers the commit state: the
    /// next id from `meta.json` (falling back to a `labels/` scan) and the
    /// row index from `index.json`, reconciled against the files on disk.
    pub fn open_with(dir: PathBuf, keep: usize, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir.join("labels"))?;
        let now = unix_ms();
        let meta = fs::read(dir.join(META_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
        let (next_id, created_ms) = match meta {
            Some(m) => (m.next_id, m.created_ms),
            None => (highest_label_id(&dir.join("labels")).saturating_add(1), now),
        };
        let rows = fs::read(dir.join(INDEX_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<IndexEntry>>(&b).ok())
            .unwrap_or_default();
        let mut archive = Archive {
            dir,
            next_id,
            created_ms,
            rows,
            sum_bytes: 0,
            keep,
            max_bytes,
            prunes: 0,
        };
        archive.reconcile();
        Ok(archive)
    }

    /// Re-align the in-memory index with `labels/` and the commit point
    /// (`meta.next_id` - filesystem files are not the commit truth, the
    /// counter is):
    /// - indexed rows whose `{id}.zpl` is gone (pruned, then the debounced
    ///   `index.json` write was lost) are dropped;
    /// - ids on disk below `next_id` that the index is missing were
    ///   committed (their `meta.json` write landed) but their index write
    ///   was lost - they are re-stubbed from their files (`ts_ms` from
    ///   mtime, `bytes` from the raw length, width and height from the IR;
    ///   diagnostic messages are not recoverable - they were never
    ///   written) ;
    /// - ids at or above `next_id` were never committed (the push failed
    ///   before `meta.json` advanced) - their files are deleted so a retry
    ///   re-uses the id.
    ///
    /// When `meta.json` is itself gone, the scan-derived `next_id` sits
    /// above every file, so everything on disk is treated as committed -
    /// the best recovery possible without the commit point.
    fn reconcile(&mut self) {
        let labels_dir = self.dir.join("labels");
        let mut on_disk: BTreeSet<u64> = BTreeSet::new();
        if let Ok(entries) = fs::read_dir(&labels_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".zpl")) {
                    if let Ok(n) = stem.parse::<u64>() {
                        on_disk.insert(n);
                    }
                }
            }
        }
        let before = self.rows.len();
        self.rows.retain(|e| on_disk.contains(&e.row.id));
        if self.rows.len() != before {
            crate::log!(
                "archive: dropped {} indexed row(s) whose files are gone",
                before - self.rows.len()
            );
        }

        let committed: Vec<u64> = on_disk
            .iter()
            .copied()
            .filter(|id| *id < self.next_id)
            .collect();
        let known: BTreeSet<u64> = self.rows.iter().map(|e| e.row.id).collect();
        for id in committed.iter().filter(|id| !known.contains(id)) {
            match stub_row(&labels_dir, *id) {
                Some(entry) => self.rows.push(entry),
                None => {
                    // The IR is unreadable - the label is broken; remove all three files.
                    for ext in ["zpl", "json", "png"] {
                        let _ = fs::remove_file(labels_dir.join(format!("{id}.{ext}")));
                    }
                    crate::log!("archive: deleted unreadable label files {id}");
                }
            }
        }
        let stubbed = self.rows.len() - known.len();
        if stubbed > 0 {
            crate::log!("archive: re-stubbed {stubbed} committed label(s) from files");
        }

        for id in on_disk
            .iter()
            .filter(|id| !known.contains(id) && **id >= self.next_id)
        {
            for ext in ["zpl", "json", "png"] {
                let _ = fs::remove_file(labels_dir.join(format!("{id}.{ext}")));
            }
            crate::log!("archive: deleted {id} (files written without a commit)");
        }

        self.rows.sort_by_key(|e| e.row.id);
        self.sum_bytes = self.rows.iter().map(|e| e.row.bytes as u64).sum();
    }

    /// Archive one label: writes `{id}.zpl` / `{id}.json` / `{id}.png`,
    /// appends the row index, bumps `meta.json` (the commit), then applies
    /// rolling retention. Returns the assigned id (`row.id` is filled in -
    /// the caller cannot know it before the push).
    ///
    /// Error semantics: the id is consumed only once a file for it exists.
    /// A failure before `next_id` advances re-uses the id on retry; the
    /// `meta.json` commit is written last - if THAT write fails, the files
    /// for the id are already on disk, so the in-process counter advances
    /// anyway and a restart repairs the count via the `labels/` scan (the
    /// id is never re-written with different content).
    pub fn push(
        &mut self,
        row: &mut LabelRow,
        raw: &[u8],
        ir: &Label,
        png: &[u8],
        messages: &[String],
    ) -> io::Result<u64> {
        let id = self.next_id;
        let labels = self.dir.join("labels");
        write_atomic(&labels.join(format!("{id}.zpl")), raw)?;
        write_atomic(
            &labels.join(format!("{id}.json")),
            &serde_json::to_vec(ir).map_err(io::Error::other)?,
        )?;
        write_atomic(&labels.join(format!("{id}.png")), png)?;
        self.next_id += 1;
        let meta = Meta {
            next_id: self.next_id,
            created_ms: self.created_ms,
            last_push_ms: row.ts_ms,
        };
        write_atomic(
            &self.dir.join(META_FILE),
            &serde_json::to_vec(&meta).map_err(io::Error::other)?,
        )?;
        row.id = id;
        self.rows.push(IndexEntry {
            row: row.clone(),
            messages: messages.to_vec(),
        });
        self.sum_bytes += row.bytes as u64;
        self.prune();
        Ok(id)
    }

    /// Rolling retention, best-effort: keep the newest `keep` rows, then
    /// keep total raw bytes under `max_bytes`. Oldest rows first. A failed
    /// file deletion leaves the row in place (never half-pruned) and the
    /// remaining rows still get their chance `ponytail:` - `sum_bytes`
    /// counts raw `.zpl` bytes only (JSON/PNG companions excluded): a
    /// near-zero-cost O(1) knob; switch to on-disk size if disk usage ever
    /// matters.
    fn prune(&mut self) {
        let mut removed = 0u64;
        while self.rows.len() > self.keep {
            if !self.remove_oldest() {
                break;
            }
            removed += 1;
        }
        while self.sum_bytes > self.max_bytes && !self.rows.is_empty() {
            if !self.remove_oldest() {
                break;
            }
            removed += 1;
        }
        if removed > 0 {
            self.prunes += removed;
            crate::log!(
                "archive: pruned {removed} label(s) (keep={}, max_bytes={}); oldest first",
                self.keep,
                self.max_bytes
            );
        }
    }

    /// Delete the newest-of-all oldest row's three files and drop it from
    /// the index. `false` when a deletion fails (or the index is empty).
    fn remove_oldest(&mut self) -> bool {
        let Some(entry) = self.rows.first() else {
            return false;
        };
        let id = entry.row.id;
        let labels = self.dir.join("labels");
        let ok = ["zpl", "json", "png"]
            .iter()
            .all(|ext| delete_if_present(&labels.join(format!("{id}.{ext}"))));
        if ok {
            self.sum_bytes = self.sum_bytes.saturating_sub(entry.row.bytes as u64);
            self.rows.remove(0);
        }
        ok
    }

    /// Delete the retained label `id`: remove its three files first; a
    /// failed deletion leaves the row indexed (never half-pruned). Returns
    /// whether the id existed and was removed. The debounced `index.json`
    /// write picks up the row drop.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(pos) = self.rows.iter().position(|e| e.row.id == id) else {
            return false;
        };
        let labels = self.dir.join("labels");
        let ok = ["zpl", "json", "png"]
            .iter()
            .all(|ext| delete_if_present(&labels.join(format!("{id}.{ext}"))));
        if ok {
            self.sum_bytes = self
                .sum_bytes
                .saturating_sub(self.rows[pos].row.bytes as u64);
            self.rows.remove(pos);
        }
        ok
    }

    /// Delete every retained label. Returns how many were removed (a row
    /// whose files fail to delete stays).
    pub fn clear(&mut self) -> usize {
        let ids: Vec<u64> = self.rows.iter().map(|e| e.row.id).collect();
        ids.into_iter().filter(|&id| self.remove(id)).count()
    }

    /// Labels currently retained (`next_id - 1` historical; ids start at 1).
    pub fn len(&self) -> usize {
        self.next_id.saturating_sub(1) as usize
    }

    /// Whether no labels are currently retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The live row index, oldest first.
    pub fn rows(&self) -> &[IndexEntry] {
        &self.rows
    }

    /// Row-index entry for `id`, if present.
    pub fn find_row(&self, id: u64) -> Option<&IndexEntry> {
        self.rows.iter().find(|e| e.row.id == id)
    }

    /// Raw `.zpl` bytes of `id`; `Ok(None)` when the file is absent.
    pub fn read_raw(&self, id: u64) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.labels_dir().join(format!("{id}.zpl"))) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parsed IR of `id` from `{id}.json`; `Ok(None)` when absent.
    pub fn read_ir(&self, id: u64) -> io::Result<Option<Label>> {
        match fs::read(self.labels_dir().join(format!("{id}.json"))) {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// PNG bytes of `id`; `Ok(None)` when absent.
    pub fn read_png(&self, id: u64) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.labels_dir().join(format!("{id}.png"))) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Overwrite `{id}.png` with a fresh render (`POST .../rerender`).
    pub fn write_png(&self, id: u64, png: &[u8]) -> io::Result<()> {
        write_atomic(&self.labels_dir().join(format!("{id}.png")), png)
    }

    /// Oldest-first prunes performed since this archive was opened (the
    /// `/api/diagnostics` "since boot" view).
    pub fn pruned(&self) -> u64 {
        self.prunes
    }

    fn labels_dir(&self) -> PathBuf {
        self.dir.join("labels")
    }
}

/// Rebuild a row-index entry for an on-disk label whose index write was
/// lost: metadata from the files (mtime, raw length, IR geometry).
/// Diagnostic messages are not recoverable (`row.diags = 0` is the honest
/// admission). `None` when the IR is unreadable - the label is broken.
fn stub_row(labels_dir: &Path, id: u64) -> Option<IndexEntry> {
    let ir: Label = fs::read(labels_dir.join(format!("{id}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())?;
    let ts_ms = fs::metadata(labels_dir.join(format!("{id}.zpl")))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let bytes = fs::metadata(labels_dir.join(format!("{id}.zpl")))
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    Some(IndexEntry {
        row: LabelRow {
            id,
            ts_ms,
            bytes,
            diags: 0,
            width: ir.width_dots,
            height: ir.height_dots,
        },
        messages: Vec::new(),
    })
}

/// `fs::remove_file`, treating a missing file as success (idempotent).
fn delete_if_present(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Highest existing `{id}.zpl` under `labels_dir`, or 0 if none.
fn highest_label_id(labels_dir: &Path) -> u64 {
    let mut max = 0u64;
    if let Ok(entries) = fs::read_dir(labels_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".zpl")) {
                if let Ok(n) = stem.parse::<u64>() {
                    max = max.max(n);
                }
            }
        }
    }
    max
}

/// Write `data` to `path` via a sibling temp file + rename, so a crash
/// never leaves a half-written archive file under its final name.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Milliseconds since the Unix epoch (0 if the clock is pre-epoch).
pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
