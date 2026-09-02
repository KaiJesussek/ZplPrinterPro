//! Handlers for the text-field command set: `^FO ^FT ^FD ^FV ^FH ^FR ^FB ^A`
//! (field-scoped) and `^FW ^CF ^CW ^CI ^FC` (printer state).
//!
//! Field-scoped commands no-op outside a format (state `Outside`), matching
//! Zebra; the printer-state commands are meaningful any time.

use crate::diag::{Diagnostic, Diagnostics};
use crate::ir::{Justification, OriginMode, Rotation, TextBlock};
use crate::state::{FieldDraft, FormatState, ParseContext};

/// Split command parameters on commas, trimming ASCII whitespace from each
/// segment. Multi-line ZPL puts the line break inside the last parameter
/// (`^FO100,50\n`); every numeric handler (here and in the Task 6 config
/// handlers) parses through this, so trimming keeps the trailing LF/CRLF
/// from corrupting the final value. ^FD field data does NOT go through this
/// splitter - `append_data` keeps its bytes verbatim, as Zebra does.
pub(crate) fn split_commas(params: &[u8]) -> Vec<&[u8]> {
    params.split(|&b| b == b',').map(trim_ws).collect()
}

/// Strip ASCII whitespace from both ends of a parameter segment.
pub(crate) fn trim_ws(p: &[u8]) -> &[u8] {
    let start = p.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(p.len());
    let end = p.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &p[start..end]
}

/// Split params into the first `n` comma-separated segments (whitespace
/// trimmed) plus the raw remainder - which keeps its commas. Storage
/// commands (`~DG`) put graphic data after the header segments, and that
/// data legitimately contains `,` (and `!`/`:`) compression characters, so
/// it must not go through [`split_commas`].
pub(crate) fn head_segments(params: &[u8], n: usize) -> (Vec<&[u8]>, &[u8]) {
    let mut segs = Vec::with_capacity(n);
    let mut start = 0;
    let mut count = 0;
    for (i, &b) in params.iter().enumerate() {
        if b != b',' {
            continue;
        }
        if count + 1 == n {
            segs.push(trim_ws(&params[start..i]));
            return (segs, &params[i + 1..]);
        }
        segs.push(trim_ws(&params[start..i]));
        start = i + 1;
        count += 1;
    }
    segs.push(trim_ws(&params[start..]));
    (segs, &[])
}

/// Parse a decimal unsigned parameter; `None` for empty or malformed input.
pub(crate) fn to_u32(p: &[u8]) -> Option<u32> {
    if p.is_empty() || !p.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    core::str::from_utf8(p).ok()?.parse().ok()
}

/// Parse one numeric parameter segment: an empty/missing segment silently
/// takes `fallback` (that is ZPL's default rule); garbage warns and falls
/// back too, but never panics. Shared with the Task 8 geometry handlers.
pub(crate) fn p_u32(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    fallback: u32,
) -> u32 {
    match seg.and_then(to_u32) {
        Some(v) => v,
        None => {
            if let Some(raw) = seg {
                if !raw.is_empty() {
                    diags.push(Diagnostic::warning(
                        cmd,
                        format!(
                            "invalid {name} '{}'; using {fallback}",
                            String::from_utf8_lossy(raw)
                        ),
                    ));
                }
            }
            fallback
        }
    }
}

/// Map a `^A`/`^CF` font-name character to the IR `[canonical, raw]` pair.
/// Font `0` is the default font, historically synonymous with `A`; the raw
/// character is kept in the second byte for fidelity (Task 11 maps both).
fn resolve_font(spec: u8) -> Option<[u8; 2]> {
    match spec {
        b'0' => Some([b'A', b'0']),
        b'1'..=b'9' => Some([spec, spec]),
        b'A'..=b'Z' => Some([spec, spec]),
        b'a'..=b'z' => {
            let up = spec.to_ascii_uppercase();
            Some([up, up])
        }
        _ => None,
    }
}

/// `^FW` rotation: N/0 normal, R/90, I/180, B/270 (case-insensitive letters,
/// numeric degrees), per the guide's allowed values. ^BC's `o` parameter
/// uses the same letters.
pub(crate) fn parse_rotation(p: &[u8]) -> Option<Rotation> {
    match p {
        b"N" | b"n" | b"0" => Some(Rotation::R0),
        b"R" | b"r" | b"90" => Some(Rotation::R90),
        b"I" | b"i" | b"180" => Some(Rotation::R180),
        b"B" | b"b" | b"270" => Some(Rotation::R270),
        _ => None,
    }
}

/// Open a field draft (at printer defaults) if none is open yet. No-op
/// outside a format; returns whether the draft exists.
fn ensure_field(ctx: &mut ParseContext) -> bool {
    match &ctx.state {
        FormatState::Field { .. } => return true,
        FormatState::InFormat { .. } => {}
        _ => return false,
    }
    let (draft, fields) = match core::mem::take(&mut ctx.state) {
        FormatState::InFormat { fields } => (FieldDraft::open(&ctx.mode), fields),
        _ => return false, // not in a format
    };
    ctx.state = FormatState::Field { draft, fields };
    true
}

/// Mutable handle to the open field draft, creating a default-positioned one
/// on the first field command of a format. Shared with the storage handlers
/// (^XG/^IM in memory.rs), which recall graphics into the open field.
pub(crate) fn draft_mut<'ctx>(ctx: &'ctx mut ParseContext<'_>) -> Option<&'ctx mut FieldDraft> {
    ensure_field(ctx);
    match &mut ctx.state {
        FormatState::Field { draft, .. } => Some(draft),
        _ => None,
    }
}

/// `^FOx,y,z`: field origin, the upper-left corner of the field area
/// (Block mode; z justification is parsed-but-dropped, it has no IR slot).
pub(crate) fn field_origin(ctx: &mut ParseContext, params: &[u8]) {
    apply_origin(ctx, params, OriginMode::Block, "FO");
}

/// `^FTx,y,z`: typeset origin, the baseline of the last text line
/// (Dot mode). x/y defaults are 0,0 until Task 13's text flow measures
/// "position after the last formatted field".
pub(crate) fn field_typeset(ctx: &mut ParseContext, params: &[u8]) {
    apply_origin(ctx, params, OriginMode::Dot, "FT");
}

fn apply_origin(ctx: &mut ParseContext, params: &[u8], origin: OriginMode, cmd: &str) {
    let parts = split_commas(params);
    let x = p_u32(parts.first().copied(), cmd, "x", ctx.diags, 0);
    let y = p_u32(parts.get(1).copied(), cmd, "y", ctx.diags, 0);
    if let Some(d) = draft_mut(ctx) {
        d.x = x;
        d.y = y;
        d.origin = origin;
    }
}

/// `^FD`: field data, accumulated verbatim (^FH escapes expanded).
pub(crate) fn field_data(ctx: &mut ParseContext, params: &[u8]) {
    append_data(ctx, params);
}

/// `^FV`: like `^FD` but re-evaluated per copy (variable field).
pub(crate) fn field_variable(ctx: &mut ParseContext, params: &[u8]) {
    if let Some(d) = draft_mut(ctx) {
        d.re_eval = true;
    }
    append_data(ctx, params);
}

fn append_data(ctx: &mut ParseContext, params: &[u8]) {
    // Copy the hex flag before borrowing the draft so `ctx.diags` stays free.
    let hex = draft_mut(ctx).and_then(|d| d.hex);
    let bytes = match hex {
        Some(ind) => decode_hex(params, ind, ctx.diags),
        None => params.to_vec(),
    };
    if let Some(d) = draft_mut(ctx) {
        d.data.extend_from_slice(&bytes);
    }
}

/// `^FHx`: hex escapes in this field's ^FD data use indicator x
/// (default `_`).
pub(crate) fn field_hex(ctx: &mut ParseContext, params: &[u8]) {
    if let Some(d) = draft_mut(ctx) {
        d.hex = Some(params.first().copied().unwrap_or(b'_'));
    }
}

/// `^FR`: reverse print (white on black) for this field only.
pub(crate) fn field_reverse(ctx: &mut ParseContext, _params: &[u8]) {
    if let Some(d) = draft_mut(ctx) {
        d.reverse = true;
    }
}

/// `^FWr`: default rotation for subsequent fields; invalid values warn and
/// fall back to N (per the Task 3 deferred-minor ruling).
pub(crate) fn field_orientation(ctx: &mut ParseContext, params: &[u8]) {
    let rot = split_commas(params).first().copied().unwrap_or(&[]);
    if rot.is_empty() {
        ctx.mode.fw_rotation = Rotation::R0; // bare ^FW resets to power-up N
        return;
    }
    match parse_rotation(rot) {
        Some(r) => ctx.mode.fw_rotation = r,
        None => {
            ctx.diags.push(Diagnostic::warning(
                "FW",
                format!(
                    "invalid rotation '{}'; using N",
                    String::from_utf8_lossy(rot)
                ),
            ));
            ctx.mode.fw_rotation = Rotation::R0;
        }
    }
    // The z justification parameter (x.14) is parsed-but-dropped: the IR has
    // no field-level justification slot outside ^FB.
}

/// `^FBa,b,c,d,e`: block formatting of the open field (guide defaults:
/// width 0 "does not print", 1 line, 0 spacing, left). The hanging-indent
/// `e` and justified `d` have no IR slot (J ~ Left per copy).
pub(crate) fn field_block(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let width = p_u32(parts.first().copied(), "FB", "width", ctx.diags, 0);
    let max_lines = p_u32(parts.get(1).copied(), "FB", "max lines", ctx.diags, 1);
    let line_spacing = p_u32(parts.get(2).copied(), "FB", "line spacing", ctx.diags, 0);
    let justification = parse_justification(parts.get(3).copied().unwrap_or(b""), ctx.diags);
    if let Some(d) = draft_mut(ctx) {
        d.block = Some(TextBlock {
            width,
            max_lines,
            line_spacing,
            justification,
        });
    }
}

fn parse_justification(p: &[u8], diags: &mut Diagnostics) -> Justification {
    match p.first() {
        Some(b'L') | Some(b'l') => Justification::Left,
        Some(b'C') | Some(b'c') => Justification::Center,
        Some(b'R') | Some(b'r') => Justification::Right,
        Some(b'J') | Some(b'j') => {
            diags.push(Diagnostic::warning(
                "FB",
                "justified (J) approximated as left (no IR variant)",
            ));
            Justification::Left
        }
        None => Justification::Left, // guide default d = L
        Some(_) => {
            diags.push(Diagnostic::warning(
                "FB",
                "invalid justification; using left",
            ));
            Justification::Left
        }
    }
}

/// Parse an `^A`/`^CF` size parameter: explicit values (0 allowed, meaning
/// "standard font matrix" per the guide) are used; missing segments fall
/// back to the current printer default; garbage warns and falls back too.
pub(crate) fn parse_size(
    seg: Option<&[u8]>,
    cmd: &str,
    name: &str,
    diags: &mut Diagnostics,
    fallback: u32,
) -> u32 {
    match seg {
        None => fallback,                     // ^Af or ^Af,o: no size given
        Some(p) if p.is_empty() => fallback,  // trailing comma
        Some(p) => match to_u32(p) {
            Some(v) => v,
            None => {
                diags.push(Diagnostic::warning(
                    cmd,
                    format!(
                        "invalid {name} '{}'; using current default",
                        String::from_utf8_lossy(p)
                    ),
                ));
                fallback
            }
        },
    }
}

/// `^Af,o,h,w`: font (+ optional orientation, height, width) for the open
/// field. The letter right after the font name is the field orientation
/// (N/R/I/B per the guide); h/w default to the last ^CF values (9/5 at
/// power-up), an explicit 0 keeps the standard font matrix.
pub(crate) fn select_font(ctx: &mut ParseContext, params: &[u8]) {
    // ^A@ (params start with `@`) selects a stored font by name instead of
    // a ^A character.
    if params.first() == Some(&b'@') {
        return select_downloaded_font(ctx, params);
    }
    // Params run `f[,o],h,w`: f is the first char of the first comma segment;
    // an optional orientation letter is the second char of that segment.
    let parts = split_commas(params);
    let Some(first) = parts.first().copied() else {
        return; // bare ^A keeps the ^CF default font
    };
    let Some(&f) = first.first() else {
        return;
    };
    let font = match resolve_font(f) {
        Some(font) => font,
        None => {
            ctx.diags.push(Diagnostic::warning(
                "A",
                format!("invalid font name '{}'; using current default", f as char),
            ));
            ctx.mode.default_font
        }
    };
    let orientation = match first.get(1) {
        Some(&o) if matches!(o, b'N' | b'n' | b'R' | b'r' | b'I' | b'i' | b'B' | b'b') => {
            parse_rotation(&[o])
        }
        _ => None,
    };
    let h = parse_size(parts.get(1).copied(), "A", "height", ctx.diags, ctx.mode.default_font_h);
    let w = parse_size(parts.get(2).copied(), "A", "width", ctx.diags, ctx.mode.default_font_w);
    if let Some(d) = draft_mut(ctx) {
        d.font = font;
        if let Some(r) = orientation {
            d.rotation = r;
        }
        d.font_height = h;
        d.font_width = w;
    }
}

/// `^A@o,h,w,d:f.x` - call a stored font by name (guide p.62), e.g.
/// `^A@N,50,50,E:CYRI_UB.FNT`. The orientation/size parameters behave like
/// `^A`'s; the font name (device stripped) is recorded on the field as
/// `TextField.font_name` (Task 11 maps it to `FontSpec::Downloaded`).
/// A name-less `^A@` carries over the last named font (guide: "The font
/// named carries over on all subsequent ^A@ commands without a font name");
/// before any name was given it keeps the `^CF` default.
fn select_downloaded_font(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let orientation = parts
        .first()
        .and_then(|p| p.get(1))
        .copied()
        .filter(|o| matches!(o, b'N' | b'n' | b'R' | b'r' | b'I' | b'i' | b'B' | b'b'))
        .map(|o| parse_rotation(&[o]))
        .flatten();
    let h = parse_size(parts.get(1).copied(), "A@", "height", ctx.diags, ctx.mode.default_font_h);
    let w = parse_size(parts.get(2).copied(), "A@", "width", ctx.diags, ctx.mode.default_font_w);
    let named = parts
        .get(3)
        .copied()
        .map(crate::memory::strip_device)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned());
    let name = match named {
        Some(n) => {
            ctx.mode.downloaded_font = Some(n.clone());
            Some(n)
        }
        None => ctx.mode.downloaded_font.clone(),
    };
    if let Some(d) = draft_mut(ctx) {
        if let Some(r) = orientation {
            d.rotation = r;
        }
        d.font_height = h;
        d.font_width = w;
        d.font_name = name;
    }
}

/// `^GSo,h,w` - graphic symbol (guide p.217): the field prints one of the
/// built-in symbols ((C) (R) (TM) ... selected by the `^FD` character A-E) instead
/// of a text glyph. Orientation/size behave like `^A`'s.
pub(crate) fn graphic_symbol(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let rotation = parts
        .first()
        .and_then(|p| p.first())
        .copied()
        .map(|o| parse_rotation(&[o]))
        .flatten();
    let h = parse_size(parts.get(1).copied(), "GS", "height", ctx.diags, ctx.mode.default_font_h);
    let w = parse_size(parts.get(2).copied(), "GS", "width", ctx.diags, ctx.mode.default_font_w);
    if let Some(d) = draft_mut(ctx) {
        d.symbol = true;
        if let Some(r) = rotation {
            d.rotation = r;
        }
        d.font_height = h;
        d.font_width = w;
    }
}

/// `^CFf,h,w`: printer-wide default font and default character height/width
/// (power-up: A, 9, 5). h/w are the defaults bare ^A inherits; 0 keeps the
/// standard font matrix.
pub(crate) fn change_default_font(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    match parts.first().and_then(|p| p.first()).copied() {
        None => ctx.mode.default_font = [b'A', b' '], // bare ^CF resets the font
        Some(f) => match resolve_font(f) {
            Some(font) => ctx.mode.default_font = font,
            None => ctx.diags.push(Diagnostic::warning(
                "CF",
                format!("invalid font name '{}'; keeping current default", f as char),
            )),
        },
    }
    update_size(&mut ctx.mode.default_font_h, parts.get(1).copied(), "height", ctx.diags);
    update_size(&mut ctx.mode.default_font_w, parts.get(2).copied(), "width", ctx.diags);
}

/// Apply an explicit ^CF size; missing/garbage keep the current default.
fn update_size(slot: &mut u32, seg: Option<&[u8]>, name: &str, diags: &mut Diagnostics) {
    let Some(p) = seg else { return };
    if p.is_empty() {
        return;
    }
    match to_u32(p) {
        Some(v) => *slot = v,
        None => diags.push(Diagnostic::warning(
            "CF",
            format!(
                "invalid {name} '{}'; keeping current",
                String::from_utf8_lossy(p)
            ),
        )),
    }
}

/// `^CWa,x`: assign stored font name x to the one-character identifier a.
/// Consumed by Task 11/13 (^A@-style downloaded-font selection).
pub(crate) fn assign_font(ctx: &mut ParseContext, params: &[u8]) {
    let Some(&a) = params.first() else {
        ctx.diags.push(Diagnostic::warning("CW", "missing font character"));
        return;
    };
    if !a.is_ascii_alphanumeric() {
        ctx.diags.push(Diagnostic::warning(
            "CW",
            "font character must be alphanumeric",
        ));
        return;
    }
    let name = match params.iter().position(|&b| b == b',') {
        Some(pos) => trim_ws(&params[pos + 1..]).to_vec(),
        None => Vec::new(),
    };
    ctx.mode
        .font_assignments
        .insert(a, String::from_utf8_lossy(&name).into_owned());
}

/// `^CIa,...`: international character set (codepage) for the printer
/// (power-up: 0). The s/d remapping params are consumed by Task 12's
/// codepage model.
pub(crate) fn change_international(ctx: &mut ParseContext, params: &[u8]) {
    let first = split_commas(params).first().copied().unwrap_or(&[]);
    if first.is_empty() {
        ctx.mode.code_page = 0; // bare ^CI resets to power-up 0
        return;
    }
    match to_u32(first) {
        Some(v) if v <= u8::MAX as u32 => ctx.mode.code_page = v as u8,
        _ => ctx.diags.push(Diagnostic::warning(
            "CI",
            format!(
                "invalid character set '{}'; keeping current",
                String::from_utf8_lossy(first)
            ),
        )),
    }
}

/// `^FCa,b,c`: Real-Time Clock indicator characters (primary power-up `%`).
/// Stored for the RTC handler that substitutes clock values at print time;
/// no render effect in this task.
pub(crate) fn set_clock_indicators(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let primary = parts.first().and_then(|p| p.first()).copied().unwrap_or(b'%');
    let secondary = parts.get(1).and_then(|p| p.first()).copied().unwrap_or(0);
    let third = parts.get(2).and_then(|p| p.first()).copied().unwrap_or(0);
    ctx.mode.clock_indicators = [primary, secondary, third];
}

/// Decode a `^FH` chunk: `_XX` -> byte, `__` -> literal indicator. Malformed
/// escapes (single hex digit, non-hex, trailing indicator) warn and stay
/// literal.
fn decode_hex(chunk: &[u8], ind: u8, diags: &mut Diagnostics) -> Vec<u8> {
    let mut out = Vec::with_capacity(chunk.len());
    let mut i = 0;
    while i < chunk.len() {
        if chunk[i] != ind {
            out.push(chunk[i]);
            i += 1;
            continue;
        }
        match chunk.get(i + 1) {
            Some(&n) if n == ind => {
                out.push(ind); // __ -> literal indicator
                i += 2;
            }
            Some(&h1) => match hex_val(h1).zip(chunk.get(i + 2).and_then(|&h2| hex_val(h2))) {
                Some((hi, lo)) => {
                    out.push(hi << 4 | lo);
                    i += 3;
                }
                None => {
                    diags.push(Diagnostic::warning(
                        "FH",
                        "malformed hex escape; kept literally",
                    ));
                    out.push(ind);
                    i += 1;
                }
            },
            None => {
                diags.push(Diagnostic::warning(
                    "FH",
                    "trailing hex indicator; kept literally",
                ));
                out.push(ind);
                i += 1;
            }
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::parse_stream;
    use crate::scanner::Scanner;
    use crate::state::Memory;

    /// Parse a stream, keeping the context (not just the label) so that
    /// printer-state commands (`^CI ^CW ^FC ^FW`) are observable.
    fn parse_ctx<'a>(
        src: &[u8],
        diags: &'a mut Diagnostics,
        memory: &'a mut Memory,
    ) -> ParseContext<'a> {
        let mut ctx = ParseContext::new(diags, memory);
        parse_stream(&mut Scanner::new(src), &mut ctx);
        ctx
    }

    #[test]
    fn decode_hex_expands_and_stays_literal() {
        let mut diags = Diagnostics::default();
        assert_eq!(decode_hex(b"_41_42__", b'_', &mut diags), b"AB_");
        assert_eq!(decode_hex(b"no escapes", b'_', &mut diags), b"no escapes");
        assert!(diags.is_empty());

        // Malformed escapes warn and stay literal; the field decode never
        // truncates or panics.
        assert_eq!(decode_hex(b"_4Z", b'_', &mut diags), b"_4Z");
        assert_eq!(decode_hex(b"_41_", b'_', &mut diags), b"A_");
        assert!(!diags.is_empty());
    }

    #[test]
    fn ci_cw_fc_write_printer_state() {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let ctx = parse_ctx(b"^CI6", &mut diags, &mut memory);
        assert_eq!(ctx.mode.code_page, 6);

        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let ctx = parse_ctx(b"^XA^CWA,E:FONT.FNT^FC,{,#^XZ", &mut diags, &mut memory);
        assert_eq!(ctx.mode.font_assignments.get(&b'A').map(String::as_str), Some("E:FONT.FNT"));
        assert_eq!(ctx.mode.clock_indicators, [b'%', b'{', b'#']);
        assert!(diags.is_empty());
    }

    #[test]
    fn ci_invalid_warns_and_keeps_current() {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let ctx = parse_ctx(b"^CIabc", &mut diags, &mut memory);
        assert!(ctx.diags.items.iter().any(|d| d.severity == crate::diag::Severity::Warning));
        assert_eq!(ctx.mode.code_page, 0); // untouched
    }

    #[test]
    fn field_commands_outside_format_are_ignored() {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let ctx = parse_ctx(b"^FO10,10^FDx^FS", &mut diags, &mut memory);
        assert!(matches!(ctx.state, FormatState::Outside));
        assert!(ctx.pending_label.is_none());
        assert!(diags.is_empty()); // no unknown-command noise either
    }
}
