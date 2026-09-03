//! TCP session driver: accept loop, per-connection parse -> render ->
//! archive. Parse problems are diagnostics, never fatal - the socket stays
//! open and the next label prints (global constraint).

use std::collections::BTreeMap;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use zpl::cmd::{parse_stream_collect, ParsedLabel};
use zpl::diag::{Diagnostic, Diagnostics};
use zpl::fonts::DefaultFontProvider;
use zpl::memory::Memory;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{FormatState, ParseContext, PrinterConfig, PrinterMode};

use crate::archive::{unix_ms, Archive, LabelRow};

/// Parser state that must survive read-chunk boundaries. `ParseContext` is
/// rebuilt per chunk so the shared `Memory` lock is held only for the parse
/// window, not for the whole connection: an idle socket must not stall
/// other connections, and Task 21's HTTP rerender takes the same lock.
struct Persist {
    state: FormatState,
    mode: PrinterMode,
    config: PrinterConfig,
    serials: BTreeMap<u8, i64>,
}

impl Default for Persist {
    fn default() -> Self {
        Persist {
            state: FormatState::default(),
            mode: PrinterMode::default(),
            config: PrinterConfig::default(),
            serials: BTreeMap::new(),
        }
    }
}

impl Persist {
    fn hand_in(&mut self, ctx: &mut ParseContext) {
        ctx.state = mem::take(&mut self.state);
        mem::swap(&mut ctx.mode, &mut self.mode);
        mem::swap(&mut ctx.config, &mut self.config);
        ctx.serial_counters = mem::take(&mut self.serials);
    }

    fn hand_out(&mut self, ctx: &mut ParseContext) {
        self.state = mem::take(&mut ctx.state);
        mem::swap(&mut self.mode, &mut ctx.mode);
        mem::swap(&mut self.config, &mut ctx.config);
        self.serials = mem::take(&mut ctx.serial_counters);
    }
}

/// Accumulated session counters (survive forever; Task 20's `~HS` and
/// Task 21's `/api/diagnostics` read them).
#[derive(Debug, Default)]
pub struct Counters {
    pub labels: AtomicU64,
    pub bytes: AtomicU64,
    pub warnings: AtomicU64,
}

/// Server-wide state shared by TCP (and, from Task 21, HTTP) tasks.
pub struct AppState {
    pub archive: Mutex<Archive>,
    /// The printer's storage, global like a real printer's: an image stored
    /// by one connection is visible to the next. Locked per parse chunk -
    /// stored data lives in `Memory` itself, so per-chunk locking changes
    /// nothing semantically.
    pub memory: Arc<Mutex<Memory>>,
    pub counters: Counters,
}

impl AppState {
    pub fn new(archive: Archive) -> Self {
        AppState {
            archive: Mutex::new(archive),
            memory: Arc::new(Mutex::new(Memory::default())),
            counters: Counters::default(),
        }
    }
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const READ_BUF: usize = 8192;
/// Hostile-input cap per connection: a format's raw span is retained in the
/// session buffer until its ^XZ; anything above this closes the connection
/// (resource protection, not a parse error).
const MAX_STREAM_BYTES: usize = 64 << 20;
/// Default print resolution (203 dpi - the emulator's default model).
const DPI: u32 = 203;
/// `~HS` placeholder until Task 20 wires the real responder; framed STX/ETX
/// like real printer-status responses so client scanners keep framing.
const HS_PLACEHOLDER: &[u8] = b"\x02zpl-server placeholder (real ~HS in Task 20)\x03\r\n";

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
/// each completed label. `~HS` gets the placeholder response. EOF with an
/// unfinished format records a diagnostic and archives nothing.
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

        // ~HS placeholder, inline on the same socket. Task 20 replaces this
        // byte scan with real table-driven routing.
        // ponytail: literal `~HS` inside a ^FD body also answers (placeholder
        // only, harmless); revisit with the T20 responder.
        if chunk.windows(3).any(|w| w == b"~HS") {
            if let Err(e) = wr.write_all(HS_PLACEHOLDER).await {
                eprintln!("tcp: write error: {e}");
                break;
            }
        }

        state.counters.bytes.fetch_add(n as u64, Ordering::Relaxed);
        let chunk_start = input.len();
        input.extend_from_slice(chunk);
        if input.len() > MAX_STREAM_BYTES {
            eprintln!("tcp: input cap ({MAX_STREAM_BYTES} bytes) exceeded; closing connection");
            break;
        }

        let mut labels: Vec<ParsedLabel> = Vec::new();
        {
            let mut guard = state.memory.lock().await;
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
            }
            for pl in &labels {
                let raw = &input[pl.start..pl.end];
                for d in &diags.items {
                    eprintln!("tcp: {}", d.message);
                }
                let diag_count = diags.items.len();
                // Render inside its own scope: RenderOptions borrows a
                // `&dyn FontProvider`, which is not Sync, so `opts` must
                // not live across the archive await below.
                let png = {
                    let fonts = DefaultFontProvider::new();
                    let opts = RenderOptions {
                        dpi: DPI,
                        fonts: &fonts,
                        memory: &guard,
                        mode: &persist.mode,
                    };
                    match render(&pl.label, &opts) {
                        Ok(o) => o.bitmap.to_png(),
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
                match archive.push(&mut row, raw, &pl.label, &png) {
                    Ok(_id) => {
                        state.counters.labels.fetch_add(1, Ordering::Relaxed);
                        state.counters.warnings.fetch_add(diag_count as u64, Ordering::Relaxed);
                    }
                    Err(e) => eprintln!("tcp: archive push failed: {e}"),
                }
                drop(archive);
                diags.items.clear(); // diags are attributed to this label
            }
        }

        // Free everything before the last completed label: spans are only
        // read during the loop above, and a partial format started before
        // the drain is fine (its ^XA offset -> format_start is refreshed by
        // any later ^XA; the next chunk's token offsets are re-based below).
        if let Some(last) = labels.last() {
            input.drain(..last.end);
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
