//! Archive unit tests (Task 21): direct `Archive` API coverage for the
//! crash-recovery/reconcile paths, the rolling-retention failure behavior,
//! and the read/write error branches that the TCP/HTTP integration tests
//! cannot reach (files on disk are shaped by hand here).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use zpl::ir::Label;
use zpl_server::archive::{Archive, IndexEntry, LabelRow, DEFAULT_KEEP, DEFAULT_MAX_BYTES};

fn test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("zpl-arc-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A minimal but valid IR: one `^GB` box on a default-width label.
fn small_ir() -> Label {
    let mut diags = zpl::diag::Diagnostics::default();
    let mut mem = zpl::memory::Memory::default();
    let mut ctx = zpl::state::ParseContext::new(&mut diags, &mut mem);
    let mut labels = Vec::new();
    zpl::cmd::parse_stream_collect(
        &mut zpl::scanner::Scanner::new(b"^XA^GB50,50,2,B^XZ"),
        &mut ctx,
        &mut labels,
    );
    labels.pop().unwrap().label
}

/// Write `{id}.zpl / {id}.json / {id}.png` plus a `meta.json` whose
/// `next_id` claims a commit (the shape a push leaves behind when its
/// `index.json` write was lost).
fn commit_files(dir: &std::path::Path, id: u64, next_id: u64, ir: &Label) {
    let labels = dir.join("labels");
    std::fs::create_dir_all(&labels).unwrap();
    std::fs::write(labels.join(format!("{id}.zpl")), b"^XA^GB50,50,2,B^XZ").unwrap();
    std::fs::write(
        labels.join(format!("{id}.json")),
        serde_json::to_vec(ir).unwrap(),
    )
    .unwrap();
    std::fs::write(labels.join(format!("{id}.png")), [0u8; 8]).unwrap();
    let meta = serde_json::json!({
        "next_id": next_id,
        "created_ms": 1,
        "last_push_ms": 1,
    });
    std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
}

#[test]
fn open_recovers_next_id_from_meta_and_index() {
    let dir = test_dir("meta");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open(dir.clone()).unwrap();
    assert!(arc.is_empty(), "fresh archive");
    assert_eq!(arc.len(), 0);

    // Push one label: all three files + meta.json + index entry land.
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    let ir = small_ir();
    let id = arc
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &ir, &[0u8; 8], &[])
        .unwrap();
    assert_eq!(id, 1, "first id is 1");
    assert_eq!(arc.len(), 1);
    assert!(!arc.is_empty());
    drop(arc);

    // Reopen: the Some(meta) branch restores next_id and the index rows.
    let mut arc2 = Archive::open(dir.clone()).unwrap();
    assert_eq!(arc2.len(), 1);
    assert_eq!(arc2.rows()[0].row.id, 1);
    assert_eq!(arc2.find_row(1).unwrap().row.width, 832);
    let mut row2 = LabelRow {
        id: 0,
        ts_ms: 2,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    let id2 = arc2
        .push(&mut row2, b"^XA^GB50,50,2,B^XZ", &ir, &[0u8; 8], &[])
        .unwrap();
    assert_eq!(id2, 2, "next_id continued past the restart");
}

#[test]
fn reopen_stubs_committed_but_unindexed_labels() {
    let dir = test_dir("stub");
    let _keep = Cleanup(dir.clone());
    // The push's files + meta commit exist; index.json was never written.
    let ir = small_ir();
    commit_files(&dir, 1, 2, &ir);

    let arc = Archive::open(dir.clone()).unwrap();
    assert_eq!(arc.len(), 1, "restart counts the committed label");
    let entry = arc.find_row(1).unwrap();
    assert_eq!(entry.row.diags, 0, "messages are not recoverable");
    assert_eq!(
        entry.row.bytes,
        b"^XA^GB50,50,2,B^XZ".len(),
        "bytes from the raw file length"
    );
    assert_eq!(entry.row.width, ir.width_dots, "geometry from the IR");
    assert_eq!(entry.row.height, ir.height_dots);
}

#[test]
fn reopen_deletes_unreadable_committed_label() {
    let dir = test_dir("stub-bad");
    let _keep = Cleanup(dir.clone());
    // The .zpl/.png exist but the IR is garbage: stub_row returns None and
    // the three files are deleted (a broken label is never kept).
    let labels = dir.join("labels");
    std::fs::create_dir_all(&labels).unwrap();
    std::fs::write(labels.join("1.zpl"), b"^XA^GB50,50,2,B^XZ").unwrap();
    std::fs::write(labels.join("1.json"), b"{not json").unwrap();
    std::fs::write(labels.join("1.png"), [0u8; 8]).unwrap();
    let meta = serde_json::json!({ "next_id": 2, "created_ms": 1, "last_push_ms": 1 });
    std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();

    let arc = Archive::open(dir.clone()).unwrap();
    assert_eq!(arc.rows().len(), 0, "broken label is dropped, not stubbed");
    drop(arc);
    for ext in ["zpl", "json", "png"] {
        assert!(
            !labels.join(format!("1.{ext}")).exists(),
            "{ext} removed with its broken label"
        );
    }
}

#[test]
fn reopen_drops_indexed_rows_whose_files_are_gone() {
    let dir = test_dir("gone");
    let _keep = Cleanup(dir.clone());
    // index.json + meta.json claim label 1, but its files were pruned and
    // the debounced index write was lost before the prune landed.
    let ir = small_ir();
    commit_files(&dir, 1, 2, &ir);
    let row = LabelRow {
        id: 1,
        ts_ms: 1,
        bytes: 28,
        diags: 0,
        width: 832,
        height: 50,
    };
    let idx = vec![IndexEntry {
        row,
        messages: vec![],
    }];
    std::fs::write(dir.join("index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
    // The files are gone (a prune raced the index write).
    let labels = dir.join("labels");
    std::fs::remove_file(labels.join("1.zpl")).unwrap();
    std::fs::remove_file(labels.join("1.json")).unwrap();
    std::fs::remove_file(labels.join("1.png")).unwrap();

    let arc = Archive::open(dir.clone()).unwrap();
    assert!(
        arc.find_row(1).is_none(),
        "indexed row without files is dropped"
    );
    assert!(arc.rows().is_empty());
}

#[test]
fn reopen_deletes_files_never_committed() {
    let dir = test_dir("orphan");
    let _keep = Cleanup(dir.clone());
    // The push wrote files for id 3 but died before meta.json advanced (the
    // filesystem write failed). next_id says 2, so 3 is an orphan.
    let ir = small_ir();
    commit_files(&dir, 3, 2, &ir);
    let labels = dir.join("labels");

    let arc = Archive::open(dir.clone()).unwrap();
    assert!(arc.rows().is_empty(), "orphan rows never enter the index");
    drop(arc);
    for ext in ["zpl", "json", "png"] {
        assert!(
            !labels.join(format!("3.{ext}")).exists(),
            "orphan {ext} deleted on recovery"
        );
    }
}

#[test]
fn reopen_ignores_junk_filenames_and_recovers_highest_id() {
    let dir = test_dir("junk");
    let _keep = Cleanup(dir.clone());
    // No meta.json: next_id comes from a labels/ scan. Junk files and
    // non-numeric stems must be ignored; 7.zpl drives next_id = 8.
    let labels = dir.join("labels");
    std::fs::create_dir_all(&labels).unwrap();
    std::fs::write(labels.join("readme.txt"), b"hi").unwrap();
    std::fs::write(labels.join("7.zpl"), b"x").unwrap();
    std::fs::write(labels.join("nope.zpl"), b"y").unwrap();
    std::fs::write(dir.join("meta.json"), b"{not json").unwrap(); // unreadable meta

    let mut arc = Archive::open(dir.clone()).unwrap();
    assert!(arc.rows().is_empty(), "junk scan finds no indexable rows");
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    let id = arc
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
        .unwrap();
    assert_eq!(id, 8, "next_id continues above the scanned 7");
}

#[test]
fn prune_stops_when_deletion_fails() {
    let dir = test_dir("prune-fail");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open_with(dir.clone(), 1, DEFAULT_MAX_BYTES).unwrap();
    let ir = small_ir();
    let push = |arc: &mut Archive, ts: u64| {
        let mut row = LabelRow {
            id: 0,
            ts_ms: ts,
            bytes: 10,
            diags: 0,
            width: 832,
            height: 50,
        };
        arc.push(&mut row, b"^XA^GB50,50,2,B^XZ", &ir, &[0u8; 8], &[])
            .unwrap();
    };
    push(&mut arc, 1);
    // Replace label 1's .png with a DIRECTORY so delete_if_present fails
    // (remove_file on a dir is an error, not NotFound) -> remove_oldest
    // returns false -> prune gives up instead of half-pruning.
    let labels = dir.join("labels");
    std::fs::remove_file(labels.join("1.png")).unwrap();
    std::fs::create_dir(labels.join("1.png")).unwrap();
    push(&mut arc, 2);

    assert_eq!(
        arc.rows().len(),
        2,
        "failed deletion keeps the row in place"
    );
    assert_eq!(arc.pruned(), 0, "no prune counted when deletion failed");
    drop(arc);
    std::fs::remove_dir_all(labels.join("1.png")).unwrap();
}

#[test]
fn byte_cap_prunes_oldest_first() {
    let dir = test_dir("bytes");
    let _keep = Cleanup(dir.clone());
    // keep is huge; the byte cap drives the prune. Two 20-byte labels under
    // a 30-byte cap leave exactly the newest one.
    let mut arc = Archive::open_with(dir.clone(), 1000, 30).unwrap();
    let ir = small_ir();
    for ts in 1..=2u64 {
        let mut row = LabelRow {
            id: 0,
            ts_ms: ts,
            bytes: 20,
            diags: 0,
            width: 832,
            height: 50,
        };
        arc.push(&mut row, &[b'a'; 20], &ir, &[0u8; 8], &[])
            .unwrap();
    }
    assert_eq!(arc.rows().len(), 1, "byte cap keeps the newest");
    assert_eq!(arc.rows()[0].row.id, 2);
    assert_eq!(arc.pruned(), 1);
}

#[test]
fn read_paths_report_non_notfound_errors() {
    let dir = test_dir("read-err");
    let _keep = Cleanup(dir.clone());
    let arc = Archive::open(dir.clone()).unwrap();
    // A DIRECTORY where 1.zpl should be: fs::read errors with EISDIR, which
    // is not NotFound, so the Err branch must surface (not Ok(None)).
    let labels = dir.join("labels");
    for name in ["1.zpl", "1.json", "1.png"] {
        std::fs::create_dir(labels.join(name)).unwrap();
    }
    assert!(arc.read_raw(1).is_err());
    assert!(arc.read_ir(1).is_err());
    assert!(arc.read_png(1).is_err());
    // Missing files are Ok(None), not errors.
    assert!(arc.read_raw(99).unwrap().is_none());
}

#[test]
fn push_failure_reraises_and_reuses_id() {
    let dir = test_dir("push-err");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open(dir.clone()).unwrap();
    // A DIRECTORY at 1.zpl makes the eventual rename fail.
    let labels = dir.join("labels");
    std::fs::create_dir(labels.join("1.zpl")).unwrap();
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    assert!(
        arc.push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
            .is_err(),
        "push onto an occupied path fails"
    );
    assert_eq!(arc.len(), 0, "failed push leaves no row");
    drop(arc);

    // Clean the blocker; a fresh archive over the same dir must re-use id 1.
    std::fs::remove_dir_all(labels.join("1.zpl")).unwrap();
    let mut arc2 = Archive::open(dir).unwrap();
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    let id = arc2
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
        .unwrap();
    assert_eq!(id, 1);
}

/// The JSON companion write can fail independently of the raw: a directory
/// squatting on 1.json makes write_atomic's rename fail, and the push must
/// report the error without leaving a half-row.
#[test]
fn push_json_write_failure_propagates() {
    let dir = test_dir("push-json-err");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open(dir.clone()).unwrap();
    let labels = dir.join("labels");
    // Block the .json rename, not the .zpl one.
    std::fs::create_dir(labels.join("1.json")).unwrap();
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    assert!(arc
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
        .is_err());
    assert_eq!(arc.rows().len(), 0, "no row indexed on failed push");
    // The raw landed but the json did not; on restart the file set is
    // incomplete, so the orphan .zpl is cleaned up like any other.
    drop(arc);
    std::fs::remove_dir(labels.join("1.json")).unwrap();
    let arc = Archive::open(dir.clone()).unwrap();
    assert!(arc.rows().is_empty());
    assert!(
        !labels.join("1.zpl").exists(),
        "incomplete file set dropped"
    );
}

/// The meta.json commit is written last: if THAT fails, the files exist but
/// the counter must NOT advance past the uncommitted id (the doc: a restart
/// repairs via the labels/ scan).
#[test]
fn push_meta_write_failure_reuses_id_after_restart() {
    let dir = test_dir("push-meta-err");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open(dir.clone()).unwrap();
    // Block only the meta commit.
    std::fs::create_dir(dir.join("meta.json")).unwrap();
    let mut row = LabelRow {
        id: 0,
        ts_ms: 1,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    assert!(arc
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
        .is_err());
    assert_eq!(
        arc.rows().len(),
        0,
        "uncommitted push is not indexed in memory"
    );
    drop(arc);

    std::fs::remove_dir(dir.join("meta.json")).unwrap();
    let mut arc = Archive::open(dir.clone()).unwrap();
    // The files for id 1 are on disk with no commit: the restart scans them
    // as committed-but-unindexed and stubs a row for id 1.
    assert_eq!(arc.len(), 1, "restart recovers the orphan id");
    assert_eq!(arc.rows()[0].row.id, 1);
    let mut row = LabelRow {
        id: 0,
        ts_ms: 2,
        bytes: 10,
        diags: 0,
        width: 832,
        height: 50,
    };
    let id = arc
        .push(&mut row, b"^XA^GB50,50,2,B^XZ", &small_ir(), &[0u8; 8], &[])
        .unwrap();
    assert_eq!(id, 2, "committed id 1 is never re-written");
}

/// The byte-cap prune loop also bails on a failed deletion (the keep-loop
/// failure is covered separately): with keep high and the byte cap low, the
/// second loop's `break` fires and the row survives.
#[test]
fn byte_cap_prune_stops_on_failed_deletion() {
    let dir = test_dir("bytes-fail");
    let _keep = Cleanup(dir.clone());
    let mut arc = Archive::open_with(dir.clone(), 1000, 30).unwrap();
    let ir = small_ir();
    let push = |arc: &mut Archive, ts: u64| {
        let mut row = LabelRow {
            id: 0,
            ts_ms: ts,
            bytes: 20,
            diags: 0,
            width: 832,
            height: 50,
        };
        arc.push(&mut row, &[b'a'; 20], &ir, &[0u8; 8], &[])
            .unwrap();
    };
    push(&mut arc, 1);
    // Squat a directory on the oldest row's .png so its deletion fails.
    let labels = dir.join("labels");
    std::fs::remove_file(labels.join("1.png")).unwrap();
    std::fs::create_dir(labels.join("1.png")).unwrap();
    push(&mut arc, 2);

    assert_eq!(arc.rows().len(), 2, "failed deletion keeps both rows");
    assert_eq!(arc.pruned(), 0);
    drop(arc);
    std::fs::remove_dir_all(labels.join("1.png")).unwrap();
}

// DEFAULT_KEEP is used through open_with above; keep the import honest.
#[allow(dead_code)]
const _: usize = DEFAULT_KEEP;
