//! Handlers for the drawing-field command set: `^GB ^GC ^GD ^GE` (geometric
//! shapes) and `^GF` (inline graphic field, ASCII-hex form). All are field
//! commands: their position comes from the open `^FO`/`^FT` origin (Task 5)
//! and they no-op outside a format, like every other field command.

use crate::diag::{Diagnostic, Diagnostics};
use crate::gfxdata::decode_graphic_data;
use crate::ir::GraphicKind;
use crate::state::{Geometry, ParseContext};
use crate::textfmt::{draft_mut, head_segments, p_u32, split_commas, to_u32};

/// `^GF` size-parameter ceiling: the guide's `b/c/d` range is 1..99999 and
/// out-of-range values are set to the nearest limit.
const MAX_GF_PARAM: u32 = 99_999;

/// The guide's `c` (line color) parameter: B = black, W = white, default B.
/// Canonicalized to uppercase; other values warn and fall back to B.
fn parse_color(seg: Option<&[u8]>, cmd: &str, diags: &mut Diagnostics) -> u8 {
    match seg.unwrap_or(&[]).first() {
        None => b'B',
        Some(b'B') | Some(b'b') => b'B',
        Some(b'W') | Some(b'w') => b'W',
        Some(&c) => {
            diags.push(Diagnostic::warning(
                cmd,
                format!("invalid color '{c}'; using B (black)"),
            ));
            b'B'
        }
    }
}

/// `^GBw,h,t,c,r` - box or line (guide p.210): width/height default to the
/// thickness value or 1, thickness defaults to 1, color to B, corner
/// rounding `r` (0..8, default 0) is parsed-but-dropped - the IR has no
/// rounding slot (ponytail: add one when a test or the renderer demands
/// rounded corners). `t` = 0 (filled box) is recorded verbatim; the
/// renderer applies the fill rule.
pub(crate) fn graphic_box(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let t = p_u32(parts.get(2).copied(), "GB", "thickness", ctx.diags, 1);
    let w = p_u32(parts.first().copied(), "GB", "width", ctx.diags, t.max(1));
    let h = p_u32(parts.get(1).copied(), "GB", "height", ctx.diags, t.max(1));
    let color = parse_color(parts.get(3).copied(), "GB", ctx.diags);
    if let Some(d) = draft_mut(ctx) {
        d.geometry = Some(Geometry::Box { width: w, height: h, thickness: t, color });
    }
}

/// `^GCd,t,c` - circle (guide p.212): diameter defaults to 3, thickness to
/// 1, color to B. Values are recorded verbatim; the renderer clamps per the
/// guide's allowed ranges (3..4095 diameter, thickness >= 2).
pub(crate) fn graphic_circle(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let d = p_u32(parts.first().copied(), "GC", "diameter", ctx.diags, 3);
    let t = p_u32(parts.get(1).copied(), "GC", "thickness", ctx.diags, 1);
    let color = parse_color(parts.get(2).copied(), "GC", ctx.diags);
    if let Some(draft) = draft_mut(ctx) {
        draft.geometry = Some(Geometry::Circle { diameter: d, thickness: t, color });
    }
}

/// `^GDw,h,t,c,o` - diagonal line (guide p.213): width/height default to
/// the thickness value or 3, thickness to 1, color to B, orientation `o`
/// (R/"/" right-leaning, L/"\" left-leaning) to R.
pub(crate) fn graphic_diagonal(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let t = p_u32(parts.get(2).copied(), "GD", "thickness", ctx.diags, 1);
    let w = p_u32(parts.first().copied(), "GD", "width", ctx.diags, t.max(3));
    let h = p_u32(parts.get(1).copied(), "GD", "height", ctx.diags, t.max(3));
    let color = parse_color(parts.get(3).copied(), "GD", ctx.diags);
    let direction = match parts.get(4).copied().unwrap_or(&[]).first() {
        None => b'R',
        Some(b'R') | Some(b'r') | Some(b'/') => b'R',
        Some(b'L') | Some(b'l') | Some(b'\\') => b'L',
        Some(&o) => {
            ctx.diags.push(Diagnostic::warning(
                "GD",
                format!("invalid direction '{o}'; using R (/)"),
            ));
            b'R'
        }
    };
    if let Some(draft) = draft_mut(ctx) {
        draft.geometry = Some(Geometry::Diagonal {
            width: w,
            height: h,
            thickness: t,
            color,
            direction,
        });
    }
}

/// `^GEw,h,t,c` - ellipse (guide p.214): width/height default to the
/// thickness value or 1, thickness to 1, color to B. Values recorded
/// verbatim; the renderer clamps per the guide (3..4095, thickness >= 2).
pub(crate) fn graphic_ellipse(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let t = p_u32(parts.get(2).copied(), "GE", "thickness", ctx.diags, 1);
    let w = p_u32(parts.first().copied(), "GE", "width", ctx.diags, t.max(1));
    let h = p_u32(parts.get(1).copied(), "GE", "height", ctx.diags, t.max(1));
    let color = parse_color(parts.get(3).copied(), "GE", ctx.diags);
    if let Some(draft) = draft_mut(ctx) {
        draft.geometry = Some(Geometry::Ellipse { width: w, height: h, thickness: t, color });
    }
}

/// `^GFa,b,c,d,data` - inline graphic field (guide p.215; this guide is
/// ground truth over the plan brief's sketch - the brief's "a=bytes per
/// row, c=format 0/1" is the inverse of the guide's parameter table):
///
/// - `a` = compression type: **A** ASCII hex (default; "follows the format
///   for other download commands", so the `~DG` data grammar - repeat
///   letters, `,`/`!`/`:` row ops, `;` terminator, CR/LF - applies via the
///   shared decoder), **B** binary, **C** compressed binary. Only A is
///   supported here: B/C payloads cannot ride a ZPL text stream (any `^`/`~`
///   byte splits the token - the guide itself marks them as download
///   aborts) and C's scheme is host-side and undocumented. Others warn and
///   skip.
/// - `b` = binary byte count (bytes transmitted; "for ASCII download, the
///   parameter should match" c), `c` = graphic field count (total image
///   bytes = width x height; rows = c / d), `d` = bytes per row. All range
///   1..99999, out-of-range set to the nearest limit; a missing b/c/d is
///   "command is ignored" -> warning + skip.
/// - `data` = ASCII hex, two digits per image byte; any data after the
///   count (c) is satisfied is ignored.
///
/// The decoded bitmap lands on the open field draft as
/// `GraphicKind::Inline` with its bytes; position comes from `^FO`/`^FT`.
pub(crate) fn graphic_field(ctx: &mut ParseContext, params: &[u8]) {
    let (segs, data) = head_segments(params, 4);
    let comp = segs.first().and_then(|p| p.first()).copied().unwrap_or(b'A');
    let Some(b) = segs.get(1).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning("GF", "missing binary byte count (b); field skipped"));
        return;
    };
    let Some(c) = segs.get(2).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning("GF", "missing graphic field count (c); field skipped"));
        return;
    };
    let Some(d) = segs.get(3).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning("GF", "missing bytes per row (d); field skipped"));
        return;
    };
    // Guide: out-of-range values are set to the nearest limit.
    let _b = b.clamp(1, MAX_GF_PARAM);
    let c = c.clamp(1, MAX_GF_PARAM);
    let d = d.clamp(1, MAX_GF_PARAM);
    if c % d != 0 {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            format!("{c} total bytes is not a multiple of {d} bytes per row; field skipped"),
        ));
        return;
    }
    if !matches!(comp, b'A' | b'a') {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            format!(
                "compression type '{comp}': only ASCII hexadecimal (A) is supported; binary and compressed-binary payloads cannot be carried by a ZPL text stream and the compressed scheme is undocumented \u{2014} field skipped"
            ),
        ));
        return;
    }
    let Some(bytes) = decode_graphic_data(data, c as usize, d as usize) else {
        ctx.diags.push(Diagnostic::warning("GF", "malformed graphic data; field skipped"));
        return;
    };
    // Validation passed - only now open/extend the field draft, so a
    // malformed ^GF never fabricates an empty field.
    if let Some(draft) = draft_mut(ctx) {
        draft.graphic = Some(GraphicKind::Inline);
        draft.inline_data = Some((bytes, d));
    }
}
