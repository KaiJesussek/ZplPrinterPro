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
    _ratio: u32,
    _height: u32,
    _line: u8,
) -> Result<Pattern, String> {
    match symbology {
        b'C' => bc128::encode(data, module),
        _ => Err(format!(
            "unsupported symbology '{}' (Task 15 registers ^B1..^BE)",
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
    let data = prepare_ucc(f, data);
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
fn ucc_mod10(data: &[u8]) -> Option<u8> {
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
    });
    if let Some(r) = rotation {
        d.rotation = r;
    }
}
