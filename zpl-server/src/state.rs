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

/// Printhead temperature ladder. `OverTemp` suppresses the ~HS response
/// entirely (guide p.233 "HEAD OVER-TEMPERATURE"); in a response the `l`
/// bit (String 1 field 12) is the only elevated-temperature flag, so a
/// delivered response with `l=1` unambiguously means `Warm` (see
/// `hs.rs`). The `k` (under-temperature) bit has no state to map - the
/// ladder has no "cold" rung - and stays clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Temp {
    Normal,
    Warm,
    OverTemp,
}

/// Media type for the `mmm` bit 7 (media-type) flag (guide p.235).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaType {
    DieCut,
    Continuous,
}

/// Print technology for the `mmm` bit 0 and the `q` thermal-transfer flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintMode {
    DirectThermal,
    ThermalTransfer,
}

/// Immediately observable hardware conditions. Defaults: a healthy,
/// powered-on printer with media loaded, head down, and a normal
/// temperature head.
#[derive(Clone, Debug)]
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
