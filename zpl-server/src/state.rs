//! Server-side printer state (Task 20): the mutable snapshot the protocol
//! responders read and the Task 21 web API mutates. This is the *printer*,
//! not the parser: parse state (^LL, ^PQ, open formats) is driven into it
//! by the TCP session at label completions, and everything else is a plain
//! `pub` field any API can flip (paper out, pause, temperature, ...).
//!
//! Locking: `AppState::printer_state` is a `Mutex<PrinterState>`. The TCP
//! session takes it while holding the memory guard (memory -> printer_state
//! order); Task 21's HTTP tasks must use the same order (they hold the
//! memory lock for rerenders, so taking printer_state inside it is free).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use zpl::memory::Memory;

use crate::archive::Archive;
use crate::archive::LabelRow;

/// Printhead temperature ladder. `OverTemp` suppresses the ~HS response
/// entirely (guide p.233 "HEAD OVER-TEMPERATURE"); in a response the `l`
/// bit (String 1 field 12) is the only elevated-temperature flag, so a
/// delivered response with `l=1` unambiguously means `Warm` (see
/// `hs.rs`). The `k` (under-temperature) bit has no state to map - the
/// ladder has no "cold" rung - and stays clear.
///
/// Serde: the Task 21 API exposes/validates these values as strings
/// (`"Normal" | "Warm" | "OverTemp"`); an unknown string 400s on PUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Temp {
    Normal,
    Warm,
    OverTemp,
}

/// Media type for the `mmm` bit 7 (media-type) flag (guide p.235).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MediaType {
    DieCut,
    Continuous,
}

/// Print technology for the `mmm` bit 0 and the `q` thermal-transfer flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PrintMode {
    DirectThermal,
    ThermalTransfer,
}

/// Immediately observable hardware conditions. Defaults: a healthy,
/// powered-on printer with media loaded, head down, and a normal
/// temperature head.
///
/// Serde: the wire format and the persisted `dir/state.json`. Field names
/// are the API's JSON keys; enum fields use the variant names above.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Conditions {
    pub paper_out: bool,
    pub ribbon_out: bool,
    pub head_open: bool,
    pub rewind_full: bool,
    pub head_temp: Temp,
    pub pause: bool,
    pub comm_diag: bool,
    pub media_type: MediaType,
    pub print_mode: PrintMode,
    /// Static-RAM installed flag - String 3's `y` (0 = not installed).
    pub static_ram: bool,
}

impl Default for Conditions {
    fn default() -> Self {
        Conditions {
            paper_out: false,
            ribbon_out: false,
            head_open: false,
            rewind_full: false,
            head_temp: Temp::Normal,
            pause: false,
            comm_diag: false,
            media_type: MediaType::DieCut,
            print_mode: PrintMode::DirectThermal,
            static_ram: true,
        }
    }
}

/// The full printer snapshot behind ~HS (and the Task 21 API).
///
/// The four derived fields (`label_length_dots`, `formats_in_buffer`,
/// `partial_format`, `labels_remaining`) are updated by the TCP session as
/// labels complete; the rest are free `pub` state. Field names carry the
/// ~HS response fields they feed (see the byte-exact serializer in
/// `hs.rs`).
#[derive(Clone, Debug)]
pub struct PrinterState {
    pub conditions: Conditions,
    /// `dddd` - label length in dots, from the last printed label's
    /// rendered height (a real printer's sensor reading; 0 = none yet).
    pub label_length_dots: u32,
    /// `eee` - formats in the receive buffer. The emulator prints every
    /// completed format synchronously, so this is always 0; kept as a
    /// `pub` field because the Task 21 API may eventually set it.
    pub formats_in_buffer: u32,
    /// `h` - a format is open and not yet closed by ^XZ.
    pub partial_format: bool,
    /// `f` - receive-buffer full flag.
    pub buffer_full: bool,
    /// `j` - corrupt-RAM flag ("configuration data lost").
    pub corrupt_ram: bool,
    /// `uuuuuuuu` - copies of queued formats still to print. The session
    /// adds each completed format's `^PQ` copies and consumes one per
    /// printed copy (v1 renders each format once, so a ^PQn batch leaves
    /// n-1 outstanding).
    pub labels_remaining: u32,
    /// `r` - print-mode code (0 Rewind, 1 Peel-Off, 2 Tear-Off, 3 Cutter,
    /// 4 Applicator, 5 Delayed cut, 6 Linerless Peel, 7 Linerless Rewind,
    /// 8 Partial Cutter, 9 RFID, K Kiosk, A Kiosk CutStream). Default 2 =
    /// Tear-Off, the standard factory mode.
    pub print_mode_code: u8,
    /// `s` - print-width mode (0 = printer width, 1 = label width).
    pub print_width_mode: u8,
    /// `t` - label-waiting flag (1 = label waiting in Peel-Off mode).
    pub label_waiting: bool,
    /// `~HQOD` - non-resettable print meter (labels printed this boot).
    pub meter_total: u64,
    /// `~HQOD` - user-resettable meter 1 (Task 21 may reset these).
    pub meter_cntr1: u64,
    /// `~HQOD` - user-resettable meter 2.
    pub meter_cntr2: u64,
    /// `^FN#` -> data of the last completed label; the `^HV` responder
    /// reads it (empty until the first label with numbered fields).
    pub numbered: BTreeMap<u32, String>,
}

impl Default for PrinterState {
    fn default() -> Self {
        PrinterState {
            conditions: Conditions::default(),
            label_length_dots: 0,
            formats_in_buffer: 0,
            partial_format: false,
            buffer_full: false,
            corrupt_ram: false,
            labels_remaining: 0,
            print_mode_code: 2, // Tear-Off
            print_width_mode: 0,
            label_waiting: false,
            meter_total: 0,
            meter_cntr1: 0,
            meter_cntr2: 0,
            numbered: BTreeMap::new(),
        }
    }
}

/// Accumulated session counters, reset at boot (Task 20's `~HS` and Task
/// 21's `/api/diagnostics` read them; the TCP session bumps them).
#[derive(Debug, Default)]
pub struct Counters {
    /// Completed labels archived since boot.
    pub labels: AtomicU64,
    /// Raw TCP bytes received since boot.
    pub bytes: AtomicU64,
    /// Diagnostics generated by labels since boot (any severity).
    pub warnings: AtomicU64,
    /// `Severity::UnknownCommand` diagnostics since boot (the
    /// `/api/diagnostics` headline number).
    pub unknown_commands: AtomicU64,
}

/// One SSE push from server to all `/api/events` subscribers (Task 21).
/// The wire format is `event: label|state` + JSON data - see `http.rs`.
#[derive(Clone, Debug)]
pub enum Event {
    /// A new archived label; data is the `LabelRow` (same shape as the
    /// `/api/labels` list rows, so the UI prepends it unchanged).
    Label(LabelRow),
    /// Printer conditions changed (a `PUT /api/printer-state`); data is
    /// the merged `Conditions` JSON.
    State(Conditions),
}

/// Server-wide state shared by TCP and HTTP tasks. Single source of truth
/// for the printer: `printer_state` is the state `~HS` reads, the `Memory`
/// the parse stores into, and the `archive` every completed label lands
/// in. Lock order is memory -> printer_state (the TCP session takes
/// printer_state while holding the memory guard; Task 21's rerender holds
/// the memory lock too, so taking printer_state inside it is free). The
/// archive mutex is independent (never held while taking memory or
/// printer_state).
pub struct AppState {
    pub archive: Mutex<Archive>,
    /// The printer's storage, global like a real printer's: an image stored
    /// by one connection is visible to the next. Locked per parse chunk -
    /// stored data lives in `Memory` itself, so per-chunk locking changes
    /// nothing semantically.
    pub memory: Arc<Mutex<Memory>>,
    pub counters: Counters,
    /// The printer's live state (~HS conditions, batch remaining, meters,
    /// ^FN data). Mutated by the session at label completions and by the
    /// Task 21 web API; read by every query responder.
    pub printer_state: Mutex<PrinterState>,
    /// SSE fan-out; the TCP session sends `Label` events on archive, the
    /// HTTP PUT sends `State` events.
    pub events: broadcast::Sender<Event>,
    /// Where `memory.json` / `state.json` / `index.json` live (the archive
    /// dir; debounced by `http::spawn_persistence`).
    pub persist_dir: PathBuf,
}

/// SSE buffer per subscriber; a slow client that lags behind is dropped
/// from the stream (the browser reconnects - the UI repaints from GETs).
const EVENT_CAP: usize = 128;

impl AppState {
    /// Fresh (or boot-reloaded) server state around an opened archive.
    /// `dir/state.json` (Conditions) and `dir/memory.json` (Memory) are
    /// loaded best-effort - an unreadable or missing file logs and falls
    /// back to defaults, never a boot failure.
    pub fn new(archive: Archive) -> Self {
        let persist_dir = archive.dir.clone();

        let mut conditions = Conditions::default();
        match fs::read(persist_dir.join("state.json")) {
            Ok(bytes) => match serde_json::from_slice::<Conditions>(&bytes) {
                Ok(c) => conditions = c,
                Err(e) => eprintln!("state: ignoring unreadable {}: {e}", persist_dir.join("state.json").display()),
            },
            Err(_) => {} // first boot
        }

        let mut memory = Memory::default();
        match fs::read(persist_dir.join("memory.json")) {
            Ok(bytes) => match serde_json::from_slice::<Memory>(&bytes) {
                Ok(m) => memory = m,
                Err(e) => eprintln!("state: ignoring unreadable {}: {e}", persist_dir.join("memory.json").display()),
            },
            Err(_) => {} // first boot
        }

        let mut printer = PrinterState::default();
        printer.conditions = conditions;
        let (events, _) = broadcast::channel(EVENT_CAP);
        AppState {
            archive: Mutex::new(archive),
            memory: Arc::new(Mutex::new(memory)),
            counters: Counters::default(),
            printer_state: Mutex::new(printer),
            events,
            persist_dir,
        }
    }
}
