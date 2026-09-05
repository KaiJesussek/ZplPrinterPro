//! HTTP API (Task 21): the axum router behind `fn router(state)` - the
//! labels REST surface, printer-state GET/PUT, storage + diagnostics
//! views, the SSE event stream, and the debounced disk persistence of
//! `Memory` + `Conditions` (+ the archive row index).
//!
//! Endpoints (see the design spec secREST API):
//! - `GET /api/labels?limit=&since=` -> newest-first `[LabelRow]` (since =
//!   `ts_ms` floor, limit caps the count)
//! - `GET /api/labels/{id}` -> `{"meta": LabelRow, "ir": Label, "diags": [..]}`
//! - `GET /api/labels/{id}.zpl` | `{id}.png` -> raw bytes / cached render
//! - `POST /api/labels/{id}/rerender?dpi=` -> re-parse the stored raw
//!   (obtains the PrinterMode per the T10 ruling), re-render at `dpi`,
//!   rewrite `{id}.png`
//! - `POST /api/render?dpi=` -> direct ZPL entry (web UI playground):
//!   parse+render arbitrary ZPL text, PNG inline, nothing archived
//! - `GET|PUT /api/printer-state` -> `Conditions` JSON; PUT is a partial
//!   patch, unknown enum strings 400
//! - `GET /api/storage` -> images/fonts/formats with sizes (the honest
//!   `www` view; image pixel data is never shipped)
//! - `GET /api/diagnostics` -> boot counters (labels/bytes/warnings/
//!   unknown_commands/prunes)
//! - `GET /api/events` -> SSE: `event: label` (new row, `LabelRow` data)
//!   and `event: state` (merged `Conditions` data from PUT)
//!
//! Errors: unknown ids -> 404, malformed params/bodies -> 400, never a
//! panic on hostile input. Persistence: `dir/memory.json` (Memory),
//! `dir/state.json` (Conditions) and `dir/index.json` (archive rows) are
//! written by [`spawn_persistence`]'s 500 ms debounce, only when a cheap
//! fingerprint shows a change; `AppState::new` reloaded them at boot.

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::services::ServeDir;

use zpl::cmd::parse_stream_collect;
use zpl::diag::Diagnostics;
use zpl::fonts::DefaultFontProvider;
use zpl::memory::Memory;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;

use crate::archive::{write_atomic, LabelRow};
use crate::state::{AppState, Conditions, Event};

// ---------------------------------------------------------------------------
// Router

/// Build the API router around shared server state (the test seam: tests
/// drive it with `tower::ServiceExt::oneshot`).
///
/// The API routes are registered first; anything unmatched falls through
/// to the static UI (`static/index.html` + `app.js` + `style.css`), so
/// `/api/*` is never shadowed by the filesystem. `STATIC_DIR` is resolved
/// at compile time, so the binary serves its UI regardless of CWD.
const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/labels", get(list_labels))
        .route("/api/labels/{id}", get(label_get))
        .route("/api/labels/{id}/rerender", post(rerender))
        .route("/api/render", post(render_direct))
        .route(
            "/api/printer-state",
            get(get_printer_state).put(put_printer_state),
        )
        .route("/api/storage", get(get_storage))
        .route("/api/diagnostics", get(get_diagnostics))
        .route("/api/events", get(events_sse))
        .fallback_service(ServeDir::new(STATIC_DIR))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Errors

/// API error: a status plus a JSON `{"error": ...}` body. 4xx for client
/// mistakes, 500 only for server-side inconsistencies (corrupt archive
/// files) - never a panic.
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn not_found(message: impl Into<String>) -> Self {
        AppError {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        AppError {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        AppError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// GET /api/labels

/// Missing params are `None` (serde treats absent `Option` fields as such);
/// garbage (`limit=abc`) is a serde_urlencoded error, which axum turns
/// into a 400 before the handler runs.
#[derive(Deserialize)]
struct ListParams {
    limit: Option<usize>,
    since: Option<u64>,
}

async fn list_labels(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<LabelRow>>, AppError> {
    if params.limit == Some(0) {
        return Err(AppError::bad_request("limit must be > 0"));
    }
    let rows = state.archive.lock().await;
    let mut out: Vec<LabelRow> = Vec::new();
    for entry in rows.rows().iter().rev() {
        if let Some(since) = params.since {
            if entry.row.ts_ms < since {
                continue;
            }
        }
        out.push(entry.row.clone());
        if let Some(limit) = params.limit {
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// GET /api/labels/{id} (+ {id}.zpl / {id}.png)

async fn label_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let (base, kind) = if let Some(base) = id.strip_suffix(".zpl") {
        (base, "zpl")
    } else if let Some(base) = id.strip_suffix(".png") {
        (base, "png")
    } else {
        (id.as_str(), "detail")
    };
    let id: u64 = base
        .parse()
        .map_err(|_| AppError::not_found("no such label"))?;

    match kind {
        "zpl" => {
            let raw = state
                .archive
                .lock()
                .await
                .read_raw(id)
                .map_err(|e| AppError::internal(format!("reading label {id}: {e}")))?
                .ok_or_else(|| AppError::not_found(format!("no such label {id}")))?;
            Ok((StatusCode::OK, [(header::CONTENT_TYPE, "text/plain")], raw).into_response())
        }
        "png" => {
            let png = state
                .archive
                .lock()
                .await
                .read_png(id)
                .map_err(|e| AppError::internal(format!("reading label {id}: {e}")))?
                .ok_or_else(|| AppError::not_found(format!("no such label {id}")))?;
            Ok((StatusCode::OK, [(header::CONTENT_TYPE, "image/png")], png).into_response())
        }
        _ => {
            let (row, messages, ir) = {
                let archive = state.archive.lock().await;
                let entry = archive
                    .find_row(id)
                    .ok_or_else(|| AppError::not_found(format!("no such label {id}")))?;
                let ir = archive
                    .read_ir(id)
                    .map_err(|e| AppError::internal(format!("reading label {id} IR: {e}")))?
                    .ok_or_else(|| AppError::not_found(format!("no such label {id}")))?;
                (entry.row.clone(), entry.messages.clone(), ir)
            };
            Ok(Json(serde_json::json!({
                "meta": row,
                "ir": ir,
                "diags": messages,
            }))
            .into_response())
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/labels/{id}/rerender?dpi=

#[derive(Deserialize)]
struct RerenderParams {
    dpi: u32,
}

async fn rerender(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<RerenderParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id: u64 = id
        .parse()
        .map_err(|_| AppError::not_found("no such label"))?;
    if params.dpi == 0 {
        return Err(AppError::bad_request("dpi must be >= 1"));
    }

    // The label must exist (the archive is the single source of truth).
    let raw = {
        let archive = state.archive.lock().await;
        if archive.find_row(id).is_none() {
            return Err(AppError::not_found(format!("no such label {id}")));
        }
        archive
            .read_raw(id)
            .map_err(|e| AppError::internal(format!("reading label {id}: {e}")))?
            .ok_or_else(|| AppError::not_found(format!("no such label {id}")))?
    };

    // Re-parse the raw .zpl under the CURRENT shared memory (stored images
    // resolve like they did at print time) to obtain the PrinterMode -
    // render-time `^LH`/`^PO`/`^PM` effects (T6 ruling) need the parse,
    // not the stored IR (T10 ruling).
    let (png, width, height) = {
        let mut guard = state.memory.lock().await;
        let mut diags = Diagnostics::default();
        // Parse and render must not overlap: `ctx` borrows `guard` and
        // holds the mode the T6 ruling needs at render time, so take both
        // the label and a mode clone out before opening `RenderOptions`.
        let (label, mode) = {
            let mut ctx = zpl::state::ParseContext::new(&mut diags, &mut guard);
            let mut labels = Vec::new();
            parse_stream_collect(&mut Scanner::new(&raw), &mut ctx, &mut labels);
            let pl = labels
                .pop()
                .ok_or_else(|| AppError::internal("stored raw did not re-parse to a label"))?;
            (pl.label, ctx.mode.clone())
        };
        let fonts = DefaultFontProvider::new();
        let opts = RenderOptions {
            dpi: params.dpi,
            fonts: &fonts,
            memory: &guard,
            mode: &mode,
        };
        let outcome = render(&label, &opts)
            .map_err(|e| AppError::internal(format!("re-render of label {id} failed: {e:?}")))?;
        (
            outcome.bitmap.to_png(),
            outcome.bitmap.width,
            outcome.bitmap.height,
        )
    };

    state
        .archive
        .lock()
        .await
        .write_png(id, &png)
        .map_err(|e| AppError::internal(format!("caching re-render of {id}: {e}")))?;

    Ok(Json(serde_json::json!({
        "id": id,
        "dpi": params.dpi,
        "width": width,
        "height": height,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/render?dpi= - direct ZPL entry (web UI playground)

#[derive(Deserialize)]
struct RenderParams {
    #[serde(default = "default_dpi")]
    dpi: u32,
}

fn default_dpi() -> u32 {
    203
}

/// Parse arbitrary ZPL text with a fresh `ParseContext` and return the
/// rendered PNG inline (`image/png`). The label is NOT archived -
/// playground semantics, no storage side effects other than the shared
/// memory lock (stored-image `^XG`/`^IS` targets resolve like they did at
/// print time, matching [`rerender`]). Input without a complete
/// `^XA..^XZ` format -> 400 with the parse diagnostics.
async fn render_direct(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RenderParams>,
    body: Bytes,
) -> Result<Response, AppError> {
    if params.dpi == 0 {
        return Err(AppError::bad_request("dpi must be >= 1"));
    }
    let (png, width, height) = {
        let mut guard = state.memory.lock().await;
        let (label, mode, diags) = {
            let mut diags = Diagnostics::default();
            let mut ctx = zpl::state::ParseContext::new(&mut diags, &mut guard);
            let mut labels = Vec::new();
            parse_stream_collect(&mut Scanner::new(&body), &mut ctx, &mut labels);
            (labels.pop().map(|pl| pl.label), ctx.mode.clone(), diags)
        };
        // `diags` is owned here - `ctx` (its mutable borrower) has dropped.
        let label = label.ok_or_else(|| {
            let detail = diags
                .items
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            AppError::bad_request(format!(
                "input contains no complete ^XA..^XZ format{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                }
            ))
        })?;
        let fonts = DefaultFontProvider::new();
        let opts = RenderOptions {
            dpi: params.dpi,
            fonts: &fonts,
            memory: &guard,
            mode: &mode,
        };
        let outcome = render(&label, &opts)
            .map_err(|e| AppError::internal(format!("render failed: {e:?}")))?;
        (
            outcome.bitmap.to_png(),
            outcome.bitmap.width,
            outcome.bitmap.height,
        )
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header("x-label-width", width.to_string())
        .header("x-label-height", height.to_string())
        .body(axum::body::Body::from(png))
        .map_err(|e| AppError::internal(format!("building PNG response: {e}")))
}

// ---------------------------------------------------------------------------
// GET|PUT /api/printer-state

async fn get_printer_state(State(state): State<Arc<AppState>>) -> Json<Conditions> {
    Json(state.printer_state.lock().await.conditions.clone())
}

/// PUT body: a partial patch - `Option` fields, only the present ones are
/// applied (the UI sends one checkbox at a time). Enum fields reuse the
/// `Conditions` serde, so an unknown string (`"Frozen"`) deserializes to
/// a rejection.
#[derive(Deserialize, Default)]
struct ConditionsPatch {
    paper_out: Option<bool>,
    ribbon_out: Option<bool>,
    head_open: Option<bool>,
    rewind_full: Option<bool>,
    head_temp: Option<crate::state::Temp>,
    pause: Option<bool>,
    comm_diag: Option<bool>,
    media_type: Option<crate::state::MediaType>,
    print_mode: Option<crate::state::PrintMode>,
    static_ram: Option<bool>,
}

/// `Json` wrapper turning BOTH rejection flavors (syntax + data, e.g. an
/// unknown enum string) into a 400 `AppError` - the brief's "malformed
/// bodies -> 400" contract. (axum 0.8 converts extractor rejections through
/// their own response; the default Json rejection is 422, so the wire
/// status must be lifted here.)
struct JsonValidated<T>(T);

impl<S, T> FromRequest<S> for JsonValidated<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(
        req: axum::http::Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|_| AppError::bad_request("malformed JSON body"))?;
        Ok(JsonValidated(value))
    }
}

async fn put_printer_state(
    State(state): State<Arc<AppState>>,
    JsonValidated(patch): JsonValidated<ConditionsPatch>,
) -> Json<Conditions> {
    let mut ps = state.printer_state.lock().await;
    let c = &mut ps.conditions;
    if let Some(v) = patch.paper_out {
        c.paper_out = v;
    }
    if let Some(v) = patch.ribbon_out {
        c.ribbon_out = v;
    }
    if let Some(v) = patch.head_open {
        c.head_open = v;
    }
    if let Some(v) = patch.rewind_full {
        c.rewind_full = v;
    }
    if let Some(v) = patch.head_temp {
        c.head_temp = v;
    }
    if let Some(v) = patch.pause {
        c.pause = v;
    }
    if let Some(v) = patch.comm_diag {
        c.comm_diag = v;
    }
    if let Some(v) = patch.media_type {
        c.media_type = v;
    }
    if let Some(v) = patch.print_mode {
        c.print_mode = v;
    }
    if let Some(v) = patch.static_ram {
        c.static_ram = v;
    }
    let merged = c.clone();
    drop(ps); // never hold the state lock across the (sync) broadcast send
    let _ = state.events.send(Event::State(merged.clone()));
    Json(merged)
}

// ---------------------------------------------------------------------------
// GET /api/storage

/// The `Memory` snapshot, sizes not payloads: the UI's "what's stored and
/// how big" view that makes the `~HS` `www` count explainable. `Image`
/// pixel data is never shipped (it can be MiBs); the detail sizes are
/// honest byte counts.
#[derive(Serialize)]
struct StorageView {
    images: std::collections::BTreeMap<String, StoredImage>,
    fonts: std::collections::BTreeMap<String, usize>,
    formats: std::collections::BTreeMap<String, usize>,
    counts: StorageCounts,
}

#[derive(Serialize)]
struct StoredImage {
    width: u32,
    height: u32,
    bytes: usize,
}

#[derive(Serialize)]
struct StorageCounts {
    images: usize,
    fonts: usize,
    formats: usize,
}

async fn get_storage(State(state): State<Arc<AppState>>) -> Json<StorageView> {
    let memory = state.memory.lock().await;
    Json(StorageView {
        images: memory
            .images
            .iter()
            .map(|(name, img)| {
                (
                    name.clone(),
                    StoredImage {
                        width: img.width,
                        height: img.height,
                        bytes: img.data.len(),
                    },
                )
            })
            .collect(),
        fonts: memory
            .fonts
            .iter()
            .map(|(n, b)| (n.clone(), b.len()))
            .collect(),
        formats: memory
            .formats
            .iter()
            .map(|(n, b)| (n.clone(), b.len()))
            .collect(),
        counts: StorageCounts {
            images: memory.images.len(),
            fonts: memory.fonts.len(),
            formats: memory.formats.len(),
        },
    })
}

// ---------------------------------------------------------------------------
// GET /api/diagnostics

async fn get_diagnostics(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let (labels, bytes, warnings, unknown) = (
        state.counters.labels.load(Ordering::Relaxed),
        state.counters.bytes.load(Ordering::Relaxed),
        state.counters.warnings.load(Ordering::Relaxed),
        state.counters.unknown_commands.load(Ordering::Relaxed),
    );
    let prunes = state.archive.lock().await.pruned();
    Json(serde_json::json!({
        "labels": labels,
        "bytes": bytes,
        "warnings": warnings,
        "unknown_commands": unknown,
        "prunes": prunes,
    }))
}

// ---------------------------------------------------------------------------
// GET /api/events (SSE)

async fn events_sse(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = BroadcastStream::new(rx).map_while(|item| match item {
        Ok(Event::Label(row)) => Some(Ok(SseEvent::default()
            .event("label")
            .json_data(row)
            .expect("LabelRow serializes"))),
        Ok(Event::State(conditions)) => Some(Ok(SseEvent::default()
            .event("state")
            .json_data(conditions)
            .expect("Conditions serializes"))),
        // A subscriber that lagged behind (or the sender closing) ENDS the
        // stream - `map_while` returning None terminates it, so EventSource
        // sees the connection close and reconnects, and the UI repaints
        // from GETs on the 'open' event. (Skipping the lagged read would
        // keep a stale, permanently missynchronized stream alive.)
        Err(_) => None,
    });
    // 15 s comment pings keep intermediaries from killing the idle stream.
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

// ---------------------------------------------------------------------------
// Persistence: debounced writes of memory.json / state.json / index.json

/// Which of the three persisted files a write pass targets.
#[derive(Clone, Copy)]
enum Slot {
    Memory,
    Conditions,
    Rows,
}

impl Slot {
    fn path(self, state: &AppState) -> std::path::PathBuf {
        match self {
            Slot::Memory => state.persist_dir.join("memory.json"),
            Slot::Conditions => state.persist_dir.join("state.json"),
            Slot::Rows => state.persist_dir.join("index.json"),
        }
    }
}

/// Per-file write results of a `persist_now` pass: only files that actually
/// hit disk advance the debounce fingerprint, so a transient failure is
/// retried on the next tick instead of being forgotten because `last`
/// advanced unconditionally.
#[derive(Default, Clone, Copy)]
pub struct PersistOutcome {
    pub memory: bool,
    pub conditions: bool,
    pub rows: bool,
}

/// Serialize the live state to `dir/memory.json`, `dir/state.json`
/// (`Conditions`) and `dir/index.json` (archive rows) - the shared seam
/// the debounce calls and the persistence tests call directly. Writes are
/// atomic (temp + rename); failures log and are retried on the next tick
/// via [`PersistOutcome`].
pub async fn persist_now(state: &AppState) -> PersistOutcome {
    let memory = {
        let m = state.memory.lock().await;
        serde_json::to_vec(&*m).ok()
    };
    let conditions = {
        let ps = state.printer_state.lock().await;
        serde_json::to_vec(&ps.conditions).ok()
    };
    let rows = {
        let archive = state.archive.lock().await;
        serde_json::to_vec(archive.rows()).ok()
    };
    let mut out = PersistOutcome::default();
    for (slot, bytes) in [
        (Slot::Memory, memory),
        (Slot::Conditions, conditions),
        (Slot::Rows, rows),
    ] {
        if let Some(bytes) = bytes {
            match write_atomic(&slot.path(state), &bytes) {
                Ok(()) => match slot {
                    Slot::Memory => out.memory = true,
                    Slot::Conditions => out.conditions = true,
                    Slot::Rows => out.rows = true,
                },
                Err(e) => crate::log!(
                    "persist: writing {} failed: {e}",
                    slot.path(state).display()
                ),
            }
        }
    }
    out
}

/// FNV-1a fold of the stored bytes - the content-sensitive memory
/// fingerprint. Map lengths / byte totals were content-blind: a same-length
/// re-download (~DG overwriting an image by name) changed nothing, so the
/// new bytes never hit disk. The hash walks the maps deterministically
/// (BTreeMap key order), so identical state always folds identically.
fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn memory_fingerprint(m: &Memory) -> u64 {
    let mut h = fnv1a(0xcbf29ce484222325, b"memory\x00");
    for (name, img) in &m.images {
        h = fnv1a(fnv1a(h, name.as_bytes()), &img.data);
    }
    for (name, font) in &m.fonts {
        h = fnv1a(fnv1a(h, name.as_bytes()), font);
    }
    for (name, fmt) in &m.formats {
        h = fnv1a(fnv1a(h, name.as_bytes()), fmt);
    }
    h
}

/// Cheap change fingerprint of the mutable state, so the 500 ms tick only
/// writes when something actually changed (memory.json ships image bytes -
/// a full write every tick under load would be wasteful).
struct Fingerprint {
    memory: u64,
    conditions: Option<Vec<u8>>,
    rows: (usize, Option<u64>, Option<u64>),
}

async fn fingerprint(state: &AppState) -> Fingerprint {
    let memory = memory_fingerprint(&*state.memory.lock().await);
    let conditions = {
        let ps = state.printer_state.lock().await;
        serde_json::to_vec(&ps.conditions).ok()
    };
    let rows = {
        let archive = state.archive.lock().await;
        (
            archive.rows().len(),
            archive.rows().first().map(|e| e.row.id),
            archive.rows().last().map(|e| e.row.id),
        )
    };
    Fingerprint {
        memory,
        conditions,
        rows,
    }
}

/// Spawn the debounced persistence loop: wakes every 500 ms, writes only
/// when a fingerprint changed since the last write. `index.json` (the
/// archive row index) is refreshed the same way - a commit's index write
/// can lag by up to one tick, and `Archive::open` re-stubs from files on
/// the next boot, so a crash in the window loses nothing but the (already
/// eprintln'd) diagnostic messages of the newest seconds.
pub fn spawn_persistence(state: Arc<AppState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = fingerprint(&state).await;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = fingerprint(&state).await;
            if now.memory != last.memory
                || now.conditions != last.conditions
                || now.rows != last.rows
            {
                let out = persist_now(&state).await;
                // Only files actually written advance their fingerprint: a
                // failed write keeps the old value and retries every tick
                // until it succeeds or the state changes again.
                if out.memory {
                    last.memory = now.memory;
                }
                if out.conditions {
                    last.conditions = now.conditions;
                }
                if out.rows {
                    last.rows = now.rows;
                }
            }
        }
    })
}

/// Bind the HTTP listener and serve the router; the main.rs spawn seam.
pub async fn spawn_http(state: Arc<AppState>, port: u16) -> std::io::Result<JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let app = router(state);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            crate::log!("http: serve error: {e}");
        }
    });
    Ok(handle)
}
