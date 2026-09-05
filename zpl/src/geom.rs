//! Handlers for the drawing-field command set: `^GB ^GC ^GD ^GE` (geometric
//! shapes) and `^GF` (inline graphic field, ASCII-hex form). All are field
//! commands: their position comes from the open `^FO`/`^FT` origin (Task 5)
//! and they no-op outside a format, like every other field command.

use crate::diag::{Diagnostic, Diagnostics};
use crate::gfxdata::{decode_base64, decode_graphic_data, zlib_decompress};
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
        d.geometry = Some(Geometry::Box {
            width: w,
            height: h,
            thickness: t,
            color,
        });
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
        draft.geometry = Some(Geometry::Circle {
            diameter: d,
            thickness: t,
            color,
        });
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
        draft.geometry = Some(Geometry::Ellipse {
            width: w,
            height: h,
            thickness: t,
            color,
        });
    }
}

/// `^GFa,b,c,d,data` - inline graphic field (guide p.215; this guide is
/// ground truth over the plan brief's sketch - the brief's "a=bytes per
/// row, c=format 0/1" is the inverse of the guide's parameter table):
///
/// - `a` = compression type: **A** ASCII hex (default; "follows the format
///   for other download commands", so the `~DG` data grammar - repeat
///   letters, `,`/`!`/`:` row ops, `;` terminator, CR/LF - applies via the
///   shared decoder), **B** binary, **C** compressed binary, **P** ZB64
///   PNG. B and C carry `b` strictly-binary bytes after `d` - the parse
///   driver raw-frames them ([`gf_binary_len`]/[`gf_binary_handler`]),
///   since the payload may legally contain `^`/`~` ("all control prefixes
///   are ignored until the total number of bytes needed ... is sent").
///   P is the ZB64 base64 encoding of a PNG (p.1602), printable ASCII
///   only - it rides the tokenizer; dimensions come from the PNG itself.
///   C's payload is a raw zlib stream (RFC 1950: 0x78 header + adler32),
///   inflated to `c` graphic bytes - the same deflate the `:Z64:` A path
///   uses, minus the base64/CRC framing; a malformed stream warns and skips.
///   Others warn and skip.
/// - `b` = binary byte count (bytes transmitted; "for ASCII download, the
///   parameter should match" c), `c` = graphic field count (total image
///   bytes = width x height; rows = c / d), `d` = bytes per row. All range
///   1..99999, out-of-range set to the nearest limit; a missing b/c/d is
///   "command is ignored" -> warning + skip.
/// - `data` = ASCII hex, two digits per image byte - or the `:Z64:`
///   variant, a base64 zlib stream that inflates to the raw bitmap (the
///   reference payloads use this); any data after the count (c) is
///   satisfied is ignored. P is the ZB64 PNG instead.
///
/// The decoded bitmap lands on the open field draft as
/// `GraphicKind::Inline` with its bytes; position comes from `^FO`/`^FT`.
pub(crate) fn graphic_field(ctx: &mut ParseContext, params: &[u8]) {
    let (segs, data) = head_segments(params, 4);
    let comp = segs
        .first()
        .and_then(|p| p.first())
        .copied()
        .unwrap_or(b'A');
    // P: ZB64-encoded PNG (guide p.1602 - base64 emits only printable
    // ASCII, so the payload rides the tokenizer with no raw framing; the
    // PNG header itself carries the dimensions). Decode first, then open
    // the draft only on success, like every other ^GF path.
    if matches!(comp, b'P' | b'p') {
        match zb64_png_bitmap(data) {
            Ok((w, _h, bits)) => {
                if let Some(draft) = draft_mut(ctx) {
                    draft.graphic = Some(GraphicKind::Inline);
                    draft.inline_data = Some((bits, crate::memory::packed_row_bytes(w) as u32));
                }
            }
            Err(why) => ctx.diags.push(Diagnostic::warning("GF", why)),
        }
        return;
    }
    let Some(b) = segs.get(1).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing binary byte count (b); field skipped",
        ));
        return;
    };
    let Some(c) = segs.get(2).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing graphic field count (c); field skipped",
        ));
        return;
    };
    let Some(d) = segs.get(3).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing bytes per row (d); field skipped",
        ));
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
    // B/C with a parseable byte count never reach the table handler (the
    // driver frames them via gf_binary_len/gf_binary_handler); landing
    // here means `b` was missing - already warned above. Unknown letters
    // warn here.
    if !matches!(comp, b'A' | b'a') {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            format!(
                "compression type '{comp}': only ASCII hex (A), binary (B), compressed-binary (C), and ZB64 PNG (P) are supported; field skipped"
            ),
        ));
        return;
    }
    // Two A-format encodings: the classic ASCII-hex nibble stream, and the
    // `:Z64:` variant (the reference payloads) whose base64 body is a zlib
    // stream inflating to the raw bitmap. Both take the graphic count `c`
    // bytes; the A-format rule "any data after the count is satisfied is
    // ignored" applies to both (extras past `c` are dropped). The `:Z64:`
    // body may carry a trailing `:crc` (four hex digits) - the same
    // trailer the ZB64 PNG path tolerates - stripped but not verified,
    // since the CRC polynomial is unspecified.
    let c = c as usize;
    let d = d as usize;
    let decoded = if let Some(rest) = data.strip_prefix(b":Z64:") {
        let body = strip_crc_trailer(rest);
        decode_base64(body)
            .and_then(|raw| zlib_decompress(&raw))
            .filter(|raw| raw.len() >= c)
            .map(|raw| raw[..c].to_vec())
    } else {
        decode_graphic_data(data, c, d)
    };
    let Some(bytes) = decoded else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "malformed graphic data; field skipped",
        ));
        return;
    };
    // Validation passed - only now open/extend the field draft, so a
    // malformed ^GF never fabricates an empty field.
    if let Some(draft) = draft_mut(ctx) {
        draft.graphic = Some(GraphicKind::Inline);
        draft.inline_data = Some((bytes, d as u32));
    }
}

/// ^GF binary framing (used by the parse driver): for `a` = B/C, returns
/// `(declared byte count, payload bytes the token's params run already
/// absorbed)`. The scanner stops the params run at the first `^`/`~` -
/// which may sit inside the payload - so the driver consumes the remaining
/// `declared - already` bytes raw before the next token. `None` for A/P
/// (text payloads) and malformed byte counts.
pub(crate) fn gf_binary_len(params: &[u8]) -> Option<(usize, usize)> {
    let (segs, already) = head_segments(params, 4);
    let comp = segs
        .first()
        .and_then(|p| p.first())
        .copied()
        .unwrap_or(b'A');
    if !matches!(comp, b'B' | b'b' | b'C' | b'c') {
        return None;
    }
    let declared = segs
        .get(1)
        .and_then(|p| to_u32(p))?
        .clamp(1, MAX_GF_PARAM) as usize;
    Some((declared, already.len()))
}

/// ^GF B/C payload receiver: reconstructs the full payload (the token's
/// data segments plus the raw tail the driver took), validates the header
/// exactly as the A path does, and lands the bitmap on the open field
/// draft. Uncompressed B data *is* the bitmap (`b` should equal `c`;
/// extras are ignored, a short payload warns and skips). C's payload is a
/// raw zlib stream whose *compressed* length is `b` (smaller than `c`):
/// inflate it first, then check the inflated size against `c` (extras
/// ignored, a malformed/short stream warns and skips) - never a panic,
/// never a partial draft.
pub(crate) fn gf_binary_handler(ctx: &mut ParseContext, params: &[u8], tail: &[u8]) {
    let (segs, already) = head_segments(params, 4);
    let comp = segs
        .first()
        .and_then(|p| p.first())
        .copied()
        .unwrap_or(b'A');
    let Some(b) = segs.get(1).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing binary byte count (b); field skipped",
        ));
        return;
    };
    let Some(c) = segs.get(2).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing graphic field count (c); field skipped",
        ));
        return;
    };
    let Some(d) = segs.get(3).and_then(|p| to_u32(p)) else {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            "missing bytes per row (d); field skipped",
        ));
        return;
    };
    let declared = b.clamp(1, MAX_GF_PARAM) as usize;
    let c = c.clamp(1, MAX_GF_PARAM) as usize;
    let d = d.clamp(1, MAX_GF_PARAM) as usize;
    if !c.is_multiple_of(d) {
        ctx.diags.push(Diagnostic::warning(
            "GF",
            format!("{c} total bytes is not a multiple of {d} bytes per row; field skipped"),
        ));
        return;
    }
    // Payload = what the token absorbed (already) + the raw tail, capped
    // at the declared count. The driver guarantees the full count except
    // in the truncated case (warned there, dropped here).
    let mut payload = Vec::with_capacity(already.len().min(declared));
    payload.extend_from_slice(&already[..already.len().min(declared)]);
    payload.extend_from_slice(&tail[..tail.len().min(declared - payload.len())]);
    let Some(bytes) = (payload.len() == declared).then_some(payload) else {
        return;
    };
    match comp {
        b'B' | b'b' if bytes.len() >= c => {
            // Extras past the graphic count are ignored, mirroring the A
            // path's "any data after the count is satisfied is ignored".
            if let Some(draft) = draft_mut(ctx) {
                draft.graphic = Some(GraphicKind::Inline);
                draft.inline_data = Some((bytes[..c].to_vec(), d as u32));
            }
        }
        b'B' | b'b' => {
            ctx.diags.push(Diagnostic::warning(
                "GF",
                format!(
                    "binary payload is {declared} bytes, graphic count (c) is {c}; field skipped"
                ),
            ));
        }
        b'C' | b'c' => {
            // C's payload is a raw zlib stream (RFC 1950: 0x78 header,
            // deflate body, adler32 trailer) - the same deflate the `:Z64:`
            // A path uses, minus the base64/CRC framing. `b` is the
            // *compressed* length (smaller than `c`), so inflate first and
            // then check the *inflated* size against `c`; the declared `b`
            // bytes are the stream, `c` the inflated bitmap count. Malformed
            // zlib (bad header, truncated stream, inflates short of `c`)
            // warns and skips - never a panic, never a partial draft.
            match zlib_decompress(&bytes) {
                Some(inflated) if inflated.len() >= c => {
                    // Extras past the graphic count are ignored, mirroring
                    // the A/B paths.
                    if let Some(draft) = draft_mut(ctx) {
                        draft.graphic = Some(GraphicKind::Inline);
                        draft.inline_data = Some((inflated[..c].to_vec(), d as u32));
                    }
                }
                _ => {
                    ctx.diags.push(Diagnostic::warning(
                        "GF",
                        format!(
                            "compressed binary (C) payload did not inflate to {c} graphic bytes; field skipped"
                        ),
                    ));
                }
            }
        }
        _ => {}
    }
}

/// Decode a ZB64-encoded PNG (guide p.1602-1603): an optional `:B64:`/
/// `:Z64:` lead-in and a trailing `:crc` are stripped, the interior is
/// MIME base64, and the PNG opens into the 1bpp row-packed bitmap model.
/// Z64 (LZ77-compressed) payloads are acknowledged but not decompressed -
/// the inflater is out of scope for what is practically always B64 data.
fn zb64_png_bitmap(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let body = if let Some(rest) = data.strip_prefix(b":B64:") {
        rest
    } else if data.starts_with(b":Z64:") {
        return Err("Z64-compressed PNG is not supported; field skipped".into());
    } else {
        data
    };
    // The appendix tags the encoded stream with a trailing `:crc` (four hex
    // digits); the CRC polynomial is unspecified, so the trailer is stripped
    // but not verified.
    let body = strip_crc_trailer(body);
    let raw = crate::gfxdata::decode_base64(body)
        .ok_or_else(|| "malformed ZB64 data; field skipped".to_string())?;
    crate::gfxdata::png_to_bitmap(&raw)
        .ok_or_else(|| "malformed PNG data; field skipped".to_string())
}

/// Strip an optional trailing `:crc` (four hex digits) from a base64 body.
/// Both the ZB64 PNG path (`:B64:`) and the A-format `:Z64:` path may carry
/// the trailer; the CRC polynomial is unspecified, so it is dropped but not
/// verified. `data` without the trailer is returned unchanged.
fn strip_crc_trailer(data: &[u8]) -> &[u8] {
    if data.len() >= 5
        && data[data.len() - 5] == b':'
        && data[data.len() - 4..].iter().all(|b| b.is_ascii_hexdigit())
    {
        &data[..data.len() - 5]
    } else {
        data
    }
}
