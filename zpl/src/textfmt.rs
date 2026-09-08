//! Handlers for the text-field command set: `^FO ^FT ^FD ^FV ^FH ^FR ^FB
//! ^TB ^SN ^A` (field-scoped) and `^FW ^CF ^CW ^CI ^FC` (printer state).
//!
//! Field-scoped commands no-op outside a format (state `Outside`), matching
//! Zebra; the printer-state commands are meaningful any time.

use crate::config::u32_in;
use crate::diag::{Diagnostic, Diagnostics};
use crate::ir::{Justification, OriginMode, Rotation, TextBlock};
use crate::state::{
    FieldDraft, FormatState, HostValueRequest, ParseContext, QueryRequest, SerialSetting,
};

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
    let start = p
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(p.len());
    let end = p
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
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

/// Parse a signed decimal parameter (optional `-`); `None` for empty or
/// malformed input. ^SN's increment is signed (a minus sign decrements),
/// and its 12-digit maximum exceeds i32, so the result is i64 with checked
/// accumulation (oversized input saturates; the ^SN handler range-checks).
pub(crate) fn to_i64(p: &[u8]) -> Option<i64> {
    let (sign, digits) = match p.first() {
        Some(b'-') => (-1, &p[1..]),
        Some(b'+') => (1, &p[1..]),
        _ => (1, p),
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for &b in digits {
        v = v.saturating_mul(10).saturating_add((b - b'0') as i64);
    }
    Some(sign * v)
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
        b'0' => Some(*b"A0"),
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
    // ^FO's x/y default is 0 (the guide's parameter table): a blank
    // argument resets, it never carries. ^FT's default is "the position
    // after the last formatted text field"; a blank argument carries the
    // previous origin (measured reference: `^FT^GSN^FDA` stacks a graphic
    // symbol at the prior field's position).
    let x = p_u32(parts.first().copied(), cmd, "x", ctx.diags, 0);
    let y = p_u32(parts.get(1).copied(), cmd, "y", ctx.diags, 0);
    if let Some(d) = draft_mut(ctx) {
        d.origin = origin;
        if cmd == "FO" {
            d.x = x;
            d.y = y;
            d.auto_x = false;
            d.auto_y = false;
            return;
        }
        let blank_x = parts.first().is_some_and(|p| p.is_empty());
        let blank_y = parts.get(1).is_some_and(|p| p.is_empty());
        d.auto_x = blank_x;
        d.auto_y = blank_y;
        if !blank_x {
            d.x = x;
        }
        if !blank_y {
            d.y = y;
        }
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

/// `^SNv,n,z`: serialized field data (guide p.341) - v is the data string
/// whose rightmost integer run is the index (default "1"), n the signed
/// increment (default 1; a minus sign decrements), z = Y/N leading zeros
/// (default N). The data is expanded at ^XZ and counters tracked on
/// [`ParseContext`] (`cmd.rs::expand_serials`), so this command replaces
/// `^FD` for the field - no separate field-data command follows.
pub(crate) fn serial_number(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let v = match parts.first().copied() {
        Some(p) if !p.is_empty() => p,
        _ => b"1".as_slice(), // guide: v default 1
    };
    // Guide: "12-digits maximum for the portion to be indexed".
    let step = match parts.get(1).copied() {
        Some(p) if !p.is_empty() => match to_i64(p) {
            Some(v) if v.unsigned_abs() <= 999_999_999_999 => v,
            _ => {
                ctx.diags.push(Diagnostic::warning(
                    "SN",
                    format!(
                        "invalid increment '{}'; using 1",
                        String::from_utf8_lossy(p)
                    ),
                ));
                1
            }
        },
        _ => 1,
    };
    let zeros = match parts.get(2).and_then(|p| p.first()) {
        None => false, // guide: z default N
        Some(b'Y') | Some(b'y') => true,
        Some(b'N') | Some(b'n') => false,
        Some(_) => {
            ctx.diags.push(Diagnostic::warning(
                "SN",
                "invalid leading-zeros value; using N",
            ));
            false
        }
    };
    if let Some(d) = draft_mut(ctx) {
        d.data = v.to_vec();
        d.serial = Some(SerialSetting {
            step,
            zeros,
            mask: None,
            incr: None,
        });
    }
}

/// ^SF mask,increment: serialize a standard ^FD string through a mask of
/// position classes (guide p.335). The mask is right-aligned to the ^FD
/// characters; the increment string is a per-position value in the aligned
/// class alphabet (default: decimal one).
pub(crate) fn serialized_field(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let Some(mask) = parts.first().copied().filter(|p| !p.is_empty()) else {
        ctx.diags.push(Diagnostic::warning(
            "SF",
            "missing mask string; ^SF ignored",
        ));
        return;
    };
    if !mask.iter().all(|c| {
        matches!(
            c,
            b'D' | b'd' | b'H' | b'h' | b'O' | b'o' | b'A' | b'a' | b'N' | b'n' | b'%'
        )
    }) {
        ctx.diags.push(Diagnostic::warning(
            "SF",
            format!(
                "invalid mask character '{}'; ^SF ignored",
                String::from_utf8_lossy(
                    &mask
                        .iter()
                        .cloned()
                        .find(|c| !matches!(
                            c,
                            b'D' | b'd'
                                | b'H'
                                | b'h'
                                | b'O'
                                | b'o'
                                | b'A'
                                | b'a'
                                | b'N'
                                | b'n'
                                | b'%'
                        ))
                        .unwrap_or(0)
                        .to_le_bytes()
                )
            ),
        ));
        return;
    }
    let mask = mask.to_vec();
    // ^SF's increment uses the same class alphabet for its own characters
    // (mask-aligned); the default is a decimal one. Only non-% positions
    // contribute.
    let mut incr: Vec<u8> = parts.get(1).copied().unwrap_or(b"1").to_vec();
    if incr.is_empty() {
        incr = b"1".to_vec();
    }
    if let Some(d) = draft_mut(ctx) {
        d.serial = Some(SerialSetting {
            step: 1, // unused by the mask path (the incr string is the step)
            zeros: false,
            mask: Some(mask),
            incr: Some(incr),
        });
    }
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

/// `^LRa` (guide p.295): reverse print all fields from here on (N/Y; the
/// stored Y/N answer is carried as a persisted option). Fields following
/// the command print in reverse video, like an implicit ^FR on each.
pub(crate) fn label_reverse(ctx: &mut ParseContext, params: &[u8]) {
    ctx.label_reverse = match params.split(|&b| b == b',').next().and_then(|p| p.first()) {
        Some(b'Y') | Some(b'y') => true,
        Some(b'N') | Some(b'n') => false,
        Some(_) => {
            ctx.diags.push(Diagnostic::warning(
                "LR",
                "invalid reverse-print value; using N",
            ));
            false
        }
        None => false,
    };
}

/// `^FL<ext>,<base>,<link>` (guide p.195): link a TTF/TTE extension font
/// to resident .FNT/.TTF/.TTE base fonts (link = 1 associate / 0
/// disassociate). The printer stores the association; there is no image
/// effect in this renderer (no TTF rasterization), so the triple is
/// validated and recorded.
pub(crate) fn font_link(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let ext = parts.first().copied().unwrap_or(&[]);
    let base = parts.get(1).copied().unwrap_or(&[]);
    let upper = |b: &[u8]| b.iter().map(|c| c.to_ascii_uppercase()).collect::<Vec<_>>();
    let ext_ok =
        ext.contains(&b'.') && (upper(ext).ends_with(b".TTF") || upper(ext).ends_with(b".TTE"));
    let base_ok = base.contains(&b'.');
    let link = match parts.get(2).and_then(|p| p.first()) {
        Some(b'0') => Some(0u8),
        Some(b'1') => Some(1u8),
        _ => None,
    };
    if ext_ok && base_ok {
        if let Some(l) = link {
            ctx.config.font_links.push((ext.to_vec(), base.to_vec(), l));
        } else {
            ctx.diags
                .push(Diagnostic::warning("FL", "invalid link value; ^FL ignored"));
        }
    } else {
        ctx.diags.push(Diagnostic::warning(
            "FL",
            "extension/base filenames must include an extension; ^FL ignored",
        ));
    }
}

/// `^FEa` (guide p.194): set the field-data delimiter that brackets ^FN
/// insertions in ^FD data. Any single byte except the format ('^') and
/// control ('~') prefixes.
pub(crate) fn field_delimiter(ctx: &mut ParseContext, params: &[u8]) {
    let Some(d) = params.split(|&b| b == b',').next() else {
        ctx.diags
            .push(Diagnostic::warning("FE", "missing delimiter; ^FE ignored"));
        return;
    };
    if d.len() != 1 || matches!(d[0], b'^' | b'~') {
        ctx.diags.push(Diagnostic::warning(
            "FE",
            format!(
                "invalid delimiter '{}'; ^FE ignored",
                String::from_utf8_lossy(d)
            ),
        ));
        return;
    }
    ctx.field_delimiter = d[0];
}

/// `^FPd,g` (guide p.202): field direction - H horizontal (default),
/// V vertical (characters top-to-bottom), R reverse (right-to-left) - and
/// an additional inter-character gap of g dots (0..9999).
pub(crate) fn field_prop(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let direction = match parts.first().and_then(|p| p.first()).copied() {
        Some(b'H') | Some(b'h') => crate::ir::TextDirection::H,
        Some(b'V') | Some(b'v') => crate::ir::TextDirection::V,
        Some(b'R') | Some(b'r') => crate::ir::TextDirection::R,
        Some(_) => {
            ctx.diags.push(Diagnostic::warning(
                "FP",
                format!(
                    "invalid direction '{}'; using H",
                    String::from_utf8_lossy(parts.first().copied().unwrap_or(&[]))
                ),
            ));
            crate::ir::TextDirection::H
        }
        None => crate::ir::TextDirection::H,
    };
    let gap = u32_in(parts.get(1).copied(), "FP", "gap", ctx.diags, 0, 9999, 0);
    if let Some(d) = draft_mut(ctx) {
        d.direction = direction;
        d.char_gap = gap;
    }
}

/// `^FHx`: hex escapes in this field's ^FD data use indicator x
/// (default `_`). The indicator is trimmed like numeric params: a bare
/// `^FH` on its own line must not swallow the trailing newline.
pub(crate) fn field_hex(ctx: &mut ParseContext, params: &[u8]) {
    if let Some(d) = draft_mut(ctx) {
        d.hex = Some(trim_ws(params).first().copied().unwrap_or(b'_'));
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
    let line_spacing = match parts.get(2).and_then(|p| to_i64(p)) {
        Some(v) if (-9999..=9999).contains(&v) => v as i32,
        _ => {
            if parts.get(2).is_some_and(|p| !p.is_empty()) {
                ctx.diags.push(Diagnostic::warning(
                    "FB",
                    format!(
                        "invalid line spacing '{}'; using 0",
                        String::from_utf8_lossy(parts.get(2).copied().unwrap_or(&[]))
                    ),
                ));
            }
            0
        }
    };
    let justification = parse_justification(parts.get(3).copied().unwrap_or(b""), ctx.diags);
    if let Some(d) = draft_mut(ctx) {
        d.block = Some(TextBlock {
            width,
            max_lines,
            line_spacing,
            justification,
            fb: true,
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

/// `^TBa,b,c`: text block (guide p.356) - a = block rotation N/R/I/B
/// (validated; the field's own rotation from ^A/^FW is the guide's default
/// and is not overwritten), b = block width in dots (default 1), c = block
/// height (validated and dropped: the IR has no height slot). Applied to
/// the open field like ^FB, wrapping within b with no line cap - height
/// truncation is a render-time concern (see the task report).
pub(crate) fn text_block(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    if let Some(rot) = parts.first().copied() {
        if !rot.is_empty() && parse_rotation(rot).is_none() {
            ctx.diags.push(Diagnostic::warning(
                "TB",
                format!(
                    "invalid rotation '{}'; ignored",
                    String::from_utf8_lossy(rot)
                ),
            ));
        }
    }
    let width = p_u32(parts.get(1).copied(), "TB", "width", ctx.diags, 1);
    if width == 0 {
        ctx.diags.push(Diagnostic::warning(
            "TB",
            "width must be at least 1; using 1",
        ));
    }
    let _ = p_u32(parts.get(2).copied(), "TB", "height", ctx.diags, 1);
    if let Some(d) = draft_mut(ctx) {
        d.block = Some(TextBlock {
            width: width.max(1),
            max_lines: 0, // no line cap: height-based truncation has no IR slot
            line_spacing: 0,
            justification: Justification::Left,
            fb: false, // ^TB has no "narrower than the font does not print" rule
        });
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
        None => fallback,     // ^Af or ^Af,o: no size given
        Some([]) => fallback, // trailing comma
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
    let h = parse_size(
        parts.get(1).copied(),
        "A",
        "height",
        ctx.diags,
        ctx.mode.default_font_h,
    );
    let w = parse_size(
        parts.get(2).copied(),
        "A",
        "width",
        ctx.diags,
        ctx.mode.default_font_w,
    );
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
///
/// Selection/alignment rules transcribed from the guide (p.62):
/// * `o` - field orientation, default "N or the last ^FW value": a
///   supplied rotation overrides the field's `^FW`; an absent one keeps
///   it (the code below only assigns `d.rotation` when a letter came).
/// * `h`/`w` - for a downloaded TrueType font (scalable) these are "the
///   height/width in dots of the entire character block"; missing values
///   use the last accepted `^CF` size, and a 0 keeps the standard font
///   matrix (`^A` p.60). `w` fixes every advance to that width; `h` is
///   the cell height the fonts module's cell model draws (`~DY` p.181
///   supplies the bytes; the stored font's own header metrics supply the
///   baseline and proportional advances - see `fonts.rs`).
/// * `d:f.x` - the extension values are `.FNT = font`, `.TTF = TrueType`
///   and `.TTE = TrueType Extension`; the device is a storage hint and is
///   stripped, the name (with extension) is the memory key.
/// * A name-less `^A@` carries over the last named font (guide: "The
///   font named carries over on all subsequent ^A@ commands without a
///   font name"); before any name was given it keeps the `^CF` default
///   (font A at power-up - `^A` p.62: "if an invalid or no name is
///   entered, the default set by ^CF is used").
///
/// Guide-silent corner (kept, documented): a `w`-only `^A@` (h 0) does
/// not derive `h` from `w` via the standard matrix (p.60 "If you specify
/// only the height or width value, the standard matrix ... determines the
/// other value" - the matrix of a downloaded TTF is its own metrics and
/// the guide gives no ratio); it keeps the `^CF` default height and fixes
/// the advance to `w`.
fn select_downloaded_font(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let orientation = parts
        .first()
        .and_then(|p| p.get(1))
        .copied()
        .filter(|o| matches!(o, b'N' | b'n' | b'R' | b'r' | b'I' | b'i' | b'B' | b'b'))
        .and_then(|o| parse_rotation(&[o]));
    let h = parse_size(
        parts.get(1).copied(),
        "A@",
        "height",
        ctx.diags,
        ctx.mode.default_font_h,
    );
    let w = parse_size(
        parts.get(2).copied(),
        "A@",
        "width",
        ctx.diags,
        ctx.mode.default_font_w,
    );
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
        .and_then(|o| parse_rotation(&[o]));
    let h = parse_size(
        parts.get(1).copied(),
        "GS",
        "height",
        ctx.diags,
        ctx.mode.default_font_h,
    );
    let w = parse_size(
        parts.get(2).copied(),
        "GS",
        "width",
        ctx.diags,
        ctx.mode.default_font_w,
    );
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
        None => ctx.mode.default_font = *b"A ", // bare ^CF resets the font
        Some(f) => match resolve_font(f) {
            Some(font) => ctx.mode.default_font = font,
            None => ctx.diags.push(Diagnostic::warning(
                "CF",
                format!("invalid font name '{}'; keeping current default", f as char),
            )),
        },
    }
    update_size(
        &mut ctx.mode.default_font_h,
        parts.get(1).copied(),
        "height",
        ctx.diags,
    );
    update_size(
        &mut ctx.mode.default_font_w,
        parts.get(2).copied(),
        "width",
        ctx.diags,
    );
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
        ctx.diags
            .push(Diagnostic::warning("CW", "missing font character"));
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
/// (power-up: 0, U.S.A. 1 = Code Page 1252). The raw value is stored even
/// when it is unknown/unsupported - decoding then falls back to 1252, and
/// a later bare `^CI` still resets to 0. The s/d remapping params are
/// parsed-but-ignored (the guide's per-country replacements are deferred).
pub(crate) fn change_international(ctx: &mut ParseContext, params: &[u8]) {
    let first = split_commas(params).first().copied().unwrap_or(&[]);
    let apply = |mode_cp: &mut u8, state: &mut FormatState, cp: u8| {
        *mode_cp = cp;
        // A ^CI inside a format applies to an already-open field (printer
        // state commands are meaningful any time); drafts that open later
        // snapshot the mode at open().
        if let FormatState::Field { draft, .. } = state {
            draft.code_page = cp;
        }
    };
    if first.is_empty() {
        apply(&mut ctx.mode.code_page, &mut ctx.state, 0); // bare ^CI resets
        return;
    }
    match to_u32(first) {
        Some(v) if v <= u8::MAX as u32 => {
            let cp = v as u8;
            if !crate::codepage::is_known(cp) {
                ctx.diags.push(Diagnostic::warning(
                    "CI",
                    format!("unknown character set {cp}; using the default (1252)"),
                ));
            } else if crate::codepage::is_unsupported(cp) {
                ctx.diags.push(Diagnostic::warning(
                    "CI",
                    format!("encoding not supported: ^CI{cp}; using 1252"),
                ));
            }
            apply(&mut ctx.mode.code_page, &mut ctx.state, cp);
        }
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
    let primary = parts
        .first()
        .and_then(|p| p.first())
        .copied()
        .unwrap_or(b'%');
    let secondary = parts.get(1).and_then(|p| p.first()).copied().unwrap_or(0);
    let third = parts.get(2).and_then(|p| p.first()).copied().unwrap_or(0);
    ctx.mode.clock_indicators = [primary, secondary, third];
}

/// `^FN#,"a"` (guide p.200): assign a field-number tag (0-9999, default 0)
/// to the current field; the `"a"` KDU prompt parameter is accepted and
/// ignored (the emulator has no KDU). The tag lands on the open draft; at
/// ^XZ the decoded data is recorded in `Label::numbered` under this number
/// for the server's `^HV` responder (Task 20). No-op outside a format,
/// matching the other field commands.
pub(crate) fn field_number(ctx: &mut ParseContext, params: &[u8]) {
    let segs = split_commas(params);
    let num = p_u32(segs.first().copied(), "FN", "field number", ctx.diags, 0);
    if num > 9999 {
        ctx.diags.push(Diagnostic::warning(
            "FN",
            format!("field number {num} exceeds 9999; ignored"),
        ));
        return;
    }
    if let Some(d) = draft_mut(ctx) {
        d.field_number = Some(num);
    }
}

/// `^HV#,n,h,t,a` (guide p.239): return a numbered field's data to the
/// host. The response is server-side (Task 20): parsing records a
/// `QueryRequest::HostValue` for the TCP session, which answers it with the
/// last completed label's `^FN` data (empty for a never-defined number).
///
/// Parameter parsing: `#` (default 0) and `n` (default 64, 1..256) are the
/// clean comma-separated head; `h`/`t` may themselves contain commas, so
/// the tail splits heuristically: a lone trailing `F`/`L` segment is the
/// transfer-mode parameter (accepted, not honored - one response per
/// format, the guide's default), `h` = up to the first remaining comma,
/// `t` = the rest with commas preserved. `h` and `t` are ^FH-capable: hex
/// escapes expand with the open field's indicator (the guide's
/// `^FH_^HV0,...` pattern), or stay literal outside a field.
pub(crate) fn host_value(ctx: &mut ParseContext, params: &[u8]) {
    let (segs, tail) = head_segments(params, 2);
    let field = p_u32(segs.first().copied(), "HV", "field number", ctx.diags, 0);
    let n = p_u32(segs.get(1).copied(), "HV", "byte count", ctx.diags, 64).clamp(1, 256);
    let ind = match &ctx.state {
        FormatState::Field { draft, .. } => draft.hex,
        _ => None,
    };
    let tail = strip_transfer_mode(tail);
    let (h, t) = match tail.iter().position(|&b| b == b',') {
        Some(p) => (&tail[..p], &tail[p + 1..]),
        None => (tail, &[][..]),
    };
    let header = match ind {
        Some(i) => decode_hex(h, i, ctx.diags),
        None => h.to_vec(),
    };
    let terminator = match ind {
        Some(i) => decode_hex(t, i, ctx.diags),
        None => t.to_vec(),
    };
    ctx.queries.push(QueryRequest::HostValue(HostValueRequest {
        field,
        max_bytes: n as usize,
        header,
        terminator,
    }));
}

/// Drop a trailing lone `L`/`F` segment (the `^HV` `a` transfer-mode
/// parameter); anything else - including longer data that merely starts
/// with F or L - stays put.
fn strip_transfer_mode(tail: &[u8]) -> &[u8] {
    let Some(pos) = tail.iter().rposition(|&b| b == b',') else {
        return tail;
    };
    match &tail[pos + 1..] {
        [b'F'] | [b'f'] | [b'L'] | [b'l'] => &tail[..pos],
        _ => tail,
    }
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
        assert_eq!(
            ctx.mode.font_assignments.get(&b'A').map(String::as_str),
            Some("E:FONT.FNT")
        );
        assert_eq!(ctx.mode.clock_indicators, [b'%', b'{', b'#']);
        assert!(diags.is_empty());
    }

    #[test]
    fn ci_invalid_warns_and_keeps_current() {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let ctx = parse_ctx(b"^CIabc", &mut diags, &mut memory);
        assert!(ctx
            .diags
            .items
            .iter()
            .any(|d| d.severity == crate::diag::Severity::Warning));
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
