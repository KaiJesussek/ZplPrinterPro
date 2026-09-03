//! Printer storage: stored images, downloaded fonts, and stored formats
//! (`~DG ^DF ^XF ^XG ^ID ^IL ^IM ^IS ~DY ~DS ~DT ~DU ~EG ~DN` share this
//! model; `^A@`/`^GS` consume it from the field side).
//!
//! `Memory.images` is the single source of the `~HS` string-2 stored-image
//! count (`www`): anything that stores or deletes an image mutates that map,
//! so `images.len()` is always live and truthful.

use std::collections::BTreeMap;

use crate::cmd;
use crate::diag::Diagnostic;
use crate::gfxdata::decode_graphic_data;
use crate::ir::GraphicKind;
use crate::scanner::Scanner;
use crate::state::{FieldDraft, FormatState, ParseContext, PrinterMode};
use crate::textfmt::{draft_mut, head_segments, split_commas, to_u32};

/// Printer storage: name-keyed maps, exactly as the plan specifies (the
/// server and `~HS` depend on the field names).
///
/// Image data is 1bpp packed, row-major, MSB-first: bit 7 of the first byte
/// is the leftmost dot of the first row. [`packed_row_bytes`] is the single
/// place the packed-row layout is expressed.
#[derive(Default, Debug, Clone)]
pub struct Memory {
    /// Stored bitmaps (`~DG` download; recalled by `^XG ^IM ^IL`). This map
    /// is `~HS` string-2's `www` count source.
    pub images: BTreeMap<String, Image>,
    /// Downloaded fonts (`~DY ~DS ~DT ~DU`), raw bytes; `^A@` names one.
    pub fonts: BTreeMap<String, Vec<u8>>,
    /// Stored formats (`^DF` download; recalled by `^XF`), raw ZPL bytes.
    pub formats: BTreeMap<String, Vec<u8>>,
}

/// A stored bitmap: 1bpp packed, row-major, MSB-first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Dot width of the image (always a multiple of 8: `bytes_per_row * 8`).
    pub width: u32,
    /// Dot height (= row count).
    pub height: u32,
    /// Pixels packed MSB-first; `data.len() == width/8 * height`.
    pub data: Vec<u8>,
}

/// Bytes per packed row for a `width_bits`-wide 1bpp image (rounds a
/// partial byte up, per the guide's `~DG` `w` formula). The renderer and
/// tests use this to stay consistent with the stored row layout.
pub fn packed_row_bytes(width_bits: u32) -> usize {
    ((width_bits as usize) + 7) / 8
}

/// Emulator caps for a single stored image. Real printers have a few MB of
/// DRAM; these bounds keep a hostile `~DG` from allocating unbounded
/// memory while staying far above any real label (a 32000-dot label row is
/// 4000 bytes; 16 MiB covers a continuous label multiple times over).
const MAX_IMAGE_BYTES: usize = 1 << 24;
const MAX_ROW_BYTES: usize = 4096; // 32000 dots per row (^PW/^LL max)

/// Object-name normalization shared by every store/recall command:
/// `[device:]name[.ext]` - the device (`R: E: B: A:`) is a lookup hint, our
/// stores are single-instance, so it is dropped. Case is preserved (the
/// guide's names are alphanumeric; a generator that stores `A.GRF` and
/// recalls `a.grf` loses, matching the literal-name model).
pub(crate) fn strip_device(spec: &[u8]) -> &[u8] {
    if spec.len() >= 2
        && spec[1] == b':'
        && matches!(spec[0].to_ascii_uppercase(), b'R' | b'E' | b'B' | b'A')
    {
        &spec[2..]
    } else {
        spec
    }
}

/// Normalize an image name: strip the device, default to `UNKNOWN`, and
/// append `.GRF` when the extension is not already GRF (guide `~DG`:
/// "If .GRF is not the specified file extension, .GRF is automatically
/// appended"; `^XG`/`^IM`/`^IL` recall through the same key space).
fn image_name(spec: &[u8]) -> String {
    let s = strip_device(spec);
    if s.is_empty() {
        return "UNKNOWN.GRF".into();
    }
    let ext_is_grf = s
        .rsplit(|&b| b == b'.')
        .next()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(b"GRF"));
    let mut name = String::from_utf8_lossy(s).into_owned();
    if !ext_is_grf {
        name.push_str(".GRF");
    }
    name
}

/// Normalize a stored-format name: strip the device, default to `UNKNOWN`,
/// append `.ZPL` when no extension is given (guide `^DF`/`^XF`).
pub(crate) fn format_name(spec: &[u8]) -> String {
    let s = strip_device(spec);
    if s.is_empty() {
        return "UNKNOWN.ZPL".into();
    }
    let mut name = String::from_utf8_lossy(s).into_owned();
    if !s.contains(&b'.') {
        name.push_str(".ZPL");
    }
    name
}

/// Normalize a downloaded-font key: strip the device; the name is kept
/// verbatim (guide examples embed the extension: `E:FONTFILE.TTF`). An
/// empty name defaults to `UNKNOWN`.
fn font_key(spec: &[u8]) -> String {
    let s = strip_device(spec);
    if s.is_empty() {
        return "UNKNOWN".into();
    }
    String::from_utf8_lossy(s).into_owned()
}

/// `~DGd:o.x,t,w,data` - download an ASCII-hex bitmap (guide p.175).
/// `t` = total bytes, `w` = bytes per row (`t = w * rows`), `data` is hex
/// (each char = one horizontal nibble, 4 dots) with the optional
/// compression scheme (`G-Y`/`g-z` repeat letters, `,`/`!`/`:` row ops, `;`
/// legacy terminator; see [`decode_graphic_data`]). Malformed payloads warn
/// and are dropped - never a panic.
pub(crate) fn download_graphic(ctx: &mut ParseContext, params: &[u8]) {
    let (segs, data) = head_segments(params, 3);
    let name = image_name(segs.first().copied().unwrap_or(&[]));
    let Some(t) = segs.get(1).and_then(|p| to_u32(p)) else {
        warn_drop(ctx, "DG", &name, "invalid or missing total byte count");
        return;
    };
    let Some(w) = segs.get(2).and_then(|p| to_u32(p)) else {
        warn_drop(ctx, "DG", &name, "invalid or missing bytes-per-row");
        return;
    };
    if t == 0 || w == 0 {
        warn_drop(ctx, "DG", &name, "zero size");
        return;
    }
    let (t, w) = (t as usize, w as usize);
    if t % w != 0 {
        warn_drop(ctx, "DG", &name, "total bytes not a multiple of bytes-per-row");
        return;
    }
    if t > MAX_IMAGE_BYTES || w > MAX_ROW_BYTES {
        warn_drop(ctx, "DG", &name, "image exceeds emulator size limits");
        return;
    }
    let Some(bytes) = decode_graphic_data(data, t, w) else {
        warn_drop(ctx, "DG", &name, "malformed graphic data");
        return;
    };
    let height = (t / w) as u32;
    ctx.memory.images.insert(
        name,
        Image {
            width: (w * 8) as u32,
            height,
            data: bytes,
        },
    );
}

fn warn_drop(ctx: &mut ParseContext, cmd: &str, name: &str, why: &str) {
    ctx.diags.push(Diagnostic::warning(
        cmd,
        format!("{name}: {why}; image dropped"),
    ));
}

/// `^XGd:o.x,mx,my` - recall a stored image into the open field (guide
/// p.373). The recall becomes `GraphicField::Stored(name)`; the renderer
/// resolves the bytes (Task 10). `mx`/`my` (1..10, default 1) are validated
/// but not carried in the IR (ponytail: the IR has no magnification slot;
/// add one when a test demands scaled recall).
pub(crate) fn recall_graphic(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let name = image_name(parts.first().copied().unwrap_or(&[]));
    for (label, seg) in [("mx", parts.get(1)), ("my", parts.get(2))] {
        if let Some(p) = seg {
            if p.is_empty() {
                continue; // trailing comma: default 1
            }
            match to_u32(p) {
                Some(v) if (1..=10).contains(&v) => {}
                _ => ctx.diags.push(Diagnostic::warning(
                    "XG",
                    format!("invalid magnification {label} '{}'; ignored", String::from_utf8_lossy(p)),
                )),
            }
        }
    }
    if let Some(d) = draft_mut(ctx) {
        d.graphic = Some(GraphicKind::Stored(name));
    }
}

/// `^IMd:o.x` - Image Move (guide p.248): like `^XG` without sizing
/// parameters; recalls the stored image into the open field.
pub(crate) fn image_move(ctx: &mut ParseContext, params: &[u8]) {
    let name = image_name(split_commas(params).first().copied().unwrap_or(&[]));
    if let Some(d) = draft_mut(ctx) {
        d.graphic = Some(GraphicKind::Stored(name));
    }
}

/// `^ILd:o.x` - Image Load (guide p.247): recall a stored image merged at
/// the label origin; unlike `^XG`/`^IM` it is used without an open field
/// (position is always `^FO0,0`, no `^FS`).
pub(crate) fn image_load(ctx: &mut ParseContext, params: &[u8]) {
    let name = image_name(split_commas(params).first().copied().unwrap_or(&[]));
    let draft = graphic_draft(&ctx.mode, GraphicKind::Stored(name));
    match &mut ctx.state {
        FormatState::InFormat { fields } => fields.push(draft),
        // ponytail: appended after the field drafts already closed in this
        // format (a mid-format ^IL is not the guide's usage).
        FormatState::Field { fields, .. } => fields.push(draft),
        _ => {} // outside a format: ignored
    }
}

/// `^ISd:o.x,p` - Image Save (guide p.249): save the label-so-far as a
/// raster image. That requires rendering, which lands with the renderer
/// (Task 10); instead of storing bytes that are not the rendered image, the
/// command is parsed, validated and reported as not supported.
pub(crate) fn image_save(ctx: &mut ParseContext, params: &[u8]) {
    let parts = split_commas(params);
    let name = String::from_utf8_lossy(parts.first().copied().unwrap_or(&[])).into_owned();
    let _print_after = parts.get(1).copied().unwrap_or(b"Y"); // accepted; N/Y
    ctx.diags.push(Diagnostic::not_supported(
        "IS",
        format!(
            "'{name}' would store a rendered image of this format; rendering lands with the renderer task (^IS is parsed but not stored)"
        ),
    ));
}

/// A closed field draft recalling a stored graphic at the given position
/// (used by ^IL, which does not go through the field/^FS machinery).
fn graphic_draft(mode: &PrinterMode, kind: GraphicKind) -> FieldDraft {
    let mut draft = FieldDraft::open(mode);
    draft.graphic = Some(kind);
    draft
}

/// `^IDd:o.x` - delete stored objects by name; `*` is a wildcard in the
/// name and/or extension (guide p.245: `*.GRF` all images, `SAMPLE.*` every
/// object named SAMPLE, `*.*` everything). Deletion matches across the
/// images, fonts and formats stores. A pattern matching nothing is ignored
/// (guide: the command is ignored when the object is not found).
pub(crate) fn delete_object(ctx: &mut ParseContext, params: &[u8]) {
    // A name-less ^ID deletes the object stored under the UNKNOWN default
    // (guide ^ID: "Default: if a name is not specified, UNKNOWN is used").
    let stripped = strip_device(params);
    let pat: &[u8] = if stripped.is_empty() { b"UNKNOWN" } else { stripped };
    ctx.memory.images.retain(|k, _| !glob_match(pat, k.as_bytes()));
    ctx.memory.fonts.retain(|k, _| !glob_match(pat, k.as_bytes()));
    ctx.memory.formats.retain(|k, _| !glob_match(pat, k.as_bytes()));
}

/// Iterative `*`-glob match: `*` matches any run (including empty); all
/// other characters match literally.
fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut q) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while q < s.len() {
        if p < pat.len() && (pat[p] == b'*' || pat[p] == s[q]) {
            if pat[p] == b'*' {
                star = Some(p);
                mark = q;
            }
            p += 1;
            q += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            q = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// `~EG` - Erase Download Graphics (guide p.185): erase all stored images.
pub(crate) fn erase_graphics(ctx: &mut ParseContext, _params: &[u8]) {
    ctx.memory.images.clear();
}

/// `~DN` - Abort Download Graphic (guide p.177): abort an in-progress ~DG
/// transmission and return the printer to Print Mode. The guide's intent
/// (per controller ruling) is that ~DN does NOT delete stored images -
/// deletions go through ^ID / ~EG. In this tokenized parse model a ~DG
/// payload arrives in a single command token, so there is nothing mid-flight
/// to abort; the call is a deliberate no-op that leaves `Memory.images`
/// untouched (`www` stays truthful).
pub(crate) fn abort_download(_ctx: &mut ParseContext, _params: &[u8]) {
    // no-op: nothing to abort in a tokenized ~DG model
}

/// `~DYd:f,b,x,t,w,data` - download an object (guide p.181): a font or
/// graphic in any supported format. The data bytes land raw in
/// `Memory.fonts` (Task 13 rasterizes). Header segments: `d:f` name,
/// `b` = A (ASCII hex), B (binary), C (AR-compressed), P (PNG/ZB64);
/// `x` = extension, `t` = byte count, `w` = bytes/row (ignored for fonts).
pub(crate) fn download_object(ctx: &mut ParseContext, params: &[u8]) {
    let (segs, data) = head_segments(params, 5);
    let raw_name = segs.first().copied().unwrap_or(&[]);
    let name = String::from_utf8_lossy(raw_name).into_owned();
    let format = segs.get(1).and_then(|p| p.first()).copied();
    let ext = segs.get(2).and_then(|p| p.first()).copied().unwrap_or(b'G');
    let t = segs.get(3).and_then(|p| to_u32(p)).map(|v| v as usize);
    // Extension G = raw .GRF bitmap: decode like ~DG and store as a stored
    // image (so `www` counts it). Everything else is an object (font,
    // bitmap file, ...) whose bytes land raw in `fonts`.
    if ext == b'G' || ext == b'g' {
        if format != Some(b'A') {
            warn_drop(ctx, "DY", &name, "graphic downloads need ASCII-hex data");
            return;
        }
        let Some(t) = t else {
            warn_drop(ctx, "DY", &name, "missing byte count");
            return;
        };
        let Some(w) = segs.get(4).and_then(|p| to_u32(p)) else {
            warn_drop(ctx, "DY", &name, "missing bytes-per-row");
            return;
        };
        let (t, w) = (t as usize, w as usize);
        if t == 0 || w == 0 || t % w != 0 || t > MAX_IMAGE_BYTES || w > MAX_ROW_BYTES {
            warn_drop(ctx, "DY", &name, "graphic size invalid or over limits");
            return;
        }
        let Some(bytes) = decode_graphic_data(data, t, w) else {
            warn_drop(ctx, "DY", &name, "malformed graphic data");
            return;
        };
        let key = image_name(raw_name);
        ctx.memory.images.insert(
            key,
            Image {
                width: (w * 8) as u32,
                height: (t / w) as u32,
                data: bytes,
            },
        );
        return;
    }
    let name = font_key(raw_name);
    let bytes = match format {
        Some(b'A') => {
            let Some(t) = t else {
                warn_drop(ctx, "DY", &name, "missing byte count");
                return;
            };
            if t > MAX_IMAGE_BYTES {
                warn_drop(ctx, "DY", &name, "object exceeds emulator size limits");
                return;
            }
            let Some(decoded) = decode_graphic_data(data, t, { let w = segs.get(4).and_then(|p| to_u32(p)).unwrap_or(0) as usize; w.max(1) }) else {
                warn_drop(ctx, "DY", &name, "malformed or short hex data");
                return;
            };
            decoded
        }
        Some(b'B') => {
            // Raw binary. A payload crossing a ^/~ byte cannot be carried in
            // one scanner token (the printer streams it separately); we take
            // what arrived and warn when it does not match the declared size.
            let bytes = data.to_vec();
            if let Some(t) = t {
                if bytes.len() != t {
                    ctx.diags.push(Diagnostic::warning(
                        "DY",
                        format!("{name}: binary payload is {} bytes, declared {t}", bytes.len()),
                    ));
                    return;
                }
            }
            bytes
        }
        Some(b'C') | Some(b'P') => {
            ctx.diags.push(Diagnostic::warning(
                "DY",
                format!("{name}: {}-format objects are not supported; dropped", format.unwrap() as char),
            ));
            return;
        }
        _ => {
            warn_drop(ctx, "DY", &name, "missing data format (a = ASCII hex, b = binary)");
            return;
        }
    };
    if bytes.is_empty() {
        warn_drop(ctx, "DY", &name, "empty object");
        return;
    }
    ctx.memory.fonts.insert(name, bytes);
}

/// `~DSd:o.x,s,data` - download a scalable (Intellifont) font (guide
/// p.178): `s` bytes of ASCII hex. Stored in `fonts` like ~DY; -- note the
/// plan sketch read ~DS as a delete command, which the guide does not
/// support (~DS is a download; deletions go through ^ID).
pub(crate) fn download_scalable_font(ctx: &mut ParseContext, params: &[u8]) {
    font_download(ctx, params, "FNT", "DS");
}

/// `~DTd:o.x,s,data` - download a bounded TrueType font (guide p.179).
pub(crate) fn download_bounded_font(ctx: &mut ParseContext, params: &[u8]) {
    font_download(ctx, params, "DAT", "DT");
}

/// `~DUd:o.x,s,data` - download an unbounded TrueType font (guide p.180).
pub(crate) fn download_unbounded_font(ctx: &mut ParseContext, params: &[u8]) {
    font_download(ctx, params, "FNT", "DU");
}

fn font_download(ctx: &mut ParseContext, params: &[u8], default_ext: &str, cmd: &str) {
    let (segs, data) = head_segments(params, 2);
    let name = font_key(segs.first().copied().unwrap_or(&[]));
    let name = if name.contains('.') { name } else { format!("{name}.{default_ext}") };
    let Some(s) = segs.get(1).and_then(|p| to_u32(p)) else {
        warn_drop(ctx, cmd, &name, "invalid or missing font size");
        return;
    };
    let s = s as usize;
    if s > MAX_IMAGE_BYTES {
        warn_drop(ctx, cmd, &name, "font exceeds emulator size limits");
        return;
    }
    let Some(decoded) = decode_graphic_data(data, s, 1) else {
        warn_drop(ctx, cmd, &name, "malformed or short hex data");
        return;
    };
    ctx.memory.fonts.insert(name, decoded);
}

/// `^XFd:o.x` - recall a stored format into the current label (guide
/// p.372): the stored bytes are re-executed into this parse session
/// (recursion capped at depth 4; `^XA`/`^XZ`/`^DF` inside stored bytes are
/// ignored with a warning). A missing format warns and parsing continues.
pub(crate) fn recall_format(ctx: &mut ParseContext, params: &[u8]) {
    let name = format_name(params);
    if ctx.recall_depth >= 4 {
        ctx.diags.push(Diagnostic::warning(
            "XF",
            format!("{name}: stored-format recall depth limit (4) reached; recursion stopped"),
        ));
        return;
    }
    let Some(bytes) = ctx.memory.formats.get(&name).cloned() else {
        ctx.diags.push(Diagnostic::warning(
            "XF",
            format!("stored format '{name}' not found; recall skipped"),
        ));
        return;
    };
    ctx.recall_depth += 1;
    // The stored format may have been written with a ^CC/^CT-changed prefix
    // set: parse_stream seeds the fresh scanner with the characters
    // currently in effect.
    let mut scanner = Scanner::new(&bytes);
    cmd::parse_stream(&mut scanner, ctx);
    ctx.recall_depth -= 1;
}

/// `^TOs:o.x,d:o.x`: transfer a stored object between storage devices.
/// Accept-only: transfer has no label-image effect, but the accept-only
/// contract still separates the guide's documented silent ignore (bare `^TO`
/// with no parameters - "If only ^TO is entered, the command is ignored")
/// from a malformed payload, which warns. The guide's structural rule: at
/// least one source parameter and one destination parameter must be given.
pub(crate) fn transfer_object(ctx: &mut ParseContext, params: &[u8]) {
    if params.is_empty() {
        return; // guide: bare ^TO is ignored
    }
    let parts = split_commas(params);
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        ctx.diags.push(Diagnostic::warning(
            "TO",
            format!("invalid transfer '{}'; ignored", String::from_utf8_lossy(params)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfxdata::decode_graphic_data;

    #[test]
    fn glob_matches_zebra_patterns() {
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b"*.GRF", b"LOGO.GRF", true),
            (b"*.GRF", b"LOGO.PNG", false),
            (b"SAMPLE.*", b"SAMPLE.GRF", true),
            (b"SAMPLE.*", b"SAMPLE.ZPL", true),
            (b"SAMPLE1.GRF", b"SAMPLE1.GRF", true),
            (b"SAMPLE1.GRF", b"SAMPLE2.GRF", false),
            (b"*.*", b"ANY.THING", true),
            (b"OTHER*", b"OTHER.GRF", true),
            (b"OTHER*", b"OTHERS", true),
            (b"OTHER*", b"YO.GRF", false),
        ];
        for &(pat, s, want) in cases {
            assert_eq!(glob_match(pat, s), want, "{} vs {}", String::from_utf8_lossy(pat), String::from_utf8_lossy(s));
        }
    }

    #[test]
    fn decode_expands_repeats_and_row_ops() {
        // M6 = seven 6-nibbles (needs one more nibble to close the last
        // byte); hB = forty B-nibbles; vMB = 327 B-nibbles (guide p.1761
        // formulas - each standalone run is followed by the pairing nibble).
        let m6 = decode_graphic_data(b"M66", 4, 2).expect("M6 decodes");
        assert_eq!(m6, vec![0x66, 0x66, 0x66, 0x66]);
        let vmb = decode_graphic_data(b"vMBB", 164, 1).expect("vMB decodes");
        assert_eq!(vmb, vec![0xBB; 164]);
        // Row ops: ':' repeats the previous row, ',' fills with zeros.
        let rowed = decode_graphic_data(b"FF,:", 4, 2).expect("row ops decode");
        assert_eq!(rowed, vec![0xFF, 0x00, 0xFF, 0x00]);
        // ';' terminates; trailing CR/LF are ignored.
        let term = decode_graphic_data(b"FF;\n", 1, 1).expect("terminator decode");
        assert_eq!(term, vec![0xFF]);
        // Guide p.1762: an odd trailing zero is sent "one zero and a single
        // comma" - `FFFF0,` closes the dangling 0 into a 0x00 byte and then
        // fills the row to the declared size.
        let oddzero = decode_graphic_data(b"FFFF0,", 3, 1).expect("odd-zero comma decodes");
        assert_eq!(oddzero, vec![0xFF, 0xFF, 0x00]);
        let oddzero_midrow = decode_graphic_data(b"FFFF0,", 4, 2).expect("odd-zero comma fills the row");
        assert_eq!(oddzero_midrow, vec![0xFF, 0xFF, 0x00, 0x00]);
        // A dangling NON-zero nibble before the comma is malformed.
        assert!(decode_graphic_data(b"F,", 2, 1).is_none());
        // Malformed: odd nibble count, bad repeat digit, mid-byte ':'.
        assert!(decode_graphic_data(b"M6", 3, 1).is_none());
        assert!(decode_graphic_data(b"M", 1, 1).is_none());
        assert!(decode_graphic_data(b"0:", 2, 1).is_none());
        // Exactly t bytes are decoded; the printer stops at the declared
        // size, so a longer data string is truncated, not malformed.
        assert_eq!(decode_graphic_data(b"FFFF", 1, 1), Some(vec![0xFF]));
        // Uncompressed plain hex.
        assert_eq!(decode_graphic_data(b"0F", 1, 1), Some(vec![0x0F]));
    }
}
