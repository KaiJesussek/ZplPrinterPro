//! HTTP API tests (Task 21): tower oneshot against the in-process router,
//! cross-stack proofs (PUT /api/printer-state <-> TCP ~HS), persistence
//! reload, archive retention, and the SSE event stream.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_stream::StreamExt;
use tower::ServiceExt;

use zpl::memory::Image;
use zpl_server::archive::{Archive, LabelRow, DEFAULT_MAX_BYTES};
use zpl_server::http::{persist_now, router};
use zpl_server::state::{Conditions, Event};
use zpl_server::tcp::{spawn_tcp, AppState};

fn test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("zpl-http-{tag}-{nanos}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One TCP session that archives `payload` (single ^XA..^XZ span).
async fn send_label(addr: SocketAddr, payload: &[u8]) {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(payload).await.unwrap();
    sock.shutdown().await.unwrap(); // EOF ends the session
}

async fn wait_labels(state: &AppState, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state
        .counters
        .labels
        .load(std::sync::atomic::Ordering::Relaxed)
        < n
    {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} archived label(s)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Start TCP + HTTP in-process on ephemeral ports, sharing one AppState.
async fn spawn(dir: PathBuf) -> (SocketAddr, Arc<AppState>) {
    let state = Arc::new(AppState::new(Archive::open(dir).unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    spawn_tcp(listener, state.clone());
    (addr, state)
}

/// Drive one request through the router; returns the raw response.
async fn request(state: &Arc<AppState>, req: Request<Body>) -> axum::response::Response {
    router(state.clone())
        .oneshot(req)
        .await
        .expect("router oneshot must not error")
}

async fn get(state: &Arc<AppState>, path: &str) -> axum::response::Response {
    request(
        state,
        Request::builder().uri(path).body(Body::empty()).unwrap(),
    )
    .await
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

async fn post(state: &Arc<AppState>, path: &str) -> axum::response::Response {
    request(
        state,
        Request::builder()
            .method("POST")
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post_bytes(state: &Arc<AppState>, path: &str, body: &[u8]) -> axum::response::Response {
    request(
        state,
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(body.to_vec()))
            .unwrap(),
    )
    .await
}

/// PNG pixel dims from the IHDR (bytes 16..24, both big-endian).
fn png_dims(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    (w, h)
}

async fn body_string(resp: axum::response::Response) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("reading the body must not fail");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn render_direct_returns_png_without_archiving() {
    let dir = test_dir("render");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // A valid format renders inline as a PNG.
    let resp = post_bytes(&state, "/api/render", b"^XA^FO10,10^A0N,20,20^FDhi^FS^XZ").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png",
        "inline PNG, not JSON"
    );
    let png = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("reading the body must not fail");
    let (w, h) = png_dims(&png);
    assert!(w > 0 && h > 0, "sane label dims {w}x{h}");

    // The playground must not archive: the label list stays empty.
    let (status, body) = body_string(get(&state, "/api/labels").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "[]", "direct render has no storage side effect");

    // Garbage / no complete format -> 400 with the parse diagnostics.
    let resp = post_bytes(&state, "/api/render", b"no format here").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let (_, body) = body_string(resp).await;
    assert!(body.contains("^XA..^XZ"), "error explains the format requirement: {body}");

    // A malformed format warns but still renders (parse -> fallback).
    let resp = post_bytes(&state, "/api/render", b"^XA^BY0^FS^XZ").await;
    assert_eq!(resp.status(), StatusCode::OK, "warn-only inputs still render");

    let _ = addr; // spawn binds an ephemeral port; the handle keeps it alive
}

#[tokio::test]
async fn labels_list_empty_then_shows_archived_rows() {
    let dir = test_dir("list");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Empty list.
    let (status, body) = body_string(get(&state, "/api/labels").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "[]", "fresh archive lists no labels");

    // Push a label with an explicit ^LL (auto-height labels report
    // height_dots 0 - the IR truth), then the list must show it.
    send_label(addr, b"^XA^LL50^GB50,50,2,B^XZ").await;
    wait_labels(&state, 1).await;

    let (status, body) = body_string(get(&state, "/api/labels").await).await;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 1, "one archived label");
    let row = &rows[0];
    assert_eq!(row["id"], 1);
    assert_eq!(row["width"], 832, "default ^PW at 203 dpi");
    assert_eq!(row["height"], 50);
    assert!(row["ts_ms"].as_u64().unwrap() > 0);
    assert!(row["bytes"].as_u64().unwrap() > 0);
    assert_eq!(row["diags"], 0);
}

#[tokio::test]
async fn label_detail_raw_and_png_endpoints() {
    let dir = test_dir("detail");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;
    let payload = b"^XA^FO10,10^FDHello^FS^XZ";
    send_label(addr, payload).await;
    wait_labels(&state, 1).await;

    // Detail: meta + IR + diag list.
    let (status, body) = body_string(get(&state, "/api/labels/1").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["meta"]["id"], 1);
    assert_eq!(v["meta"]["diags"], 0);
    assert_eq!(
        v["ir"]["fields"].as_array().unwrap().len(),
        1,
        "the ^FD text parsed into one field"
    );
    assert_eq!(
        v["ir"]["width_dots"], 832,
        "IR is served with the label's geometry"
    );
    assert_eq!(
        v["diags"],
        serde_json::json!([]),
        "no diagnostics for a clean label"
    );

    // Raw bytes.
    let (status, body) = body_string(get(&state, "/api/labels/1.zpl").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_bytes(),
        payload,
        "raw .zpl is the exact archived bytes"
    );

    // PNG.
    let resp = get(&state, "/api/labels/1.png").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
    let png = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let (w, h) = png_dims(&png);
    assert!(w > 0 && h > 0);
}

#[tokio::test]
async fn unknown_ids_are_404() {
    let dir = test_dir("404");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    for path in [
        "/api/labels/999",
        "/api/labels/999.zpl",
        "/api/labels/999.png",
        "/api/labels/nonsense",
        "/api/labels/nonsense.png",
    ] {
        let (status, _) = body_string(get(&state, path).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} must 404");
    }
    let (status, _) = body_string(post(&state, "/api/labels/999/rerender?dpi=300").await).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "rerender of an unknown id must 404"
    );
}

#[tokio::test]
async fn printer_state_put_get_and_cross_stack_tcp_hs() {
    let dir = test_dir("state");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // GET defaults.
    let (status, body) = body_string(get(&state, "/api/printer-state").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paper_out"], false);
    assert_eq!(v["head_temp"], "Normal");

    // PUT partial patch.
    let (status, body) =
        body_string(put_json(&state, "/api/printer-state", r#"{"paper_out":true}"#).await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paper_out"], true, "PUT echoes the merged conditions");

    // GET reflects.
    let (status, body) = body_string(get(&state, "/api/printer-state").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["paper_out"], true);

    // Cross-stack: the TCP ~HS responder reads the SAME state - paper out
    // is one of the guide's five no-response conditions, so ~HS must be
    // silent (the read times out with zero bytes).
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HS").await.unwrap();
    let mut buf = [0u8; 256];
    let n = match tokio::time::timeout(Duration::from_millis(300), sock.read(&mut buf)).await {
        Ok(Ok(n)) => n, // the server answered - the bug
        Ok(Err(e)) => panic!("~HS read error: {e}"),
        Err(_elapsed) => 0, // desired: the read hangs, nothing arrives
    };
    if n > 0 {
        panic!(
            "~HS answered with {n} bytes while paper_out=true: {}",
            String::from_utf8_lossy(&buf[..n])
        );
    }
    drop(sock);

    // Recovery: paper in -> ~HS answers again with STX frames.
    let resp = put_json(&state, "/api/printer-state", r#"{"paper_out":false}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"~HS").await.unwrap();
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
        .await
        .unwrap()
        .expect("~HS must respond again once paper is loaded");
    assert!(n > 0);
    assert_eq!(buf[0], 0x02, "~HS frames with STX");
    drop(sock);

    // PUT validation: bad enum -> 400; malformed JSON -> 400.
    let resp = put_json(&state, "/api/printer-state", r#"{"head_temp":"Frozen"}"#).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "unknown enum value must 400"
    );
    let resp = put_json(&state, "/api/printer-state", "{not json").await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "malformed JSON must 400"
    );
}

#[tokio::test]
async fn rerender_at_300_dpi_rewrites_png_larger() {
    let dir = test_dir("rerender");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Default-font text scales with dpi (`eff = dpi/203`), so a 300-dpi
    // re-render must produce a taller bitmap than the 203-dpi archive.
    // `^CFA,0` keeps the standard font matrix (font_height 0), which is
    // the path where the renderer scales text by dpi/203 - so the 300-dpi
    // re-render measurably grows.
    send_label(addr, b"^XA^CFA,0^FO10,10^FDHello^FS^XZ").await;
    wait_labels(&state, 1).await;
    let before = std::fs::read(dir.join("labels/1.png")).unwrap();
    let (w203, h203) = png_dims(&before);

    let (status, body) = body_string(post(&state, "/api/labels/1/rerender?dpi=300").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["dpi"], 300);
    let (w300, h300) = (
        v["width"].as_u64().unwrap() as u32,
        v["height"].as_u64().unwrap() as u32,
    );

    let after = std::fs::read(dir.join("labels/1.png")).unwrap();
    let (w_after, h_after) = png_dims(&after);
    assert_eq!(
        (w_after, h_after),
        (w300, h300),
        "response dims match the cached file"
    );
    assert!(
        h300 > h203,
        "300-dpi text render must be taller: {h203} -> {h300}"
    );
    assert_ne!(
        (w300, h300),
        (w203, h203),
        "the PNG file must have been rewritten"
    );
    assert_ne!(before, after, "the PNG bytes change");

    // Bad dpi params -> 400.
    let (status, _) = body_string(post(&state, "/api/labels/1/rerender?dpi=0").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = body_string(post(&state, "/api/labels/1/rerender?dpi=abc").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = body_string(post(&state, "/api/labels/1/rerender").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing dpi must 400");
}

#[tokio::test]
async fn storage_and_diagnostics_endpoints_report_state() {
    let dir = test_dir("storage");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // An unknown command inside a label's span reaches the diagnostics.
    send_label(addr, b"^XA^QX oops^GB50,50,2,B^XZ").await;
    wait_labels(&state, 1).await;

    // Deposit a stored image directly (same channel ~DG uses).
    state.memory.lock().await.images.insert(
        "LOGO.GRF".into(),
        Image {
            width: 8,
            height: 8,
            data: vec![0u8; 8],
        },
    );

    let (status, body) = body_string(get(&state, "/api/storage").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["counts"]["images"], 1);
    assert_eq!(v["images"]["LOGO.GRF"]["width"], 8);
    assert_eq!(v["images"]["LOGO.GRF"]["bytes"], 8);
    assert_eq!(v["counts"]["fonts"], 0);
    assert_eq!(v["counts"]["formats"], 0);

    let (status, body) = body_string(get(&state, "/api/diagnostics").await).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["labels"].as_u64().unwrap() >= 1);
    assert!(v["bytes"].as_u64().unwrap() > 0, "bytes counted since boot");
    assert!(
        v["warnings"].as_u64().unwrap() >= 1,
        "the ^QX warning is counted"
    );
    assert!(
        v["unknown_commands"].as_u64().unwrap() >= 1,
        "the ^QX unknown command is counted separately"
    );
}

#[tokio::test]
async fn persistence_reloads_memory_and_conditions() {
    let dir = test_dir("persist");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Mutate memory and conditions, then flush through the real seam.
    state.memory.lock().await.images.insert(
        "LOGO.GRF".into(),
        Image {
            width: 16,
            height: 8,
            data: vec![0xff; 16],
        },
    );
    state
        .memory
        .lock()
        .await
        .fonts
        .insert("F.TTF".into(), vec![1, 2, 3]);
    let resp = put_json(&state, "/api/printer-state", r#"{"paper_out":true}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    persist_now(&state).await;

    // The TCP session is still a live holder of the same state; drop it so
    // nothing pins the old Memory.
    let _ = addr;

    // A fresh archive + AppState over the same dir must restore both.
    let state2 = Arc::new(AppState::new(Archive::open(dir.clone()).unwrap()));
    let mem = state2.memory.lock().await;
    assert!(
        mem.images.contains_key("LOGO.GRF"),
        "stored image survives a restart"
    );
    assert_eq!(mem.images["LOGO.GRF"].width, 16);
    assert_eq!(mem.fonts["F.TTF"], vec![1, 2, 3]);
    drop(mem);
    let ps = state2.printer_state.lock().await;
    assert!(ps.conditions.paper_out, "conditions survive a restart");
    drop(ps);

    // Defaults when the files are absent.
    let fresh = test_dir("persist-fresh");
    let _keep2 = Cleanup(fresh.clone());
    let state3 = Arc::new(AppState::new(Archive::open(fresh).unwrap()));
    let mem = state3.memory.lock().await;
    assert!(mem.images.is_empty());
    drop(mem);
    assert!(!state3.printer_state.lock().await.conditions.paper_out);
}

#[tokio::test]
async fn retention_prunes_oldest_labels() {
    let dir = test_dir("retain");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn_limited(dir.clone(), 2, DEFAULT_MAX_BYTES).await;

    for _ in 0..4 {
        send_label(addr, b"^XA^GB50,50,2,B^XZ").await;
        tokio::time::sleep(Duration::from_millis(30)).await; // separate sessions
    }
    wait_labels(&state, 4).await;

    // KEEP=2: rows are the two newest; the two oldest file sets are gone.
    {
        let arc = state.archive.lock().await;
        let rows: Vec<&LabelRow> = arc.rows().iter().map(|e| &e.row).collect();
        assert_eq!(rows.len(), 2, "only the newest two rows survive");
        assert_eq!((rows[0].id, rows[1].id), (3, 4));
        assert_eq!(arc.pruned(), 2, "two prunes logged in diagnostics");
    }
    let labels = dir.join("labels");
    for id in [1u64, 2] {
        for ext in ["zpl", "json", "png"] {
            assert!(
                !labels.join(format!("{id}.{ext}")).exists(),
                "label {id}.{ext} must be pruned"
            );
        }
    }
    assert!(labels.join("3.zpl").exists());
    assert!(labels.join("4.zpl").exists());

    // The list endpoint serves only the retained rows, newest first.
    let (_, body) = body_string(get(&state, "/api/labels").await).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let ids: Vec<u64> = rows.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![4, 3], "newest first");

    // limit caps the list; since filters by ts_ms floor.
    let (_, body) = body_string(get(&state, "/api/labels?limit=1").await).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], 4, "limit keeps the newest");
    let (status, _) = body_string(get(&state, "/api/labels?limit=abc").await).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-numeric limit must 400"
    );
    let (_, body) = body_string(get(&state, "/api/labels?since=99999999999999").await).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert!(
        rows.is_empty(),
        "since above every ts_ms filters everything out"
    );

    // Size cap: with a tiny max_bytes, rows fall below the keep threshold.
    let dir2 = test_dir("retain-size");
    let _keep2 = Cleanup(dir2.clone());
    let (addr2, state2) = spawn_limited(dir2.clone(), 1000, 40).await;
    for _ in 0..3 {
        send_label(addr2, b"^XA^GB50,50,2,B^XZ").await;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    wait_labels(&state2, 3).await;
    let arc = state2.archive.lock().await;
    let rows = arc.rows();
    assert!(
        rows.len() < 3,
        "size cap prunes to fit (~20 B per label, cap 40 B)"
    );
    assert!(
        rows.iter().all(|e| e.row.id > 1),
        "oldest labels pruned first"
    );
}

#[tokio::test]
async fn sse_streams_label_and_state_events() {
    let dir = test_dir("sse");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let resp = get(&state, "/api/events").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE],
        "text/event-stream",
        "SSE content type"
    );
    let mut stream = Box::pin(resp.into_body().into_data_stream());

    // State event (what PUT emits).
    state
        .events
        .send(Event::State(Conditions {
            paper_out: true,
            ..Conditions::default()
        }))
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("a state event must reach the stream")
        .expect("stream yields data frames")
        .expect("no transport error");
    let text = String::from_utf8_lossy(&frame).into_owned();
    assert!(text.contains("event: state"), "frame: {text}");
    assert!(text.contains("\"paper_out\":true"), "frame: {text}");

    // Label event (what the TCP session emits on archive).
    let row = LabelRow {
        id: 7,
        ts_ms: 1234,
        bytes: 10,
        diags: 0,
        width: 100,
        height: 50,
    };
    state.events.send(Event::Label(row)).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("a label event must reach the stream")
        .expect("stream yields data frames")
        .expect("no transport error");
    let text = String::from_utf8_lossy(&frame).into_owned();
    assert!(text.contains("event: label"), "frame: {text}");
    assert!(text.contains("\"id\":7"), "frame: {text}");
}

#[tokio::test]
async fn sse_stream_ends_when_a_subscriber_lags() {
    let dir = test_dir("sse-lag");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let resp = get(&state, "/api/events").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = Box::pin(resp.into_body().into_data_stream());

    // Overrun the 128-event ring buffer before the stream reads anything:
    // the subscriber falls behind, and the handler must END the stream so
    // EventSource reconnects (previously the lagged read was skipped and
    // the stale, missynchronized stream stayed open forever).
    let row = LabelRow {
        id: 7,
        ts_ms: 1234,
        bytes: 10,
        diags: 0,
        width: 100,
        height: 50,
    };
    for _ in 0..130 {
        state.events.send(Event::Label(row.clone())).unwrap();
    }
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
    assert!(
        matches!(frame, Ok(None)),
        "a lagged subscriber must see the stream end (EventSource reconnect), got {frame:?}"
    );
}

/// spawn() with a custom retention override (small KEEP / MAX bytes).
async fn spawn_limited(dir: PathBuf, keep: usize, max_bytes: u64) -> (SocketAddr, Arc<AppState>) {
    let state = Arc::new(AppState::new(
        Archive::open_with(dir, keep, max_bytes).unwrap(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    spawn_tcp(listener, state.clone());
    (addr, state)
}
