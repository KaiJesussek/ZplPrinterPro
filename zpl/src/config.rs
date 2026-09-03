//! Handlers for the label-configuration command set: `^LH ^LL ^PW ^PO ^PM
//! ^LS ^LT ^MD ^PN ^PQ ^PR ^PH ^MN ^MT ^MM ^MU ^MF ^MI ^ML ^MP ^MW ^PA ^PF
//! ^SC ^SZ ^ZZ ^XB ^XS ^JZ`, plus the tilde forms `~SD` (set darkness), `~PH`
//! (slew to home) and `~TA` (tear-off adjust).
//!
//! Label geometry lands in `PrinterMode` - the renderer consumes it - and
//! the few settings with real downstream consumers land in `PrinterConfig`
//! (darkness, media tracking, units, ZPL mode). The rest (`^PR ^MT ^MM ^MF
//! ^MI ^ML ^MP ^MW ^PA ^PF ^SC ^ZZ ^XB ^XS ^PN`) is parsed and validated but
//! not stored: the guide describes media/hardware actions that have no effect
//! on the label image, so a bitmap renderer has nowhere to put them.
//!
//! Every default below was verified against the ZPL II guide
//! (zpl-zbi2-pg-en.pdf) command pages; where the plan's sketches disagreed,
//! the guide won (see the task report).

use crate::diag::{Diagnostic, Diagnostics};
use crate::state::ParseContext;
use crate::textfmt::{split_commas, to_u32};

fn warn_ignored(cmd: &str, name: &str, raw: &[u8], diags: &mut Diagnostics) {
    diags.push(Diagnostic::warning(
        cmd,
        format!("invalid {name} '{}'; ignored", String::from_utf8_lossy(raw)),
    ));
}

/// Parse a signed decimal parameter: optional `+`/`-` then digits; `None`
/// for empty or malformed input (`^LS`, `^LT`, `^MD` are signed per guide).
fn to_i32(p: &[u8]) -> Option<i32> {
    let (sign, digits) = match p.first() {
        Some(b'-') => (-1, &p[1..]),
        Some(b'+') => (1, &p[1..]),
        _ => (1, p),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    core::str::from_utf8(digits).ok()?.parse::<i32>().ok().map(|v| sign * v)
}

/// Unsigned parameter confined to `[min, max]`. A missing/empty segment
/// silently takes `fallback` (ZPL's "no value -> keep/use default" rule);
/// garbage or out-of-range warns and falls back too - never panics.
/// Shared with the Task 9 barcode handlers (^BY/^BC ranges).
pub(crate) fn u32_in(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    min: u32,
    max: u32,
    fallback: u32,
) -> u32 {
    let Some(p) = seg else {
        return fallback;
    };
    if p.is_empty() {
        return fallback;
    }
    match to_u32(p) {
        Some(v) if (min..=max).contains(&v) => v,
        _ => {
            warn_ignored(cmd, name, p, diags);
            fallback
        }
    }
}

/// Signed counterpart of [`u32_in`].
fn i32_in(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    min: i32,
    max: i32,
    fallback: i32,
) -> i32 {
    let Some(p) = seg else {
        return fallback;
    };
    if p.is_empty() {
        return fallback;
    }
    match to_i32(p) {
        Some(v) if (min..=max).contains(&v) => v,
        _ => {
            warn_ignored(cmd, name, p, diags);
            fallback
        }
    }
}

/// Parse a signed decimal with at most one fractional digit into tenths
/// (`8.3` -> 83, `-6` -> -60). The guide's ^MD/~SD accept 0.1 steps on
/// XiIIIPlus-class printers; tenths keep those valid inputs warning-free
/// (the plan's integer sketch would have corrupted them). ^BY's ratio
/// (2.0..3.0 in 0.1 steps) uses the same arithmetic.
pub(crate) fn to_tenths(p: &[u8]) -> Option<i32> {
    let (sign, rest) = match p.first() {
        Some(b'-') => (-1, &p[1..]),
        Some(b'+') => (1, &p[1..]),
        _ => (1, p),
    };
    let (whole, frac) = match rest.iter().position(|&b| b == b'.') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => (rest, &[][..]),
    };
    if whole.is_empty() || !whole.iter().all(u8::is_ascii_digit) || frac.len() > 1 || !frac.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let w: i32 = core::str::from_utf8(whole).ok()?.parse().ok()?;
    let f: i32 = frac.first().map(|&d| (d - b'0') as i32).unwrap_or(0);
    // Checked: `w * 10` can overflow i32 (e.g. ^MD214748365); oversized input
    // returns None and the caller warns + falls back, never panics.
    Some(sign * (w.checked_mul(10)?.checked_add(f)?))
}

/// Darkness parameter (shared by ^MD and ~SD), parsed in tenths within
/// `[min, max]`; missing/empty silently keeps the setting.
fn tenths_in(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    min: i32,
    max: i32,
    fallback: i32,
) -> i32 {
    let Some(p) = seg else {
        return fallback;
    };
    if p.is_empty() {
        return fallback;
    }
    match to_tenths(p) {
        Some(v) if (min..=max).contains(&v) => v,
        _ => {
            warn_ignored(cmd, name, p, diags);
            fallback
        }
    }
}

/// First-letter parameter selector: `Some(uppercase)` when the parameter
/// starts with a letter from `allowed`, `None` silently for a missing/empty
/// parameter, and a warning + `None` for any other letter (the guide's
/// "command is ignored" behavior for invalid values). Case-insensitive,
/// matching the guide's "n or y is also accepted" convention.
fn pick_letter(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    allowed: &[u8],
) -> Option<u8> {
    let p = seg.unwrap_or(&[]);
    let Some(&c) = p.first() else {
        return None;
    };
    let up = c.to_ascii_uppercase();
    if allowed.contains(&up) {
        return Some(up);
    }
    diags.push(Diagnostic::warning(
        cmd,
        format!("invalid {name} '{c}'; ignored", c = c as char),
    ));
    None
}

/// Validate-only letter parameter: like [`pick_letter`] but the value is
/// discarded (commands whose setting has no render effect).
fn check_letters(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    allowed: &[u8],
) {
    let _ = pick_letter(seg, cmd, name, diags, allowed);
}

/// Validate-only numeric parameter in `[min, max]`; value discarded.
fn check_num(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    min: u32,
    max: u32,
) {
    let _ = u32_in(seg, cmd, name, diags, min, max, 0);
}

/// `^LHx,y`: label home position (x,y 0..32000, power-up 0). Offsets every
/// field origin by (lh_x, lh_y) at render time; fields keep their own
/// coordinates in the IR.
pub(crate) fn label_home(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.lh_x = u32_in(parts.first().copied(), "LH", "x", ctx.diags, 0, 32000, ctx.mode.lh_x);
    ctx.mode.lh_y = u32_in(parts.get(1).copied(), "LH", "y", ctx.diags, 0, 32000, ctx.mode.lh_y);
}

/// `^LLy[,x]`: label length (y 1..32000). 0 in the IR means AUTO - ^LL
/// omitted, the renderer sizes the label to its content (the guide's
/// continuous-media rule). The `x` parameter (N = continuous only / Y = all
/// media, default N) is validated and dropped: no IR slot.
pub(crate) fn label_length(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.ll_dots = u32_in(parts.first().copied(), "LL", "length", ctx.diags, 1, 32000, ctx.mode.ll_dots);
    check_letters(parts.get(1).copied(), "LL", "media scope", ctx.diags, b"YN");
}

/// `^PWa`: print width in dots (2..printer max). Power-up default is the
/// printer width - 832 dots for a 203-dpi 4-inch printer (the guide's
/// configuration-label example; the plan's 800 guess is superseded). Real
/// hardware clamps an oversized width; we accept the value as given.
pub(crate) fn print_width(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.pw_dots = u32_in(parts.first().copied(), "PW", "width", ctx.diags, 2, u32::MAX, ctx.mode.pw_dots);
}

/// `^POa`: print orientation - N = normal (0), I = invert 180deg (1). The
/// guide knows only these two values (the plan's N/I/B/A sketch is wrong);
/// missing/invalid is ignored, keeping the current setting.
pub(crate) fn print_orientation(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    match pick_letter(parts.first().copied(), "PO", "orientation", ctx.diags, b"NI") {
        Some(b'I') => ctx.mode.orientation = 1,
        Some(_) => ctx.mode.orientation = 0,
        None => {}
    }
}

/// `^PMa`: mirror image of the whole label - Y flips left-right, N normal
/// (default N); missing/invalid is ignored.
pub(crate) fn mirror_image(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    match pick_letter(parts.first().copied(), "PM", "mirror", ctx.diags, b"YN") {
        Some(b'Y') => ctx.mode.mirror = true,
        Some(_) => ctx.mode.mirror = false,
        None => {}
    }
}

/// `^LSa`: shift all field positions left by a dots (-9999..9999, power-up
/// 0) for Z-130 format compatibility. Renderer-consumed.
pub(crate) fn label_shift(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.left_shift =
        i32_in(parts.first().copied(), "LS", "shift", ctx.diags, -9999, 9999, ctx.mode.left_shift);
}

/// `^LTx`: label top offset (-120..120 on most printers), moving the whole
/// format up (negative) or down from the label's top edge. A value must be
/// specified or the command is ignored. Renderer-consumed.
pub(crate) fn label_top(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.label_top =
        i32_in(parts.first().copied(), "LT", "top", ctx.diags, -120, 120, ctx.mode.label_top);
}

/// `^MDa`: darkness adjustment relative to the current setting (-30..30,
/// power-up 0; "no value -> ignored"). Kept in tenths - with `~SD`'s 8.3 the
/// guide's 0.1 steps survive.
pub(crate) fn media_darkness(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let v = tenths_in(parts.first().copied(), "MD", "darkness", ctx.diags, -300, 300, 0);
    ctx.config.darkness += v;
}

/// `~SD##`: absolute darkness setting 00..30, stored in tenths (24 -> 240,
/// 8.3 -> 83). Tilde printer operation: handled any time, in or out of a
/// format. The guide's ^MD comment notes the two are additive; ~SD writes
/// the base.
pub(crate) fn set_darkness(ctx: &mut ParseContext, params: &[u8]) {
    ctx.config.darkness =
        tenths_in(split_commas(params).first().copied(), "SD", "darkness", ctx.diags, 0, 300, ctx.config.darkness);
}

/// `^PQq,p,r,o,e`: print quantity q (1..99,999,999, default 1). q sets
/// `Label.copies` (via `PrinterMode.copies`, which Task 20's labels-remaining
/// consumes too). The pause/replicate/override/cut params are accepted and
/// ignored until a batch engine exists - never warned on.
pub(crate) fn print_quantity(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    ctx.mode.copies = u32_in(parts.first().copied(), "PQ", "quantity", ctx.diags, 1, 99_999_999, ctx.mode.copies);
}

/// `^PRp,s,b`: print/slew/backfeed speeds (numeric 1-14, or letters A-E).
/// Hardware knobs with no label-image effect: validated, not stored.
pub(crate) fn print_rate(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    for (i, name) in ["print", "slew", "backfeed"].iter().enumerate() {
        let p = match parts.get(i).copied() {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };
        let valid = match to_u32(p) {
            Some(v) => (1..=14).contains(&v),
            None => matches!(p, [c] if matches!(c, b'A'..=b'E' | b'a'..=b'e')),
        };
        if !valid {
            warn_ignored("PR", name, p, ctx.diags);
        }
    }
}

/// `^PH` / `~PH`: feed one blank label after the current format prints.
/// No parameters, no state.
pub(crate) fn slew_to_home(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `^MNa,b`: media tracking - N continuous / Y,W non-continuous web / M mark
/// / A auto-detect / V continuous-variable-length. The letter is stored: V
/// (variable length) feeds Task 10's auto label height. The black-mark
/// offset `b` (-120..283 typical) is validated and dropped: no render effect.
pub(crate) fn media_tracking(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    if let Some(m) = pick_letter(parts.first().copied(), "MN", "media", ctx.diags, b"NYWMAV") {
        ctx.config.media_tracking = m;
    }
    let _ = i32_in(parts.get(1).copied(), "MN", "mark offset", ctx.diags, -120, 283, 0);
}

/// `^MTa`: media type - T thermal transfer / D direct thermal. Validated,
/// not stored.
pub(crate) fn media_type(ctx: &mut ParseContext, params: &[u8]) {
    check_letters(split_commas(params).first().copied(), "MT", "media type", ctx.diags, b"TD");
}

/// `^MMa,b`: print mode (T/P/R/A/C/D/F/L/U/K; b = pre-peel Y/N). Validated,
/// not stored - the plan's tear-off sketch has no consumer (media modes are
/// hardware behavior with no label-image effect).
pub(crate) fn print_mode(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_letters(parts.first().copied(), "MM", "mode", ctx.diags, b"TPRACDFLUK");
    check_letters(parts.get(1).copied(), "MM", "pre-peel", ctx.diags, b"YN");
}

/// `^MUa,b,c`: units (D/I/M, power-up D) plus optional dpi conversion
/// (base 150/200/300 -> desired 300/600). Units are stored for field scaling;
/// the dpi conversion is validated (loosely ranged - it is accept-only) and
/// dropped.
pub(crate) fn units(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    if let Some(u) = pick_letter(parts.first().copied(), "MU", "units", ctx.diags, b"DIM") {
        ctx.config.units = u;
    }
    check_num(parts.get(1).copied(), "MU", "format base", ctx.diags, 150, 300);
    check_num(parts.get(2).copied(), "MU", "desired dpi", ctx.diags, 300, 600);
}

/// `^MFp,h`: media feed action at power-up / after head close (F/C/L/N/S).
pub(crate) fn media_feed(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_letters(parts.first().copied(), "MF", "feed action", ctx.diags, b"FCLNS");
    check_letters(parts.get(1).copied(), "MF", "feed action", ctx.diags, b"FCLNS");
}

/// `^MItype,message`: maintenance alert message (type R/C; the message text
/// is dropped - no render effect).
pub(crate) fn maintenance_message(ctx: &mut ParseContext, params: &[u8]) {
    check_letters(split_commas(params).first().copied(), "MI", "alert type", ctx.diags, b"RC");
}

/// `^MLa`: maximum label length in dot rows (printer-dependent; validated,
/// not stored).
pub(crate) fn max_label_length(ctx: &mut ParseContext, params: &[u8]) {
    check_num(split_commas(params).first().copied(), "ML", "maximum length", ctx.diags, 1, u32::MAX);
}

/// `^MPa`: mode protection (D/P/C/E/S/W/F/X/M).
pub(crate) fn mode_protection(ctx: &mut ParseContext, params: &[u8]) {
    check_letters(split_commas(params).first().copied(), "MP", "mode", ctx.diags, b"DPCESWFXM");
}

/// `^MWa`: head cold warning enable (Y/N; no parameter -> instruction
/// ignored).
pub(crate) fn head_cold_warning(ctx: &mut ParseContext, params: &[u8]) {
    check_letters(split_commas(params).first().copied(), "MW", "warning", ctx.diags, b"YN");
}

/// `^PAa,b,c,d`: advanced text properties (default glyph, bidirectional
/// text, shaping, OpenType - each 0/1). Validated, not stored.
pub(crate) fn advanced_text(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    for (i, name) in ["default glyph", "bidirectional text", "shaping", "OpenType"].iter().enumerate() {
        check_num(parts.get(i).copied(), "PA", name, ctx.diags, 0, 1);
    }
}

/// `^PF#`: slew a given number of dot rows (0..32000) at high speed.
pub(crate) fn slew_rows(ctx: &mut ParseContext, params: &[u8]) {
    check_num(split_commas(params).first().copied(), "PF", "rows", ctx.diags, 0, 32000);
}

/// `^SCa,b,c,d,e,f`: serial communications (baud 110..115200, word 7/8,
/// parity N/E/O, stop 1/2, protocol X/D/R/M, Zebra protocol A/N/Z).
/// Validate-only - the baud value set is ranged, not enumerated (accept-only
/// command; no consumer).
pub(crate) fn serial_comms(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_num(parts.first().copied(), "SC", "baud", ctx.diags, 110, 115200);
    check_letters(parts.get(1).copied(), "SC", "word length", ctx.diags, b"78");
    check_letters(parts.get(2).copied(), "SC", "parity", ctx.diags, b"NEO");
    check_letters(parts.get(3).copied(), "SC", "stop bits", ctx.diags, b"12");
    check_letters(parts.get(4).copied(), "SC", "protocol", ctx.diags, b"XDRM");
    check_letters(parts.get(5).copied(), "SC", "zebra protocol", ctx.diags, b"ANZ");
}

/// `^SZa`: ZPL mode (1 = ZPL, 2 = ZPL II; power-up 2). Mode 1 selects the
/// legacy ZPL language, deliberately unimplemented here (the plan drops ZPL
/// I); the value is recorded either way.
pub(crate) fn zpl_mode(ctx: &mut ParseContext, params: &[u8]) {
    let v = u32_in(split_commas(params).first().copied(), "SZ", "mode", ctx.diags, 1, 2, ctx.config.zpl_mode as u32);
    ctx.config.zpl_mode = v as u8;
}

/// `^ZZt,b`: printer sleep (idle seconds 0..999999, shutdown label status
/// Y/N; battery printers only). Validated, not stored.
pub(crate) fn printer_sleep(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_num(parts.first().copied(), "ZZ", "idle time", ctx.diags, 0, 999_999);
    check_letters(parts.get(1).copied(), "ZZ", "label status", ctx.diags, b"YN");
}

/// `^XB`: suppress backfeed. No parameters.
pub(crate) fn suppress_backfeed(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `^XSlength,threshold,gain`: dynamic media calibration (three Y/N).
pub(crate) fn dynamic_calibration(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_letters(parts.first().copied(), "XS", "length", ctx.diags, b"YN");
    check_letters(parts.get(1).copied(), "XS", "threshold", ctx.diags, b"YN");
    check_letters(parts.get(2).copied(), "XS", "gain", ctx.diags, b"YN");
}

/// `^PNa`: presenter eject length 0..255 additional mm (KR403 present-now
/// action). Validated, not stored.
pub(crate) fn present_now(ctx: &mut ParseContext, params: &[u8]) {
    check_num(split_commas(params).first().copied(), "PN", "eject length", ctx.diags, 0, 255);
}

// --- Task 17: accept-only remainder (validated, not stored) ----------------
//
// Alert/clock/sensor/hardware settings with no label-image effect: parsed
// and validated per the guide's param tables, then discarded - the server's
// PrinterState (Task 20) is their eventual consumer. ^SN and ^TB live in
// textfmt.rs (they act on the field), ^TO in memory.rs, ^FX/^HU/^HY/^HZ/
// ~WC/~JR in cmd.rs.

/// `^SQa,b,c`: halt ZebraNet Alert (condition letter, destination letter,
/// halt Y/N). Validate-only; alert routing is server-side state (Task 20).
pub(crate) fn halt_zebranet_alert(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_letters(parts.first().copied(), "SQ", "condition", ctx.diags, b"ABCDEFGHIJKLMNOQRSTUVW*");
    check_letters(parts.get(1).copied(), "SQ", "destination", ctx.diags, b"ABCDEF*");
    check_letters(parts.get(2).copied(), "SQ", "halt messages", ctx.diags, b"YN");
}

/// `^SR####`: set printhead resistance, a four-digit value 0488..1175.
/// Validated, not stored - head calibration is hardware state.
pub(crate) fn set_head_resistance(ctx: &mut ParseContext, params: &[u8]) {
    check_num(split_commas(params).first().copied(), "SR", "resistance", ctx.diags, 488, 1175);
}

/// `^SSw,m,r,l,m2,r2,a,b,c`: set media-sensor calibration values (web/
/// media/ribbon 000..100; label length 0001..32000; LED intensities
/// 000..255; mark sensing 000..100/255). Validated, not stored.
pub(crate) fn set_media_sensors(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let spec: &[(&str, u32, u32)] = &[
        ("web", 0, 100),
        ("media", 0, 100),
        ("ribbon", 0, 100),
        ("label length", 1, 32000),
        ("media LED", 0, 255),
        ("ribbon LED", 0, 255),
        ("mark sensing", 0, 100),
        ("mark media sensing", 0, 100),
        ("mark LED sensing", 0, 255),
    ];
    for (i, (name, lo, hi)) in spec.iter().enumerate() {
        check_num(parts.get(i).copied(), "SS", name, ctx.diags, *lo, *hi);
    }
}

/// `^STa,b,c,d,e,f,g`: set the Real-Time Clock (month 01..12, day 01..31,
/// year 1998..2097, hour 00..23, minute/second 00..59, format A/P/M).
/// Validated, not stored - the clock is printer hardware state.
pub(crate) fn set_clock(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_num(parts.first().copied(), "ST", "month", ctx.diags, 1, 12);
    check_num(parts.get(1).copied(), "ST", "day", ctx.diags, 1, 31);
    check_num(parts.get(2).copied(), "ST", "year", ctx.diags, 1998, 2097);
    check_num(parts.get(3).copied(), "ST", "hour", ctx.diags, 0, 23);
    check_num(parts.get(4).copied(), "ST", "minute", ctx.diags, 0, 59);
    check_num(parts.get(5).copied(), "ST", "second", ctx.diags, 0, 59);
    check_letters(parts.get(6).copied(), "ST", "format", ctx.diags, b"APM");
}

/// `^SXa,b,c,d,e,f`: configure ZebraNet Alert - a condition letter, b
/// destination letter (A-F), c/d condition-set/clear Y/N, e/f destination
/// settings (e-mail/IP/SNMP - free-form per destination). Validated, not
/// stored (Task 20's server-side alerts).
pub(crate) fn set_zebranet_alert(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    check_letters(parts.first().copied(), "SX", "condition", ctx.diags, b"ABCDEFGHIJKLMNOPQRSTUV*");
    check_letters(parts.get(1).copied(), "SX", "destination", ctx.diags, b"ABCDEF");
    check_letters(parts.get(2).copied(), "SX", "set alert", ctx.diags, b"YN");
    check_letters(parts.get(3).copied(), "SX", "clear alert", ctx.diags, b"YN");
}

/// `^JZa`: reprint a partially printed label after a Ribbon/Media/Head
/// error (Y/N, power-up Y). Validated, not stored - error-reprint policy is
/// server-side print-job state.
pub(crate) fn reprint_after_error(ctx: &mut ParseContext, params: &[u8]) {
    check_letters(split_commas(params).first().copied(), "JZ", "reprint", ctx.diags, b"YN");
}

/// `~TA###`: tear-off media rest position (-120..120 dot rows; fewer than
/// 3 characters -> ignored per guide). Validated, not stored.
pub(crate) fn tear_off_adjust(ctx: &mut ParseContext, params: &[u8]) {
    let _ = i32_in(split_commas(params).first().copied(), "TA", "position", ctx.diags, -120, 120, 0);
}
