//! TCP session driver: accept loop, per-connection parse -> render ->
//! archive. Parse problems are diagnostics, never fatal - the socket stays
//! open and the next label prints (global constraint).

use std::collections::BTreeMap;
use std::mem;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use zpl::cmd::{parse_stream_collect, ParsedLabel};
use zpl::diag::{Diagnostic, Diagnostics, Severity};
use zpl::fonts::DefaultFontProvider;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{FormatState, ParseContext, PrinterConfig, PrinterMode, QueryRequest};

use crate::archive::{unix_ms, LabelRow};
use crate::hs::hs_response;
use crate::proto;
// Task 21 moved the server-wide state here (it now also owns the SSE
// channel); `pub use` keeps the pre-T21 `zpl_server::tcp::AppState` import
// path alive for the integration tests.
pub use crate::state::{AppState, Counters};

/// Parser state that must survive read-chunk boundaries. `ParseContext` is
/// rebuilt per chunk so the shared `Memory` lock is held only for the parse
/// window, not for the whole connection: an idle socket must not stall
/// other connections, and Task 21's HTTP rerender takes the same lock.
struct Persist {
    state: FormatState,
    mode: PrinterMode,
    config: PrinterConfig,
    serials: BTreeMap<u8, i64>,
    /// Global offset of the open format's ^XA. `ParseContext::new` re-inits
    /// `format_start` to 0, so a label opened in one read chunk and closed
    /// by ^XZ in a later one would otherwise archive from offset 0 - which
    /// includes any retained preamble before the ^XA.
    format_start: usize,
}

impl Default for Persist {
    fn default() -> Self {
        Persist {
            state: FormatState::default(),
            mode: PrinterMode::default(),
            config: PrinterConfig::default(),
            serials: BTreeMap::new(),
            format_start: 0,
        }
    }
}

impl Persist {
    fn hand_in(&mut self, ctx: &mut ParseContext) {
        ctx.state = mem::take(&mut self.state);
        mem::swap(&mut ctx.mode, &mut self.mode);
        mem::swap(&mut ctx.config, &mut self.config);
        ctx.serial_counters = mem::take(&mut self.serials);
        ctx.format_start = self.format_start;
    }

    fn hand_out(&mut self, ctx: &mut ParseContext) {
        self.state = mem::take(&mut ctx.state);
        mem::swap(&mut self.mode, &mut ctx.mode);
        mem::swap(&mut self.config, &mut ctx.config);
        self.serials = mem::take(&mut ctx.serial_counters);
        self.format_start = ctx.format_start;
    }
}

// Task 21's `AppState`/`Counters` live in `state.rs` (re-exported above);
// the TCP module owns the accept loop and session driver only.

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const READ_BUF: usize = 8192;
/// Hostile-input cap per connection: a format's raw span is retained in the
/// session buffer until its ^XZ; anything above this closes the connection
/// (resource protection, not a parse error).
const MAX_STREAM_BYTES: usize = 64 << 20;
/// Default print resolution (203 dpi - the emulator's default model).
const DPI: u32 = 203;

/// Accept loop; the testable seam - tests bind an ephemeral port and hand
/// the listener here. Runs until the listener fails.
pub fn spawn_tcp(listener: TcpListener, state: Arc<AppState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        handle_connection(stream, state).await;
                    });
                }
                Err(e) => {
                    eprintln!("tcp: accept error: {e}; stopping accept loop");
                    break;
                }
            }
        }
    })
}

/// One TCP session: read chunks, parse incrementally, render and archive
/// each completed label, and answer host queries (`~HS`/`~HQ`/`~WD`/`~HI`
/// between formats, `^HV` any time) on the same socket. Query detection is
/// scanner-driven: `~` is only a command character *outside* field data,
/// so a literal `~HS` embedded in a ^FD body reaches no handler (the
/// parse handlers record `QueryRequest`s, which this session drains after
/// each parse window). EOF with an unfinished format records a diagnostic
/// and archives nothing.
async fn handle_connection(stream: TcpStream, state: Arc<AppState>) {
    let (mut rd, mut wr) = stream.into_split();
    let mut diags = Diagnostics::default();
    let mut persist = Persist::default();
    // All input retained for exact raw-label spans; trimmed after each
    // archived label (see the drain below).
    let mut input: Vec<u8> = Vec::new();
    let mut buf = [0u8; READ_BUF];

    loop {
        let n = match tokio::time::timeout(IDLE_TIMEOUT, rd.read(&mut buf)).await {
            Ok(Ok(0)) => break, // EOF: print on ^XZ already flushed, then quit
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                eprintln!("tcp: read error: {e}");
                break;
            }
            Err(_) => break, // idle timeout
        };
        let chunk: &[u8] = &buf[..n];

        state.counters.bytes.fetch_add(n as u64, Ordering::Relaxed);
        let chunk_start = input.len();
        input.extend_from_slice(chunk);
        if input.len() > MAX_STREAM_BYTES {
            eprintln!("tcp: input cap ({MAX_STREAM_BYTES} bytes) exceeded; closing connection");
            break;
        }

        let mut labels: Vec<ParsedLabel> = Vec::new();
        let mut responses: Vec<Vec<u8>> = Vec::new();
        {
            let mut guard = state.memory.lock().await;
            let queries: Vec<QueryRequest>;
            {
                let mut ctx = ParseContext::new(&mut diags, &mut guard);
                persist.hand_in(&mut ctx);
                ctx.stream_base = chunk_start;
                // Re-base the token offsets: spans are computed against the
                // CURRENT chunk, and `input` may have been drained at the
                // front since the last parse (the driver's cross-chunk
                // continuity only holds while the stream is untrimmed).
                ctx.token_start = chunk_start;
                ctx.token_end = chunk_start;
                parse_stream_collect(&mut Scanner::new(&input[chunk_start..]), &mut ctx, &mut labels);
                persist.hand_out(&mut ctx);
                queries = ctx.queries.drain(..).collect();
            }
            for pl in &labels {
                let raw = &input[pl.start..pl.end];
                for d in &diags.items {
                    if matches!(d.severity, Severity::UnknownCommand) {
                        state
                            .counters
                            .unknown_commands
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    eprintln!("tcp: {}", d.message);
                }
                let diag_count = diags.items.len();
                let messages: Vec<String> = diags.items.iter().map(|d| d.message.clone()).collect();
                // Render inside its own scope: RenderOptions borrows a
                // `&dyn FontProvider`, which is not Sync, so `opts` must
                // not live across the archive await below.
                let (png, height) = {
                    let fonts = DefaultFontProvider::new();
                    let opts = RenderOptions {
                        dpi: DPI,
                        fonts: &fonts,
                        memory: &guard,
                        mode: &persist.mode,
                    };
                    match render(&pl.label, &opts) {
                        Ok(o) => (o.bitmap.to_png(), o.bitmap.height),
                        Err(e) => {
                            // Only reachable from hand-built IR; parsed
                            // labels always render. Skip rather than
                            // archive a broken row (the archive's 3-file
                            // layout stays intact).
                            eprintln!("tcp: render failed: {e:?}; label not archived");
                            diags.items.clear();
                            continue;
                        }
                    }
                };
                let mut row = LabelRow {
                    id: 0, // push assigns
                    ts_ms: unix_ms(),
                    bytes: raw.len(),
                    diags: diag_count,
                    width: pl.label.width_dots,
                    height: pl.label.height_dots,
                };
                let mut archive = state.archive.lock().await;
                match archive.push(&mut row, raw, &pl.label, &png, &messages) {
                    Ok(_id) => {
                        state.counters.labels.fetch_add(1, Ordering::Relaxed);
                        state.counters.warnings.fetch_add(diag_count as u64, Ordering::Relaxed);
                        // Label completion -> printer state. `labels_remaining`
                        // adds this format's ^PQ copies and consumes the
                        // first printed copy (v1 renders each format once, so
                        // a ^PQn batch leaves n-1 outstanding); the numbered
                        // map and the measured label length come from this
                        // label; every print advances the odometer.
                        let mut ps = state.printer_state.lock().await;
                        ps.labels_remaining = ps
                            .labels_remaining
                            .saturating_add(pl.label.copies)
                            .saturating_sub(1)
                            .min(99_999_999);
                        ps.meter_total = ps.meter_total.saturating_add(1);
                        ps.label_length_dots = height;
                        ps.numbered = pl.label.numbered.clone();
                    }
                    Err(e) => eprintln!("tcp: archive push failed: {e}"),
                }
                drop(archive);
                // Push the label event after the archive lock drops (the
                // broadcast send is sync/non-blocking - subscribers never
                // stall the printer). `row.id` is 0 only when push failed.
                if row.id != 0 {
                    let _ = state.events.send(crate::state::Event::Label(row.clone()));
                }
                diags.items.clear(); // diags are attributed to this label
            }
            // Query responses are built after the chunk's labels complete,
            // so ~HS/~HV read the post-print state (labels first, then the
            // queries that followed them - stream order within the chunk;
            // a query that answered mid-batch in real time sees the batch
            // after this chunk's prints, a v1 simplification).
            {
                let mut ps = state.printer_state.lock().await;
                ps.partial_format = persist.state.is_partial();
                for q in &queries {
                    let resp: Option<Vec<u8>> = match q {
                        QueryRequest::HostStatus => {
                            let out = hs_response(&ps, guard.images.len());
                            if out.is_none() {
                                eprintln!(
                                    "tcp: ~HS suppressed (paper_out|ribbon_out|head_open|rewind_full|over-temp)"
                                );
                            }
                            out
                        }
                        QueryRequest::HeadQuery(qt) => match proto::respond_hq(qt, &ps) {
                            Some(r) => Some(r),
                            None => {
                                eprintln!(
                                    "tcp: unsupported ~HQ query '{}' ignored (socket kept open)",
                                    String::from_utf8_lossy(qt)
                                );
                                None
                            }
                        },
                        QueryRequest::ConfigDump(_) => Some(proto::respond_wd(&ps)),
                        QueryRequest::ModelId => Some(proto::respond_hi()),
                        QueryRequest::HostValue(req) => Some(proto::respond_hv(req, &ps.numbered)),
                    };
                    if let Some(r) = resp {
                        responses.push(r);
                    }
                }
            }
        }

        // Flush the chunk's query responses after the locks drop.
        for r in &responses {
            if let Err(e) = wr.write_all(r).await {
                eprintln!("tcp: write error: {e}");
                break;
            }
        }

        // Free everything before the last completed label: spans are only
        // read during the loop above, and a partial format started before
        // the drain is fine (its ^XA offset -> format_start is refreshed by
        // any later ^XA; the next chunk's token offsets are re-based
        // below). Keep `format_start` pointing at the same format: a live
        // partial format's ^XA always sits after the drain point (labels
        // close an open format before a new ^XA can open one), but clamp
        // defensively so the offset is never stale if the driver evolves.
        if let Some(last) = labels.last() {
            input.drain(..last.end);
            persist.format_start = persist.format_start.saturating_sub(last.end);
        }
    }

    // EOF/idle with an open format: the label never prints. Diagnosed, not
    // archived (Task 20 keys its ~HS partial-format flag off the same live
    // state via PrinterState).
    if persist.state.is_partial() {
        let msg = "connection closed with an unfinished format; label not archived";
        eprintln!("tcp: {msg}");
        diags.push(Diagnostic::warning("^XA", msg));
        state
            .counters
            .warnings
            .fetch_add(1, Ordering::Relaxed);
    }
}
