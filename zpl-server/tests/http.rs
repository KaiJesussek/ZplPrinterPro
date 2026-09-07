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

/// `spawn()` with bearer-token auth turned on. Sets `auth_token` directly on
/// the state rather than via `ZPL_AUTH_TOKEN` (the field is `pub`, and the
/// test crate runs in edition 2021 where `std::env::set_var` is `unsafe`
/// - this workspace forbids `unsafe_code`).
async fn spawn_with_token(dir: PathBuf, token: &str) -> (SocketAddr, Arc<AppState>) {
    let mut state = AppState::new(Archive::open(dir).unwrap());
    state.auth_token = Some(token.to_owned());
    let state = Arc::new(state);
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
    assert!(
        body.contains("^XA..^XZ"),
        "error explains the format requirement: {body}"
    );

    // A malformed format warns but still renders (parse -> fallback).
    let resp = post_bytes(&state, "/api/render", b"^XA^BY0^FS^XZ").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "warn-only inputs still render"
    );

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

#[tokio::test]
async fn root_and_static_assets_are_served_embedded() {
    let dir = test_dir("static-ui");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let resp = get(&state, "/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8",
        "root must serve the embedded index.html"
    );
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("<!DOCTYPE html>"));

    let resp = get(&state, "/app.js").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(body.starts_with(b"'use strict';"));

    let resp = get(&state, "/no-such-file.txt").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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

/// Auth-on: every /api/* route rejects requests with no token.
#[tokio::test]
async fn auth_on_rejects_unauthenticated() {
    let dir = test_dir("auth-none");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn_with_token(dir.clone(), "sekret").await;
    let _ = addr;

    let resp = get(&state, "/api/labels").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer",
    );
    let (status, body) = body_string(resp).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body.contains("needs_token"),
        "401 carries the UI's login marker: {body}"
    );
}

/// Auth-on: wrong tokens are rejected in both accepted forms.
#[tokio::test]
async fn auth_on_rejects_wrong_token() {
    let dir = test_dir("auth-wrong");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn_with_token(dir.clone(), "sekret").await;
    let _ = addr;

    // Wrong Bearer header.
    let resp = request(
        &state,
        Request::builder()
            .uri("/api/labels")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    drop(resp);

    // Wrong ?token= query param.
    let (status, body) = body_string(get(&state, "/api/labels?token=wrong").await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("needs_token"), "{body}");
}

/// Auth-on: the correct token is accepted in both accepted forms.
#[tokio::test]
async fn auth_on_accepts_correct_token() {
    let dir = test_dir("auth-ok");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn_with_token(dir.clone(), "sekret").await;
    let _ = addr;

    // Bearer header.
    let resp = request(
        &state,
        Request::builder()
            .uri("/api/labels")
            .header(header::AUTHORIZATION, "Bearer sekret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "correct Bearer token");
    drop(resp);

    // ?token= query param (the EventSource/<img> form).
    let (status, body) = body_string(get(&state, "/api/labels?token=sekret").await).await;
    assert_eq!(status, StatusCode::OK, "correct query token");
    assert_eq!(body, "[]", "fresh archive lists no labels");
}

/// Auth-on: the static UI (the router fallback) stays public so the browser
/// can reach the login form; only /api/* is gated.
#[tokio::test]
async fn auth_on_ui_stays_public() {
    let dir = test_dir("auth-ui");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn_with_token(dir.clone(), "sekret").await;
    let _ = addr;

    // The API is gated...
    assert_eq!(
        get(&state, "/api/labels").await.status(),
        StatusCode::UNAUTHORIZED
    );

    // ...but the UI and its assets serve without a token.
    for path in ["/", "/app.js", "/style.css"] {
        let (status, body) = body_string(get(&state, path).await).await;
        assert_eq!(status, StatusCode::OK, "{path} must stay public");
        assert!(!body.contains("needs_token"), "{path} leaked the 401");
    }
}

/// `limit=0` is a 400 (the cap must be > 0) - the list endpoints' other
/// validation case, distinct from a non-numeric limit.
#[tokio::test]
async fn list_limit_zero_is_rejected() {
    let dir = test_dir("limit0");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let (status, _) = body_string(get(&state, "/api/labels?limit=0").await).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "limit=0 must 400 (a cap of zero rows is meaningless)"
    );
}

/// The list endpoint's pagination corner: `limit` caps the newest-first
/// walk and `since` skips rows below its ts_ms floor - exercised with both
/// set so neither filter short-circuits the other.
#[tokio::test]
async fn list_since_and_limit_compose() {
    let dir = test_dir("listfilters");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    // Two labels with distinct timestamps (separate sessions, 50 ms apart -
    // ts_ms is millisecond-granular, so a 5 ms gap is not guaranteed).
    send_label(addr, b"^XA^GB20,20,2,B^XZ").await;
    wait_labels(&state, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    send_label(addr, b"^XA^GB20,20,2,B^XZ").await;
    wait_labels(&state, 2).await;

    let (_, body) = body_string(get(&state, "/api/labels").await).await;
    let all: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(all.len(), 2);

    // since = just past the first label's ts: only the second survives the
    // floor (the floor is inclusive - `ts == since` passes).
    let ts1 = all[1]["ts_ms"].as_u64().unwrap(); // list is newest-first
    let (_, body) =
        body_string(get(&state, format!("/api/labels?since={}", ts1 + 1).as_str()).await).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 1, "since keeps only rows at/after the floor");
    assert_eq!(
        rows[0]["id"].as_u64().unwrap(),
        all[0]["id"].as_u64().unwrap()
    );

    // since runs BEFORE the limit cap: the walk skips old rows, then the
    // cap applies to the remainder.
    let (_, body) = body_string(
        get(
            &state,
            format!("/api/labels?since={}&limit=5", ts1 + 1).as_str(),
        )
        .await,
    )
    .await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 1);
}

/// 500 paths: the internal-error constructor (AppError::internal) is only
/// reachable when an archive file read fails with a real IO error - a
/// directory squatting on the label's filename is EISDIR, not NotFound.
#[tokio::test]
async fn corroded_archive_file_reads_are_500() {
    let dir = test_dir("internal");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;
    send_label(addr, b"^XA^GB20,20,2,B^XZ").await;
    wait_labels(&state, 1).await;

    // Replace label 1's .zpl with a directory: fs::read fails EISDIR.
    let labels = dir.join("labels");
    std::fs::remove_file(labels.join("1.zpl")).unwrap();
    std::fs::create_dir(labels.join("1.zpl")).unwrap();
    let resp = get(&state, "/api/labels/1.zpl").await;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a read error is a 500, not a 404"
    );
    assert!(body_string(resp).await.1.contains("reading label"));
    drop(std::fs::remove_dir_all(labels.join("1.zpl")));

    // Same for the IR read on the detail endpoint.
    std::fs::remove_file(labels.join("1.json")).unwrap();
    std::fs::create_dir(labels.join("1.json")).unwrap();
    let resp = get(&state, "/api/labels/1").await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    drop(std::fs::remove_dir_all(labels.join("1.json")));
}

/// The direct-render 400 body carries the parse diagnostics when the input
/// has non-empty messages (the plain "^XA..^XZ" hint is only the detail-less
/// fallback).
#[tokio::test]
async fn render_direct_reports_parse_diagnostics_in_400() {
    let dir = test_dir("render-diag");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    // ^QX is an unknown command: it raises a diagnostic but no complete
    // format, so the 400 must quote the message.
    let resp = post_bytes(&state, "/api/render", b"^QX bogus").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let (_, body) = body_string(resp).await;
    assert!(
        body.contains("^QX") || body.contains("unknown"),
        "the diagnostic message is quoted: {body}"
    );
}

/// PUT with every patchable field present: each Option arm of the merge
/// must apply its value (the existing tests only touch paper_out).
#[tokio::test]
async fn put_printer_state_applies_all_fields() {
    let dir = test_dir("putall");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let (status, body) = body_string(
        put_json(
            &state,
            "/api/printer-state",
            r#"{
                "paper_out": true, "ribbon_out": true, "head_open": true,
                "rewind_full": true, "head_temp": "Warm", "pause": true,
                "comm_diag": true, "media_type": "Continuous",
                "print_mode": "ThermalTransfer", "static_ram": true
            }"#,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    for (key, expected) in [
        ("paper_out", serde_json::json!(true)),
        ("ribbon_out", serde_json::json!(true)),
        ("head_open", serde_json::json!(true)),
        ("rewind_full", serde_json::json!(true)),
        ("head_temp", serde_json::json!("Warm")),
        ("pause", serde_json::json!(true)),
        ("comm_diag", serde_json::json!(true)),
        ("media_type", serde_json::json!("Continuous")),
        ("print_mode", serde_json::json!("ThermalTransfer")),
        ("static_ram", serde_json::json!(true)),
    ] {
        assert_eq!(v[key], expected, "{key} merged from the patch");
    }
}

/// GET /api/storage reports fonts and formats maps too (the existing test
/// only exercises the images map).
#[tokio::test]
async fn storage_reports_fonts_and_formats() {
    let dir = test_dir("storagemore");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    state
        .memory
        .lock()
        .await
        .fonts
        .insert("F.TTF".into(), vec![1, 2, 3, 4]);
    state
        .memory
        .lock()
        .await
        .formats
        .insert("FMT".into(), vec![5, 6]);
    state.memory.lock().await.images.insert(
        "LOGO.GRF".into(),
        Image {
            width: 8,
            height: 8,
            data: vec![0u8; 8],
        },
    );

    let (_, body) = body_string(get(&state, "/api/storage").await).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["counts"]["images"], 1);
    assert_eq!(v["fonts"]["F.TTF"], 4, "font size is its byte length");
    assert_eq!(v["formats"]["FMT"], 2);
    assert_eq!(v["images"]["LOGO.GRF"]["width"], 8);
}

/// The persistence debounce: changing state must hit disk (memory.json /
/// state.json / index.json) after one tick - the fingerprint path that the
/// explicit persist_now tests bypass.
#[tokio::test]
async fn spawn_persistence_writes_changes_after_tick() {
    let dir = test_dir("debounce");
    let _keep = Cleanup(dir.clone());
    let (addr, state) = spawn(dir.clone()).await;

    let handle = zpl_server::http::spawn_persistence(state.clone());
    // Nothing changed yet: a tick must NOT write.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !dir.join("memory.json").exists(),
        "no write before a change"
    );

    // Change memory - every map that the fingerprint folds over (fonts and
    // formats too, not just images), then wait a tick.
    state.memory.lock().await.images.insert(
        "LOGO.GRF".into(),
        Image {
            width: 8,
            height: 8,
            data: vec![0u8; 8],
        },
    );
    state
        .memory
        .lock()
        .await
        .fonts
        .insert("A.TTF".into(), vec![1, 2, 3]);
    state
        .memory
        .lock()
        .await
        .formats
        .insert("FMT".into(), vec![9]);
    send_label(addr, b"^XA^GB20,20,2,B^XZ").await;
    wait_labels(&state, 1).await;
    put_json(&state, "/api/printer-state", r#"{"paper_out":true}"#).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    handle.abort();

    assert!(dir.join("memory.json").exists(), "memory persisted");
    assert!(dir.join("state.json").exists(), "conditions persisted");
    assert!(dir.join("index.json").exists(), "rows persisted");
    let ps: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(ps["paper_out"], true);
}

/// `dpi=0` on the direct-render endpoint is a 400 (the cap that validates
/// the playback dpi - the same guard the rerender endpoint has is enforced
/// here on `POST /api/render`).
#[tokio::test]
async fn render_direct_rejects_dpi_zero() {
    let dir = test_dir("renderdpi0");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    let (status, _) =
        body_string(post_bytes(&state, "/api/render?dpi=0", b"^XA^GB20,20,2,B^XZ").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "dpi=0 must be rejected");
}

/// A persist write failure is logged and retried, not silently dropped: a
/// directory squatting on the target path makes write_atomic fail.
#[tokio::test]
async fn persist_failure_is_retried_not_forgotten() {
    let dir = test_dir("persisterr");
    let _keep = Cleanup(dir.clone());
    let (_addr, state) = spawn(dir.clone()).await;

    // A directory at memory.json makes its write_atomic fail; the others
    // still land.
    let mem_target = dir.join("memory.json");
    std::fs::create_dir(&mem_target).unwrap();
    put_json(&state, "/api/printer-state", r#"{"paper_out":true}"#).await;
    let out = zpl_server::http::persist_now(&state).await;
    assert!(!out.memory, "failed memory write reported");
    assert!(out.conditions, "conditions still written");
    assert!(out.rows, "rows still written");

    // Remove the blocker: the same call now succeeds (nothing was forgotten).
    std::fs::remove_dir(&mem_target).unwrap();
    let out = zpl_server::http::persist_now(&state).await;
    assert!(out.memory);
}

/// AppState::new falls back to defaults when the persisted condition /
/// memory files are present but unreadable (corrupt JSON), never a boot
/// failure - the "ignoring unreadable" log branches.
#[tokio::test]
async fn corrupt_persisted_state_falls_back_to_defaults() {
    let dir = test_dir("corrupt");
    let _keep = Cleanup(dir.clone());
    let (_addr, _state) = spawn(dir.clone()).await; // creates the archive dir

    // The files exist but are garbage JSON.
    std::fs::write(dir.join("state.json"), b"{not json").unwrap();
    std::fs::write(dir.join("memory.json"), b"[1,2").unwrap();

    let state = Arc::new(AppState::new(
        zpl_server::archive::Archive::open(dir.clone()).unwrap(),
    ));
    assert!(!state.printer_state.lock().await.conditions.paper_out);
    assert!(
        !state
            .printer_state
            .lock()
            .await
            .conditions
            .media_type
            .eq(&zpl_server::state::MediaType::Continuous),
        "default media type stays"
    );
    assert!(state.memory.lock().await.images.is_empty());
}
