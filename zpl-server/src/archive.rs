//! Label archive: `dir/labels/{id}.zpl` (raw bytes), `{id}.json` (serde
//! IR), `{id}.png` (render); `dir/meta.json` holds the next id and
//! timestamps. `open()` recovers the next id so restarts continue
//! numbering; `push()` writes each file temp+rename (atomic-ish) with
//! `meta.json` written last as the commit point.
//!
//! Rolling retention is Task 21's job (`ZPL_ARCHIVE_KEEP`/`_MAX_MB`); this
//! module never deletes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zpl::ir::Label;

/// One archived label's indexed metadata (Task 21's `GET /api/labels`
/// serves these rows).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LabelRow {
    pub id: u64,
    pub ts_ms: u64,
    pub bytes: usize,
    pub diags: usize,
    pub width: u32,
    pub height: u32,
}

/// `dir/meta.json` contents (commit point of a push).
#[derive(Serialize, Deserialize)]
struct Meta {
    next_id: u64,
    created_ms: u64,
    last_push_ms: u64,
}

const META_FILE: &str = "meta.json";

/// The archive directory plus the next id to issue.
pub struct Archive {
    pub dir: PathBuf,
    next_id: u64,
    created_ms: u64,
}

impl Archive {
    /// Open (creating if needed) the archive at `dir`. Recovers `next_id`
    /// from `meta.json`; if meta is missing or unreadable, scans
    /// `dir/labels` for the highest id (so a wiped meta never re-issues an
    /// id) and starts after it - a fresh dir starts at 1.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(dir.join("labels"))?;
        let now = unix_ms();
        let meta = fs::read(dir.join(META_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
        let (next_id, created_ms) = match meta {
            Some(m) => (m.next_id, m.created_ms),
            None => (highest_label_id(&dir.join("labels")).saturating_add(1), now),
        };
        Ok(Archive {
            dir,
            next_id,
            created_ms,
        })
    }

    /// Archive one label: writes `{id}.zpl` / `{id}.json` / `{id}.png`,
    /// bumps `meta.json`, and returns the assigned id. `row.id` is filled
    /// in here (the caller cannot know it before the push).
    ///
    /// Error semantics: the id is consumed only once a file for it exists.
    /// A failure before `next_id` advances re-uses the id on retry; the
    /// `meta.json` commit is written last - if THAT write fails, the files
    /// for the id are already on disk, so the in-process counter advances
    /// anyway and a restart repairs the count via the `labels/` scan (the
    /// id is never re-written with different content).
    pub fn push(&mut self, row: &mut LabelRow, raw: &[u8], ir: &Label, png: &[u8]) -> io::Result<u64> {
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
        Ok(id)
    }

    /// Labels since the id counter started (`next_id - 1`; ids start at 1).
    pub fn len(&self) -> usize {
        self.next_id.saturating_sub(1) as usize
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
fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
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
