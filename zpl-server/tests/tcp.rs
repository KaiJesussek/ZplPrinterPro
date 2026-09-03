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
