//! Barcode framework: the `Pattern`/`Bar` IR the renderer (Task 14) draws,
//! the plan's `encode`/`barcode_field` entries, and the `^BY`/`^BC`
//! handlers. Symbology encoders are pure data (bar widths / module maps)
//! with no rendering; Task 15's `^B1..^BE` and Task 16's 2D symbologies
//! register through the same entry points.
//!
//! Defaults are the guide's (^BY p.148): module width 1..10 power-up **2**,
//! wide/narrow ratio 2.0..3.0 in 0.1 steps power-up **3.0** (stored in
//! tenths, 3.0 -> 30), height power-up **10**.

use crate::bc128;
use crate::bc1d;
use crate::config::{to_tenths, u32_in};
use crate::diag::{Diagnostic, Diagnostics};
use crate::ir::BarcodeField;
use crate::state::{BarcodeDraft, ParseContext};
use crate::textfmt::{draft_mut, parse_rotation, split_commas};

/// The run-length bar pattern a 1D encoder produces. `width` is in dots
/// (element width x the ^BY module width), `black` alternates starting
/// black; the renderer paints horizontal bars of the field's height.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    pub width: u32,
    pub black: bool,
}

/// What an encoder produces: 1D bar runs, or a 2D module map (1bpp,
/// MSB-first rows; Task 16's QR/DataMatrix/Aztec/PDF417).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Bars(Vec<Bar>),
    Modules { width: u32, height: u32, data: Vec<u8> },
}

/// Encode a barcode from raw field data (^FH already applied; for ^BC the
/// UCC/EAN-128 preparation of [`barcode_field`] must run first - `encode`
/// itself is the raw core). Unknown symbologies and empty data are errors,
/// never panics; `module`/`ratio`/`height`/`line` are the ^BY/^BC values
/// (Code 128 is fixed-ratio, so `ratio` is ignored by it).
pub fn encode(
    symbology: u8,
    data: &[u8],
    module: u32,
    ratio: u32,
    _height: u32,
    _line: u8,
) -> Result<Pattern, String> {
    match symbology {
        b'C' => bc128::encode(data, module),
        // Task 15: the remaining 1D symbologies (mnemonics per the guide).
        b'1' => bc1d::code11(data, module, ratio),
        b'2' => bc1d::i2of5(data, module, ratio),
        b'3' => bc1d::code39(data, module, ratio),
        b'5' => bc1d::planet(data, module),
        b'8' => bc1d::ean8(data, module),
        b'9' => bc1d::upce(data, module),
        b'A' => bc1d::code93(data, module),
        b'E' => bc1d::ean13(data, module),
        b'K' => bc1d::codabar(data, module, ratio),
        b'M' => bc1d::msi(data, module, ratio),
        b'U' => bc1d::upca(data, module),
        _ => Err(format!(
            "unsupported symbology '{}'",
            symbology as char
        )),
    }
}

/// The renderer-facing entry: resolve defaults and turn the parsed field
/// into a [`Pattern`]. Errors (empty data, unknown symbology) degrade to an
/// empty pattern - the parser already warned where it can (^FS with no
/// data), so this never panics and never fails.
pub fn barcode_field(f: &BarcodeField) -> Pattern {
    // The IR keeps barcode data as a Latin-1 view of the source bytes
    // (Task 12 upgrades the codepage decode); recover the bytes through
    // Latin-1, NOT `str::as_bytes` - that would UTF-8-re-encode bytes >=
    // 0x80 into two symbols and corrupt the barcode (a byte-value
    // symbology encodes the original byte).
    let data = latin1_bytes(&f.data);
    if data.is_empty() {
        return Pattern::Bars(Vec::new());
    }
    // ^BY defaults are resolved at parse time; zeros here mean a field that
    // predates the ^BY wiring - fall back to the guide's power-up values.
    let module = if f.module == 0 { 2 } else { f.module };
    let ratio = if f.ratio == 0 { 30 } else { f.ratio };
    let height = if f.height == 0 { 10 } else { f.height };
    let data = if f.symbology == b'C' {
        prepare_ucc(f, data)
    } else {
        // Task 15 1D symbologies fold their options (^B1 count, ^B2
        // check, ^B3 expansion+check, ^BK start/stop, ^BM mode) into the
        // data; the encoders stay option-free.
        bc1d::prepare(f, &data)
    };
    match encode(f.symbology, &data, module, ratio, height, f.line) {
        Ok(pattern) => pattern,
        Err(_) => Pattern::Bars(Vec::new()),
    }
}

/// The IR's `String` holds each source byte as its Latin-1 code point;
/// map back 1:1. Chars beyond U+00FF (only reachable once Task 12 decodes
/// real codepages) truncate - 1D symbologies encode byte values, the known
/// Zebra lossiness for non-Latin-1 content.
fn latin1_bytes(data: &str) -> Vec<u8> {
    data.chars().map(|c| c as u32 as u8).collect()
}

/// ^BC UCC/EAN-128 data preparation, per the guide's ^BC page: `e=Y` adds
/// the mod-10 UCC check digit (the mod-103 check is always computed by the
/// encoder), `m=U` pads/trims to 19 digits (UCC case mode), `m=D`
/// strips parentheses and spaces and prepends FNC1 (`>8`, the guide's
/// invocation). Combinations (`e`+`D`) are mutually exclusive on real
/// firmware; each step applies where its data shape allows.
fn prepare_ucc(f: &BarcodeField, data: Vec<u8>) -> Vec<u8> {
    let mut data = data;
    if f.model == b'D' {
        // Guide: "The printer automatically strips out parentheses and
        // spaces for encoding" and "starts in the appropriate subset
        // followed by FNC1".
        data.retain(|&b| b != b'(' && b != b')' && b != b' ');
        if !data.is_empty() {
            let mut with_fnc1 = Vec::with_capacity(data.len() + 2);
            with_fnc1.extend_from_slice(b">8");
            with_fnc1.append(&mut data);
            data = with_fnc1;
        }
    }
    if f.ucc {
        if let Some(check) = ucc_mod10(&data) {
            data.push(b'0' + check);
        }
    }
    if f.model == b'U' {
        ucc_case_19(&mut data);
    }
    data
}

/// EAN-style mod-10 check digit over a digit string (weights 3,1,3,1,...
/// from the rightmost digit; append the digit that makes the weighted sum
/// divisible by 10). `None` when the data is not all digits - the check
/// digit is only defined for numeric UCC/EAN-128 content.
pub(crate) fn ucc_mod10(data: &[u8]) -> Option<u8> {
    if data.is_empty() || !data.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut sum = 0u32;
    let mut weight = 3;
    for &b in data.iter().rev() {
        sum += (b - b'0') as u32 * weight;
        weight = if weight == 3 { 1 } else { 3 };
    }
    Some(((10 - sum % 10) % 10) as u8)
}

/// `m=U` (UCC case mode, guide): "More than 19 digits in ^FD are
/// eliminated. Fewer than 19 digits add zeros to the right to bring the
/// count to 19." Applies to all-digit data; non-digit content is left
/// alone (the legacy case mode is for 19-digit numeric application
/// identifiers).
fn ucc_case_19(data: &mut Vec<u8>) {
    if data.is_empty() || !data.iter().all(u8::is_ascii_digit) {
        return;
    }
    if data.len() > 19 {
        data.truncate(19);
    } else {
        data.resize(19, b'0');
    }
}

/// `^BYw,r,h` (guide p.148): default module width (1..10, power-up 2),
/// wide/narrow ratio (2.0..3.0 in 0.1 steps, power-up 3.0 - stored in
/// tenths), height (power-up 10) for subsequent barcodes. Missing
/// parameters keep the current value; garbage warns and keeps it too.
pub(crate) fn bar_code_defaults(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let prev = (ctx.mode.by_module, ctx.mode.by_ratio, ctx.mode.by_height);
    ctx.mode.by_module = u32_in(
        parts.first().copied(),
        "BY",
        "module width",
        ctx.diags,
        1,
        10,
        prev.0,
    );
    ctx.mode.by_ratio = match parts.get(1).and_then(|p| to_tenths(p)) {
        Some(v) if (20..=30).contains(&v) => v as u32,
        Some(_) => {
            warn_invalid("BY", "ratio", parts.get(1).copied().unwrap_or(&[]), ctx.diags);
            prev.1
        }
        None => prev.1, // empty segment: keep current
    };
    ctx.mode.by_height = u32_in(
        parts.get(2).copied(),
        "BY",
        "height",
        ctx.diags,
        1,
        32000,
        prev.2,
    );
}

/// Parse a Y/N letter parameter (case-insensitive); garbage warns and falls
/// back, an empty segment silently keeps `fallback`.
fn yn(seg: Option<&[u8]>, cmd: &str, name: &str, diags: &mut Diagnostics, fallback: u8) -> u8 {
    match seg.and_then(|p| p.first()).copied() {
        Some(b'Y') | Some(b'y') => b'Y',
        Some(b'N') | Some(b'n') => b'N',
        Some(_) => {
            warn_invalid(cmd, name, seg.unwrap_or(&[]), diags);
            fallback
        }
        None => fallback,
    }
}

fn warn_invalid(cmd: &str, name: &str, raw: &[u8], diags: &mut Diagnostics) {
    diags.push(Diagnostic::warning(
        cmd,
        format!("invalid {name} '{}'; using default", String::from_utf8_lossy(raw)),
    ));
}

/// `^BCo,h,f,g,e,m` (guide p.94-95): Code 128. o = orientation N/R/I/B
/// (default: current ^FW), h = height 1..32000 (default: ^BY height),
/// f = print interpretation line Y/N (default Y), g = line above code Y/N
/// (default N), e = UCC check digit Y/N (default N), m = mode N/U/A/D
/// (default N). Opens a barcode field draft; ^FD data and ^FS close it.
pub(crate) fn code_128(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    // Snapshot the ^BY defaults before borrowing the draft.
    let (by_module, by_ratio, by_height) = (ctx.mode.by_module, ctx.mode.by_ratio, ctx.mode.by_height);
    let rotation = match parts.first().copied() {
        Some(o) if o.is_empty() => None, // default: current ^FW
        Some(o) => match parse_rotation(o) {
            Some(r) => Some(r),
            None => {
                warn_invalid("BC", "orientation", o, ctx.diags);
                None
            }
        },
        None => None,
    };
    let height = u32_in(
        parts.get(1).copied(),
        "BC",
        "height",
        ctx.diags,
        1,
        32000,
        by_height,
    );
    let line = yn(parts.get(2).copied(), "BC", "interpretation line", ctx.diags, b'Y');
    let interpret = yn(parts.get(3).copied(), "BC", "line above code", ctx.diags, b'N') == b'Y';
    let ucc = yn(parts.get(4).copied(), "BC", "UCC check digit", ctx.diags, b'N') == b'Y';
    let model = match parts.get(5).and_then(|p| p.first()).copied() {
        Some(b'N') | Some(b'n') => b'N',
        Some(b'U') | Some(b'u') => b'U',
        Some(b'A') | Some(b'a') => b'A',
        Some(b'D') | Some(b'd') => b'D',
        Some(_) => {
            warn_invalid("BC", "mode", parts.get(5).copied().unwrap_or(&[]), ctx.diags);
            b'N'
        }
        None => b'N',
    };
    let Some(d) = draft_mut(ctx) else {
        return; // no format open: ^BC outside ^XA..^XZ is a no-op
    };
    d.barcode = Some(BarcodeDraft {
        symbology: b'C',
        module: by_module,
        ratio: by_ratio,
        height,
        line,
        interpret,
        ucc,
        model,
        check: b'N',
        start: b'A',
        stop: b'A',
    });
    if let Some(r) = rotation {
        d.rotation = r;
    }
}


// --- Task 15 1D barcode handlers (^B1..^B9, ^BA, ^BK, ^BE, ^BU, ^BM) ----
//
// Parameter shapes transcribed from the guide: ^B1 o,e,h,f,g (p.66);
// ^B2 o,h,f,g,e (p.68); ^B3 o,e,h,f,g (p.70); ^B8 o,h,f,g (p.83); ^B9
// o,h,f,g,e (p.85); ^BA o,h,f,g,e (p.87); ^BK o,e,h,f,g,k,l (p.118); ^BE
// o,h,f,g (p.109); ^BU o,h,f,g,e (p.142); ^BM o,e,h,f,g,e2 (p.122; e2
// affects only the interpretation line, no bar effect, accepted).
//
// ^B1..^B9 scan as mnemonic `B ` (the scanner keeps single-letter
// mnemonics short) with the symbology digit as the first parameter
// segment, so one dispatch row feeds `barcode_digit`, which re-slices the
// parameter list past the digit and dispatches per symbology.

/// Parameter layout of a Task 15 ^B* command.
struct Spec {
    /// Check-digit parameter position and kind.
    check: Option<(Check, usize)>,
    /// Height position and maximum (^B5/^BU cap at 9999, the rest at 32000).
    height: (usize, u32),
    /// `f` (interpretation line below) position and default (^B5 defaults
    /// to N; the other 1D symbologies default to Y).
    line: (usize, u8),
    /// `g` (line above) position.
    above: usize,
    /// ^BK k/l start/stop positions.
    start: Option<usize>,
    stop: Option<usize>,
}

/// How each symbology's check-digit parameter behaves.
#[derive(Clone, Copy)]
enum Check {
    /// ^B2 e: append a Mod-10 check digit (carried as `ucc`).
    Ucc,
    /// ^B3 e: append the Mod-43 check digit (Y/N).
    Mod43,
    /// ^B1 e: Y = one check digit, N = two (carried as 1/2).
    DigitCount,
    /// ^BM e: A/B/C/D check-digit mode.
    Mode,
    /// ^BK e: fixed N (parsed for diagnostics, then ignored).
    FixedN,
    /// ^B9/^BU/^BA e "print check digit": the bars always carry the
    /// standard's check digit; parsed for diagnostics, no bar effect.
    PrintDigit(u8),
}

const SPEC_B1: Spec = Spec { check: Some((Check::DigitCount, 1)), height: (2, 32000), line: (3, b'Y'), above: 4, start: None, stop: None };
const SPEC_B2: Spec = Spec { check: Some((Check::Ucc, 4)), height: (1, 32000), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_B3: Spec = Spec { check: Some((Check::Mod43, 1)), height: (2, 32000), line: (3, b'Y'), above: 4, start: None, stop: None };
const SPEC_B5: Spec = Spec { check: None, height: (1, 9999), line: (2, b'N'), above: 3, start: None, stop: None };
const SPEC_B8: Spec = Spec { check: None, height: (1, 32000), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_B9: Spec = Spec { check: Some((Check::PrintDigit(b'Y'), 4)), height: (1, 32000), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_BA: Spec = Spec { check: Some((Check::PrintDigit(b'N'), 4)), height: (1, 32000), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_BK: Spec = Spec { check: Some((Check::FixedN, 1)), height: (2, 32000), line: (3, b'Y'), above: 4, start: Some(5), stop: Some(6) };
const SPEC_BE: Spec = Spec { check: None, height: (1, 32000), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_BU: Spec = Spec { check: Some((Check::PrintDigit(b'Y'), 4)), height: (1, 9999), line: (2, b'Y'), above: 3, start: None, stop: None };
const SPEC_BM: Spec = Spec { check: Some((Check::Mode, 1)), height: (2, 32000), line: (3, b'Y'), above: 4, start: None, stop: None };

/// ^B1..^B9: the symbology digit is the first parameter segment.
pub(crate) fn barcode_digit(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let Some(first) = parts.first() else {
        ctx.diags
            .push(Diagnostic::warning("B*", "barcode command is missing the symbology digit"));
        return;
    };
    let Some(sym) = first.first().copied() else {
        ctx.diags
            .push(Diagnostic::warning("B*", "barcode command is missing the symbology digit"));
        return;
    };
    // The symbology's own parameters start after the digit. The digit is
    // glued to whatever follows ("^B2N,20" -> "2N,20"), so only the first
    // byte is consumed; a comma after the digit stays in the parameter
    // list ("^B8,30" -> "8,30" -> ",30" -> the empty orientation segment
    // falls back to the current ^FW).
    let rest = if params.len() > 1 { &params[1..] } else { &[] };
    match sym {
        b'1' => barcode_1d(ctx, rest, "B1", b'1', SPEC_B1),
        b'2' => barcode_1d(ctx, rest, "B2", b'2', SPEC_B2),
        b'3' => barcode_1d(ctx, rest, "B3", b'3', SPEC_B3),
        b'5' => barcode_1d(ctx, rest, "B5", b'5', SPEC_B5),
        b'8' => barcode_1d(ctx, rest, "B8", b'8', SPEC_B8),
        b'9' => barcode_1d(ctx, rest, "B9", b'9', SPEC_B9),
        _ => {
            // ^B0 Aztec, ^B4 Code 49, ^B6, ^B7 PDF417: 2D or stacked
            // symbologies outside the 1D scope (Task 16 owns the 2D
            // mnemonics) - warn and skip, never a silent empty field.
            ctx.diags.push(Diagnostic::warning(
                "B*",
                format!(
                    "symbology ^B{} is not implemented by this build; field skipped",
                    sym as char
                ),
            ));
        }
    }
}

/// Shared parameter parsing for the Task 15 1D symbologies.
fn barcode_1d(ctx: &mut ParseContext, params: &[u8], cmd: &str, symbology: u8, spec: Spec) {
    let parts = split_commas(params);
    let (by_module, by_ratio, by_height) = (ctx.mode.by_module, ctx.mode.by_ratio, ctx.mode.by_height);
    let rotation = match parts.first().copied() {
        Some(o) if o.is_empty() => None, // default: current ^FW
        Some(o) => match parse_rotation(o) {
            Some(r) => Some(r),
            None => {
                warn_invalid(cmd, "orientation", o, ctx.diags);
                None
            }
        },
        None => None,
    };
    let height = u32_in(
        parts.get(spec.height.0).copied(),
        cmd,
        "height",
        ctx.diags,
        1,
        spec.height.1,
        by_height,
    );
    let line = yn(parts.get(spec.line.0).copied(), cmd, "interpretation line", ctx.diags, spec.line.1);
    let interpret = yn(parts.get(spec.above).copied(), cmd, "line above code", ctx.diags, b'N') == b'Y';
    let mut check = b'N';
    let mut ucc = false;
    let mut start = b'A';
    let mut stop = b'A';
    if let Some((kind, pos)) = spec.check {
        match kind {
            Check::Ucc => {
                ucc = yn(parts.get(pos).copied(), cmd, "Mod-10 check digit", ctx.diags, b'N') == b'Y'
            }
            Check::Mod43 => check = yn(parts.get(pos).copied(), cmd, "Mod-43 check digit", ctx.diags, b'N'),
            Check::DigitCount => {
                check = if yn(parts.get(pos).copied(), cmd, "check digit count", ctx.diags, b'N') == b'Y' {
                    1
                } else {
                    2
                }
            }
            Check::Mode => {
                check = match parts.get(pos).and_then(|p| p.first()).copied() {
                    Some(m @ (b'A' | b'B' | b'C' | b'D')) => m,
                    _ => {
                        if parts.get(pos).is_some_and(|p| !p.is_empty()) {
                            warn_invalid(cmd, "check digit mode", parts.get(pos).copied().unwrap_or(&[]), ctx.diags);
                        }
                        b'B'
                    }
                };
            }
            Check::FixedN => {
                let _ = yn(parts.get(pos).copied(), cmd, "check digit", ctx.diags, b'N');
            }
            Check::PrintDigit(dflt) => {
                let _ = yn(parts.get(pos).copied(), cmd, "print check digit", ctx.diags, dflt);
            }
        }
    }
    if let Some(p) = spec.start {
        start = abcd(parts.get(p).copied(), cmd, "start character", ctx.diags);
    }
    if let Some(p) = spec.stop {
        stop = abcd(parts.get(p).copied(), cmd, "stop character", ctx.diags);
    }
    let Some(d) = draft_mut(ctx) else {
        return; // no format open: a bare ^B outside ^XA..^XZ is a no-op
    };
    d.barcode = Some(BarcodeDraft {
        symbology,
        module: by_module,
        ratio: by_ratio,
        height,
        line,
        interpret,
        ucc,
        model: b'N',
        check,
        start,
        stop,
    });
    if let Some(r) = rotation {
        d.rotation = r;
    }
}

/// ^BK k/l: A-D characters (guide p.118, default A).
fn abcd(seg: Option<&[u8]>, cmd: &str, name: &str, diags: &mut Diagnostics) -> u8 {
    match seg.and_then(|p| p.first()).copied() {
        Some(c @ (b'A' | b'B' | b'C' | b'D')) => c,
        _ => {
            if seg.is_some_and(|p| !p.is_empty()) {
                warn_invalid(cmd, name, seg.unwrap_or(&[]), diags);
            }
            b'A'
        }
    }
}

pub(crate) fn code93_field(ctx: &mut ParseContext, params: &[u8]) {
    barcode_1d(ctx, params, "BA", b'A', SPEC_BA);
}
pub(crate) fn codabar_field(ctx: &mut ParseContext, params: &[u8]) {
    barcode_1d(ctx, params, "BK", b'K', SPEC_BK);
}
pub(crate) fn ean13_field(ctx: &mut ParseContext, params: &[u8]) {
    barcode_1d(ctx, params, "BE", b'E', SPEC_BE);
}
pub(crate) fn upca_field(ctx: &mut ParseContext, params: &[u8]) {
    barcode_1d(ctx, params, "BU", b'U', SPEC_BU);
}
pub(crate) fn msi_field(ctx: &mut ParseContext, params: &[u8]) {
    barcode_1d(ctx, params, "BM", b'M', SPEC_BM);
}

