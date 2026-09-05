//! Protocol integration tests (Task 23): the plan's eight scripted
//! conversations, run as REAL TCP conversations against the T19
//! ephemeral-listener seam (no mock sockets). One connection per
//! conversation, byte-sequenced writes, responses and archived artifacts
//! asserted - plus the controller's empty-format canaries (`^XA^XZ`,
//! `^XA^FWR^XZ` -> valid archive rows, zero diagnostics, blank renders,
//! quiet ~HS).
//!
//! Conversations (brief order):
//!   1. fire-and-forget label -> archive row + PNG;
//!   2. ~HS after a label -> byte-exact 3-line response with `www` counting
//!      a prior ~DG download, then decrementing after a `^ID*` prune (the
//!      user's before/after-pruning case);
//!   3. suppression: `PUT /api/printer-state` paper_out -> ~HS gets NO
//!      bytes (read timeout), recovery on paper in;
//!   4. `^HV1` readback of `^FN1` data in the same connection;
//!   5. `^PQ3` -> `labels_remaining` = 2 in the next ~HS;
//!   6. open `^XA` without `^XZ` -> `h` (partial-format) flag = 1;
//!   7. `^ZZ` / unknown commands -> connection stays alive, the following
//!      label still prints and the session still answers queries;
//!   8. `^CI13` (CP850, the T12-corrected pick) + `^FD` byte 0x94 -> the
//!      archived IR's `data` holds the decoded character `o`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use zpl::ir::{Field, Label};
use zpl_server::archive::Archive;
use zpl_server::http::router;
use zpl_server::tcp::{spawn_tcp, AppState};

/// A fresh throwaway archive dir per test (removed on drop).
fn test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("zpl-proto-{tag}-{nanos}"));
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

/// Read until `n` complete ~HS responses have arrived (or EOF). One ~HS
/// response is the ~HS contract's three STX...ETX-CRLF strings, so the loop
/// counts ETX (0x03) terminators: a caller asking for one ~HS gets all
/// three frames, not just the first STX frame that happens to land first.
async fn read_until_frames(sock: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    let deadline = Instant::now() + Duration::from_secs(3);
    while buf.iter().filter(|&&b| b == 0x03).count() < n * 3 {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} ~HS response(s); got: {:?}",
            String::from_utf8_lossy(&buf)
        );
        match tokio::time::timeout(Duration::from_millis(100), sock.read(&mut chunk)).await {
            Ok(Ok(0)) => break, // EOF: caller-side frame assertions catch a short read
            Ok(Ok(k)) => buf.extend_from_slice(&chunk[..k]),
            _ => continue,
        }
    }
    buf
}

/// Read whatever arrives within `quiet`, asserting NOTHING is heard AND
/// the connection stays open. Silence on a suppressed response must not
/// be confounded with a read error or an early close: EOF here means the
/// server killed the socket, which a suppression must never do (the same
/// session answers once the condition clears).
async fn assert_no_burst(sock: &mut TcpStream, quiet: Duration, what: &str) {
    let mut chunk = [0u8; 256];
    match tokio::time::timeout(quiet, sock.read(&mut chunk)).await {
        Ok(Ok(0)) => panic!("{what}: connection closed; expected a live, silent socket"),
        Ok(Ok(k)) => panic!("{what} must produce no bytes, got {k}: {:?}", &chunk[..k]),
        Ok(Err(e)) => panic!("{what}: read error {e}; expected a quiet timeout"),
        Err(_) => {} // quiet window elapsed, nothing heard: the assertion target
    }
}

/// Read until the peer closes, asserting EOF arrives (with a generous
/// bound). The server drops the socket only after its post-loop work
/// (e.g. the partial-format diagnostic) has run, so a returned EOF here is
/// the deterministic close notification - no short-sleep race.
async fn assert_closed(sock: &mut TcpStream, what: &str) {
    let mut probe = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut probe))
        .await
        .unwrap_or_else(|_| panic!("{what}: peer never closed the connection"))
        .unwrap_or_else(|e| panic!("{what}: read error {e}; expected EOF"));
    assert_eq!(n, 0, "{what}: peer must close the connection");
}

fn frames(resp: &[u8]) -> usize {
    resp.iter().filter(|&&b| b == 0x02).count()
}

/// String 1's comma fields of the first frame in `resp`.
fn s1_fields(resp: &[u8]) -> Vec<String> {
    let l = resp.split(|&b| b == b'\n').next().unwrap();
    l[1..l.len() - 2]
        .split(|&b| b == b',')
        .map(|f| String::from_utf8(f.to_vec()).unwrap())
        .collect()
}

/// String 2's comma fields of the first frame in `resp`.
fn s2_fields(resp: &[u8]) -> Vec<String> {
    let l = resp.split(|&b| b == b'\n').nth(1).unwrap();
    l[1..l.len() - 2]
        .split(|&b| b == b',')
        .map(|f| String::from_utf8(f.to_vec()).unwrap())
        .collect()
}

/// Drive one request through the in-process router.
async fn request(state: &Arc<AppState>, req: Request<Body>) -> axum::response::Response {
    router(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot must not error")
}

async fn put_json(state: &Arc<AppState>, path: &str, body: &str) -> axum::response::Response {
    request(
        state,
        Request::builder()
            .method("PUT")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap(),
    )
    .await
}

async fn body_string(resp: axum::response::Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body must be readable");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Decode a PNG; returns (width, height, all-pixels-white). The renderer
/// encodes 1-bit grayscale (0 black / 1 white).
fn png_blank_checked(png: &[u8]) -> (u32, u32, bool) {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().expect("archived PNG must decode");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("PNG frame must decode");
    (info.width, info.height, buf.iter().all(|&b| b == 0xFF))
}

// ---------------------------------------------------------------------------
// Conversation 1 - fire-and-forget label -> archive row + PNG

#[tokio::test]
async fn fire_and_forget_label_archives_row_and_png() {
    let dir = test_dir("ff");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let payload = b"^XA^GB50,50,2,B^XZ";
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(payload).await.unwrap();
    sock.shutdown().await.unwrap(); // fire-and-forget: EOF ends the session

    wait_labels(&state, 1).await;
    let labels = dir.join("labels");
    assert_eq!(
        std::fs::read(labels.join("1.zpl")).unwrap(),
        payload,
        "raw .zpl is the exact sent bytes"
    );
    let ir: Label = serde_json::from_slice(&std::fs::read(labels.join("1.json")).unwrap()).unwrap();
    assert_eq!(ir.width_dots, 832, "default ^PW at 203 dpi");
    assert_eq!(ir.fields.len(), 1, "the ^GB parsed into one field");
    let png = std::fs::read(labels.join("1.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    let (w, h, _) = png_blank_checked(&png);
    assert_eq!((w, h), (832, 50), "render dimensions follow the geometry");

    let arc = state.archive.lock().await;
    assert_eq!(arc.len(), 1, "exactly one archive row");
    assert_eq!(arc.rows()[0].row.bytes, payload.len());
    assert_eq!(
        arc.rows()[0].row.diags,
        0,
        "no diagnostics on a clean label"
    );
}

// ---------------------------------------------------------------------------
// Conversation 2 - ~HS with www counting a ~DG download, then decrementing
// after a ^ID* prune (the user's before/after-pruning case, byte-exact).

/// Healthiest possible state after one 50-dot box label and one stored
/// image: dddd=0050 (measured label height), www=001 (the ~DG download).
const HS_AFTER_DG: &[u8] = b"\x02158,0,0,0050,000,0,0,0,000,0,0,0\x03\r\n\
\x02000,0,0,0,0,2,0,0,00000000,1,001\x03\r\n\
\x020000,1\x03\r\n";

/// Same, after the ^ID* prune: the prune label is empty (auto height 1 ->
/// dddd=0001) and www has decremented to 000.
const HS_AFTER_PRUNE: &[u8] = b"\x02158,0,0,0001,000,0,0,0,000,0,0,0\x03\r\n\
\x02000,0,0,0,0,2,0,0,00000000,1,000\x03\r\n\
\x020000,1\x03\r\n";

#[tokio::test]
async fn hs_www_counts_stored_images_and_decrements_after_prune() {
    let dir = test_dir("www");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();

    // Script part 1: download an 8x8 image, print a label, ask ~HS. The
    // response must be exactly the three STX...ETX-CRLF strings with www
    // counting the download (before the prune).
    sock.write_all(b"~DGR:LOGO.GRF,8,1,FF00000000000000^XA^GB50,50,2,B^XZ~HS")
        .await
        .unwrap();
    let resp1 = read_until_frames(&mut sock, 1).await;
    assert_eq!(resp1, HS_AFTER_DG, "~HS before pruning, byte-exact");

    // Script part 2: prune stored objects (wildcard ^ID*), ask again - the
    // same connection, the same session: www decrements.
    sock.write_all(b"^XA^IDR:*.GRF^XZ~HS").await.unwrap();
    let resp2 = read_until_frames(&mut sock, 1).await;
    assert_eq!(resp2, HS_AFTER_PRUNE, "~HS after pruning, byte-exact");
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 3 - suppression: PUT paper_out -> ~HS produces NO bytes.

#[tokio::test]
async fn hs_suppressed_while_media_out_via_put() {
    let dir = test_dir("sup");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Media out, set through the Task 21 API (the same PrinterState the
    // TCP responder reads).
    let (status, body) =
        body_string(put_json(&state, "/api/printer-state", r#"{"paper_out":true}"#).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"paper_out\":true"), "PUT echoes the merge");

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HS").await.unwrap();
    assert_no_burst(&mut sock, Duration::from_millis(400), "~HS while paper_out").await;

    // Media loaded again -> the same connection answers normally.
    let resp = put_json(&state, "/api/printer-state", r#"{"paper_out":false}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    sock.write_all(b"~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    assert_eq!(frames(&resp), 3, "~HS answers again once paper is loaded");
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 4 - ^HV1 readback of ^FN1 data.

#[tokio::test]
async fn hv1_reads_back_fn1_data_in_same_connection() {
    let dir = test_dir("hv");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^FN1^FDHello^FS^XZ^HV1").await.unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .unwrap()
        .expect("^HV must produce a response");
    assert_eq!(&buf[..n], b"Hello", "^HV1 returns the ^FN1 field data");
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 5 - ^PQ3 -> labels_remaining = 2 in the next ~HS.

#[tokio::test]
async fn pq3_leaves_two_labels_remaining_in_status() {
    let dir = test_dir("pq");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // A ^PQ3 batch: 3 copies received at ^XZ; the completion consumes the
    // first printed copy, so 2 labels remain when ~HS is answered.
    sock.write_all(b"^XA^GB50,50,2,B^FS^PQ3^XZ~HS")
        .await
        .unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    assert_eq!(frames(&resp), 3, "one ~HS response");
    let f = s2_fields(&resp);
    assert_eq!(f[8], "00000002", "uuuuuuuu = labels remaining in batch");
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 6 - open ^XA without ^XZ -> h (partial-format) flag = 1.

#[tokio::test]
async fn open_format_without_xz_flags_partial_in_status() {
    let dir = test_dir("partial");
    let _keep = Cleanup(dir.clone());
    let (addr, _state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA").await.unwrap(); // format opened, never closed yet
    tokio::time::sleep(Duration::from_millis(200)).await; // separate reads
    sock.write_all(b"~HS").await.unwrap();
    let resp = read_until_frames(&mut sock, 1).await;
    let f = s1_fields(&resp);
    assert_eq!(f[7], "1", "h = partial-format flag");
    assert_eq!(f[6], "0", "g = comm diagnostics unaffected");
    sock.write_all(b"^XZ").await.unwrap(); // close the format cleanly
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 7 - ^ZZ / unknown commands keep the connection alive.

#[tokio::test]
async fn unknown_commands_keep_connection_alive() {
    let dir = test_dir("zz");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    // ^ZZ (printer sleep; a real T17 command - must not affect the
    // session) and the genuinely-unknown ^QX, then a label and a query in
    // the SAME conversation: the socket must survive the junk and print.
    sock.write_all(b"^ZZ^QX^XA^GB50,50,2,B^XZ~HS")
        .await
        .unwrap();

    wait_labels(&state, 1).await;
    assert!(
        state.counters.unknown_commands.load(Ordering::Relaxed) >= 1,
        "the unknown ^QX must be counted as an unknown command"
    );
    assert!(dir.join("labels/1.png").exists());

    let resp = read_until_frames(&mut sock, 1).await;
    assert_eq!(frames(&resp), 3, "the connection still answers ~HS");
    drop(sock);
}

// ---------------------------------------------------------------------------
// Conversation 8 - codepage end-to-end: ^CI13 (CP850) byte 0x94 -> o in the
// archived IR's `data`.
//
// Codepage pick (T12 note): the brief allows ^CI13/CP850 0x94->o or the
// default 1252 0xE9->e; ^CI13 is the stronger proof because it exercises
// the ^CI selection path end to end (not just the power-up default) - the
// same byte mis-decoded under 1252 yields U+201C (") and the test fails.

#[tokio::test]
async fn ci13_decodes_cp850_byte_into_archived_ir() {
    let dir = test_dir("cp");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let payload = b"^XA^CI13^FD\x94^FS^XZ"; // 0x94 = o in Zebra CP 850
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(payload).await.unwrap();
    sock.shutdown().await.unwrap();

    wait_labels(&state, 1).await;
    let labels = dir.join("labels");
    assert_eq!(
        std::fs::read(labels.join("1.zpl")).unwrap(),
        payload,
        "raw bytes archived verbatim (the 0x94 survives)"
    );
    let ir: Label = serde_json::from_slice(&std::fs::read(labels.join("1.json")).unwrap()).unwrap();
    match &ir.fields[0] {
        Field::Text(t) => assert_eq!(t.data, "\u{00F6}", "^CI13 decodes 0x94 through CP850"),
        other => panic!("expected a text field, got {other:?}"),
    }
    let png = std::fs::read(labels.join("1.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "label still renders");
}

// ---------------------------------------------------------------------------
// Controller's canary pins - empty formats are valid, quiet, blank rows.

#[tokio::test]
async fn empty_format_canaries_are_valid_rows_blank_renders_quiet_status() {
    let dir = test_dir("empty");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"^XA^XZ^XA^FWR^XZ~HS").await.unwrap();

    wait_labels(&state, 2).await;
    let labels = dir.join("labels");
    let raws = [b"^XA^XZ" as &[u8], b"^XA^FWR^XZ" as &[u8]];
    for (id, expected) in raws.iter().enumerate() {
        let id = (id + 1) as u64;
        assert_eq!(
            std::fs::read(labels.join(format!("{id}.zpl"))).unwrap(),
            *expected,
            "canary {id} archives its exact raw span"
        );
        let png = std::fs::read(labels.join(format!("{id}.png"))).unwrap();
        let (w, h, all_white) = png_blank_checked(&png);
        assert_eq!((w, h), (832, 1), "canary {id}: blank auto-sized label");
        assert!(all_white, "canary {id} must render all-white");
    }
    {
        let arc = state.archive.lock().await;
        for entry in arc.rows() {
            assert_eq!(entry.row.diags, 0, "canary rows carry zero diagnostics");
        }
    }

    // ~HS right after the two empty formats: no spurious flags.
    let resp = read_until_frames(&mut sock, 1).await;
    assert_eq!(frames(&resp), 3);
    let l1 = s1_fields(&resp);
    assert_eq!(l1[7], "0", "h = partial format stays clear");
    let l2 = s2_fields(&resp);
    assert_eq!(l2[8], "00000000", "no batch outstanding");
    assert_eq!(l2[10], "000", "www = no stored images");
    assert_eq!(l1[1], "0", "b = paper out clear");

    // Nothing anywhere on the connection: two labels, zero diagnostics,
    // zero unknown commands - including the EOF after the clean close.
    assert_eq!(state.counters.labels.load(Ordering::Relaxed), 2);
    assert_eq!(state.counters.warnings.load(Ordering::Relaxed), 0);
    assert_eq!(state.counters.unknown_commands.load(Ordering::Relaxed), 0);
    sock.shutdown().await.unwrap(); // clean EOF: labels already closed
                                    // Deterministic close wait: the server pushes its (would-be)
                                    // partial-format diagnostic before dropping the socket, so EOF on our
                                    // read confirms that check already ran - no tight EOF-poll race.
    assert_closed(&mut sock, "session after a clean shutdown").await;
    assert_eq!(
        state.counters.warnings.load(Ordering::Relaxed),
        0,
        "clean EOF must not produce a partial-format diagnostic"
    );
}
