//! End-to-end TCP session tests: a real listener on an ephemeral port, a
//! real connection, bytes in -> archive rows out. Parse garbage must never
//! panic or close the socket; labels must archive with exact raw spans.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use zpl::ir::Label;
use zpl_server::archive::Archive;
use zpl_server::tcp::{spawn_tcp, AppState};

/// A fresh throwaway archive dir per test (removed on drop).
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

/// Start the in-process accept loop on an ephemeral port.
async fn spawn(dir: PathBuf) -> (SocketAddr, Arc<AppState>) {
    let state = Arc::new(AppState::new(Archive::open(dir).unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    spawn_tcp(listener, state.clone());
    (addr, state)
}

async fn wait_labels(state: &AppState, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.counters.labels.load(Ordering::Relaxed) < n {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} archived label(s)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_warnings(state: &AppState, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.counters.warnings.load(Ordering::Relaxed) < n {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} warning(s)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn one_label_archived_with_raw_ir_and_png() {
    let dir = test_dir("one");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let payload = b"^XA^GB50,50,2,B^XZ";
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(payload).await.unwrap();
    sock.shutdown().await.unwrap(); // EOF ends the session

    wait_labels(&state, 1).await;
    {
        let arc = state.archive.lock().await;
        assert_eq!(arc.len(), 1, "archive must hold exactly one row");
    }

    let labels = dir.join("labels");
    assert_eq!(
        std::fs::read(labels.join("1.zpl")).unwrap(),
        payload,
        "raw .zpl must match the sent bytes exactly"
    );
    let ir: Label = serde_json::from_slice(&std::fs::read(labels.join("1.json")).unwrap()).unwrap();
    assert_eq!(ir.width_dots, 832, "default ^PW at 203 dpi");
    assert_eq!(ir.fields.len(), 1, "one ^GB field");
    let png = std::fs::read(labels.join("1.png")).unwrap();
    assert!(!png.is_empty(), "PNG must be non-empty");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    let _ = state;
}

#[tokio::test]
async fn two_labels_in_one_connection_each_archived_with_own_raw() {
    let dir = test_dir("two");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // One write, one chunk: the driver must yield BOTH ^XZ-closed labels.
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^GB50,50,2,B^XZ^XA^GB20,20,2,B^XZ")
        .await
        .unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 2).await;
    {
        let arc = state.archive.lock().await;
        assert_eq!(arc.len(), 2);
    }
    let labels = dir.join("labels");
    assert_eq!(
        std::fs::read(labels.join("1.zpl")).unwrap(),
        b"^XA^GB50,50,2,B^XZ"
    );
    assert_eq!(
        std::fs::read(labels.join("2.zpl")).unwrap(),
        b"^XA^GB20,20,2,B^XZ"
    );
}

#[tokio::test]
async fn two_labels_in_one_chunk_attribute_diagnostics_per_label() {
    let dir = test_dir("diag-attr");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // One chunk, two formats; only label 2 has a malformed param (a ^FO
    // with a non-numeric y). Rows must carry their own diagnostics: label
    // 1's row 0, label 2's row the ^FO warning. (Before the diag-window
    // fix, every diagnostic in the chunk was charged to the first label.)
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^GB50,50,2,B^XZ^XA^FO10,abc^FDx^FS^XZ")
        .await
        .unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 2).await;
    {
        let arc = state.archive.lock().await;
        let rows = arc.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].row.diags, 0,
            "label 1 must not inherit label 2's diagnostics"
        );
        assert_eq!(
            rows[1].row.diags, 1,
            "label 2's row carries its own warning"
        );
        assert_eq!(rows[1].messages.len(), 1);
        assert!(
            rows[1].messages[0].contains("abc"),
            "the ^FO warning names the bad y: {}",
            rows[1].messages[0]
        );
    }
    // The warning still counts toward the boot totals (buffer-wide).
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.counters.warnings.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "warning counter never bumped");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn labels_across_read_chunks_attribute_diagnostics_to_their_own_label() {
    let dir = test_dir("diag-attr-split");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Chunk 1: a stray unknown command (legit warning OUTSIDE any
    // format), then `^XA` + a ^FO with a non-numeric y (the same warning
    // construct as the one-chunk per-label test above) + a partial field,
    // NO ^XZ - label 1 stays open across the read boundary. The stray
    // warning lands in the diag buffer BEFORE label 1's ^XA, so label 1's
    // window must start at that ^XA (diag_start = 1), not at buffer start.
    // Chunk 2: close label 1 (`^FS^XZ`) and send a clean second label.
    //
    // Without the diag_start carry-over, label 1's ^XZ re-enters the
    // parse with a fresh diag_start = 0 and its window slurps BOTH
    // diagnostics; the clean label's own ^XA refresh protects it, so the
    // assertions below pin label 1 to exactly its own warning. (A label
    // whose only diagnostics sit after its ^XA is indifferent to the
    // carry-over - the ^XA handler re-inits diag_start - which is why the
    // stray is needed to observe the cross-chunk bug.)
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^QX stray\r\n^XA^FO10,abc^FDpartial")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await; // separate reads
    sock.write_all(b"^FS^XZ^XA^FO0,0^A0N,20,20^FDclean^FS^XZ")
        .await
        .unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 2).await;
    {
        let arc = state.archive.lock().await;
        let rows = arc.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].row.diags,
            1,
            "label 1's row carries only the ^FO warning it raised ({}), not the stray: {:?}",
            rows[0].messages.first().unwrap_or(&"<none>".into()),
            rows[0].messages
        );
        assert_eq!(rows[0].messages.len(), 1);
        assert!(
            rows[0].messages[0].contains("abc"),
            "label 1's warning names the bad y: {}",
            rows[0].messages[0]
        );
        assert_eq!(
            rows[1].row.diags, 0,
            "the clean label must not inherit label 1's warning"
        );
        assert_eq!(rows[1].messages.len(), 0);
    }
}

#[tokio::test]
async fn labels_across_read_chunks_keep_exact_spans_after_a_drain() {
    let dir = test_dir("drain");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // Two separate writes with a gap: the first is usually read, archived
    // and trimmed (input.drain) before the second read starts, so the
    // second label's ^XA sits at offset 0 of the trimmed buffer - the
    // span machinery must re-base, not carry a stale global offset.
    sock.write_all(b"^XA^GB50,50,2,B^XZ").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    sock.write_all(b"^XA^GB20,20,2,B^XZ").await.unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 2).await;
    let labels = dir.join("labels");
    assert_eq!(
        std::fs::read(labels.join("1.zpl")).unwrap(),
        b"^XA^GB50,50,2,B^XZ"
    );
    assert_eq!(
        std::fs::read(labels.join("2.zpl")).unwrap(),
        b"^XA^GB20,20,2,B^XZ",
        "second label must carry its own exact raw bytes"
    );
}

#[tokio::test]
async fn label_split_across_reads_excludes_preamble_from_raw() {
    let dir = test_dir("split");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // Read 1: junk preamble (must NOT end up in the raw .zpl) then a
    // format that is left OPEN (no ^XZ). Read 2 closes it.
    let preamble = b"\x00\x01\x02junk-before-the-label";
    let label = b"^XA^GB50,50,2,B";
    sock.write_all(&[&preamble[..], &label[..]].concat())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await; // separate reads
    sock.write_all(b"^XZ").await.unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 1).await;
    let raw = std::fs::read(dir.join("labels/1.zpl")).unwrap();
    assert_eq!(
        raw, b"^XA^GB50,50,2,B^XZ",
        "raw .zpl must be the exact label span \u{2014} preamble excluded"
    );
    // LabelRow.bytes == raw.len() (the archived file's length).
    assert_eq!(raw.len(), b"^XA^GB50,50,2,B^XZ".len());
    let ir: Label =
        serde_json::from_slice(&std::fs::read(dir.join("labels/1.json")).unwrap()).unwrap();
    assert_eq!(ir.fields.len(), 1, "the ^GB parsed into one field");
}

#[tokio::test]
async fn garbage_and_unknown_commands_do_not_kill_the_connection() {
    let dir = test_dir("garbage");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // Binary junk plus an unknown command: parser must diagnose, not panic,
    // and the socket must stay open for the label that follows.
    sock.write_all(b"\x00\x01\xfe\xff^QX nonsense\r\n")
        .await
        .unwrap();
    sock.write_all(b"^XA^GB50,50,2,B^XZ").await.unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 1).await;
    assert!(
        state.counters.warnings.load(Ordering::Relaxed) >= 1,
        "the ^QX unknown command must be counted as a warning"
    );
    assert!(dir.join("labels/1.png").exists());
}

#[tokio::test]
async fn eof_with_partial_format_is_diagnosed_not_archived() {
    let dir = test_dir("partial");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^FO10,10^GB50,50,2,B").await.unwrap(); // no ^XZ
    sock.shutdown().await.unwrap();

    wait_warnings(&state, 1).await; // the EOF partial-format diagnostic
    tokio::time::sleep(Duration::from_millis(50)).await; // let a wrong push land
    let arc = state.archive.lock().await;
    assert_eq!(arc.len(), 0, "a half-finished format must not be archived");
}

#[tokio::test]
async fn hs_query_answered_inline_with_placeholder() {
    let dir = test_dir("hs");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HS").await.unwrap();
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
        .await
        .unwrap()
        .expect("~HS must produce a response on the same socket");
    assert!(n > 0, "placeholder response must be non-empty");
    assert_eq!(buf[0], 0x02, "real ~HS responses frame with STX");
    drop(sock); // EOF; the session must end cleanly
    let _ = state;
}

#[tokio::test]
async fn binary_payload_split_across_reads_decodes_fully() {
    let dir = test_dir("gf-split");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // A ^GFB,2000 payload split across two reads (the session reads ~8
    // KiB; large graphics are the common case): the header lands in read
    // 1, the tail in read 2. The tail carries ^/~ bait to prove the
    // payload stays raw - a scanner that tokenized it would archive
    // phantom labels. The full decode must be warning-free and the raw
    // .zpl byte-exact.
    let mut payload = vec![0x41u8; 500]; // plain start (params absorb)
    payload.extend_from_slice(&[0x5E, 0x7E, 0x01]); // "^~\x01"
    payload.extend_from_slice(b"^XA^XZ"); // phantom-format bait
    payload.extend_from_slice(&[0x42; 240]);
    payload.extend_from_slice(&[0x5E, b'F', b'S']); // bogus ^FS inside
    payload.extend_from_slice(&[0x43; 250]);
    payload.extend_from_slice(b"~DY^FS");
    payload.resize(2000, 0x44);
    let mut label = b"^XA^FO0,0^GFB,2000,2000,8,".to_vec();
    label.extend_from_slice(&payload);
    label.extend_from_slice(b"^FS^XZ");

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(&label[..1000]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await; // separate reads
    sock.write_all(&label[1000..]).await.unwrap();
    sock.shutdown().await.unwrap(); // EOF ends the session

    wait_labels(&state, 1).await;
    {
        let arc = state.archive.lock().await;
        assert_eq!(
            arc.len(),
            1,
            "exactly one label \u{2014} no phantom from ^ bait"
        );
        let rows = arc.rows();
        assert_eq!(
            rows[0].row.diags, 0,
            "split payload must be warning-free: {:?}",
            rows[0].messages
        );
        assert_eq!(rows[0].messages.len(), 0);
    }
    // A wrong warning (truncation, phantom command, EOF partial) would
    // bump the counter; settle a moment for a stray late warning to land.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        state.counters.warnings.load(Ordering::Relaxed),
        0,
        "the whole split must decode with ZERO warnings"
    );

    let raw = std::fs::read(dir.join("labels/1.zpl")).unwrap();
    assert_eq!(
        raw, label,
        "raw .zpl must be the exact sent bytes incl. the payload tail"
    );
    let ir: Label =
        serde_json::from_slice(&std::fs::read(dir.join("labels/1.json")).unwrap()).unwrap();
    assert_eq!(ir.fields.len(), 1, "one graphic field, no phantom fields");
    match &ir.fields[0] {
        zpl::ir::Field::Graphic(g) => {
            assert_eq!(g.data, payload, "decoded payload must be byte-exact")
        }
        other => panic!("expected graphic field, got {other:?}"),
    }
    let png = std::fs::read(dir.join("labels/1.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    // 2000 bytes at 8 bytes/row = 250 rows; PNG drains the 832-dot
    // default-width canvas at auto height = content bottom (250).
    let (w, h) = (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    );
    assert_eq!((w, h), (832, 250), "PNG dimensions");
}

#[tokio::test]
async fn eof_mid_binary_payload_warns_once_and_stores_nothing() {
    let dir = test_dir("gf-eof");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // True truncation: the connection ends mid-payload. Exactly one
    // warning - the truncation diagnostic from the session's EOF branch
    // (the parse loop itself must stay silent at chunk ends, or a payload
    // that merely spans reads would warn). Outside ^XA..^XZ so the
    // unfinished-format EOF diagnostic cannot double-fire.
    let mut sock = TcpStream::connect(addr).await.unwrap();
    let mut head = b"^GFB,2000,2000,8,".to_vec();
    head.extend_from_slice(&[0x41; 500]);
    sock.write_all(&head).await.unwrap();
    sock.shutdown().await.unwrap();

    wait_warnings(&state, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await; // let a wrong push land
    let arc = state.archive.lock().await;
    assert_eq!(arc.len(), 0, "a truncated download must not be archived");
    let _ = dir;
}
