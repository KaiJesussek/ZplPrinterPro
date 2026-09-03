//! Barcode-field painting (Task 14): `Pattern::Bars` and `Pattern::Modules`
//! -> bitmap dots, the interpretation line in the field's font, quiet zones.
//!
//! # Semantics (guide-anchored)
//!
//! * **Height** - `^BC`/`^BY` `h` is "barcode height (in dots)": the BAR
//!   height only. The interpretation line is printed **below** the bars and
//!   is not part of the height parameter (the guide's `f = print
//!   interpretation line` is a separate parameter, p.94; the UCC/EAN
//!   section's Figure 12 "Printing the Interpretation Line" shows the line
//!   in its own band under the symbol).
//! * **Interpret line font** - guide ^BC p.94: "The interpretation line
//!   can be printed in any font by placing the font command before the
//!   barcode command." `BarcodeField` therefore carries the field's `^A`
//!   font (Task 14 captured it at parse); size follows the same `^A` rules
//!   as text (height -> cell scale, width -> fixed advance). `f=Y` prints
//!   the line below the bars, `g=Y` (`interpret`) above them (both may be
//!   set). The line shows the decoded field data (^FH applied), starting
//!   at the field origin - the UCC/EAN mode prints parens/spaces in the
//!   line exactly as the guide describes.
//! * **Quiet zone** - guide "Basic Format for Bar Codes" (Fonts and
//!   Barcodes, p.1586): "The size of the quiet zone depends on the size of
//!   bar widths (usually 10 times the width of the narrow bar)" - Code
//!   128's narrow element is one module (ISO/IEC 15417), so the drawer
//!   reserves **10 x module dots** on each side and starts the bars after
//!   it (para-free: the film of whitespace is part of the field this
//!   renderer draws, so a `^FO0,0` barcode still has its quiet zones).
//!   2D module maps paint raw at the origin - their symbology encoders own
//!   the quiet zone (Task 16).
//! * **Rotation** - `^FW`/`^BC o` rotate the field content about the ^FO
//!   origin, the same transform as text (`^FW` p.208: `R` = "rotated 90
//!   degrees (clockwise)", ...). Bars, modules AND the interpret line are
//!   one R0 content plane mapped through the field rotation
//!   (R90: `(u,v) -> (fh-1-v, u)`, ...). The line below the bars therefore
//!   lands on the left of an R90 symbol, glyphs rotated with the plane -
//!   text and barcode rotate identically. An interpretation line above the
//!   bars (`g=Y`) sits at negative content `v` and rotates to the right.
//!
//! # Safety
//!
//! Never panics, never allocates unboundedly: content coordinates clamp to
//! 2^50 (far beyond the 32000-dot canvas ceiling), every painted shape is
//! clipped to the canvas bounding box and loops only over on-canvas
//! pixels, glyph rasterization reuses text.rs's [`MAX_GLYPH_CELL`] cap,
//! and all modulo/bit reads are bounds-checked (`Modules` rows are read
//! defensively - a truncated payload paints as white).

use ab_glyph::{point, Font, FontVec, GlyphId};

use crate::bc::Pattern;
use crate::fonts::FontData;
use crate::ir::{BarcodeField, Rotation};
use crate::render::text::{ref_advance, MAX_GLYPH_CELL};
use crate::render::{font_spec, place, resolve_font, Bitmap, RenderOptions};

/// Content-coordinate ceiling: any dot beyond this can never intersect the
/// 32000-dot canvas (even shifted by ^LH/^LS/^LT/^PO/^PM), and clamping to
/// it keeps every bounds sum well inside i64 (2*2^50 <= 2^51).
const MAX_CONTENT: u64 = 1 << 50;

/// The R0 content box of a painted barcode, in field-relative coordinates.
/// The rotation constants match text.rs's transform: the box maps
/// `u in [0, fw), v in [0, fh)` to the post-rotation plane.
pub(crate) struct Extent {
    /// Content width: quiet zones + symbol, or the module map, or the
    /// interpret line's advance (whichever is wider).
    pub fw: u32,
    /// Content height: bar height + line cell (or the module map height).
    pub fh: u32,
}

/// Clamp a content coordinate/width to the i64-safe band.
fn c50(v: u128) -> i64 {
    v.min(MAX_CONTENT as u128) as i64
}

/// Clamp a content measure to a u32 box dimension.
fn cmax(v: u128) -> u32 {
    v.min(u32::MAX as u128) as u32
}

/// The interpret-line cell height in dots: the `^A` height rule applied to
/// the field font (`0` = standard matrix; capped like text rasterization).
/// 0 when the font is unavailable - the line then prints nothing.
fn line_cell(b: &BarcodeField, fd: Option<&FontData>, dpi: u32) -> u32 {
    let Some(fd) = fd else { return 0 };
    let eff = if b.font_height > 0 {
        b.font_height as f32 / fd.metrics.cell_height.max(1) as f32
    } else {
        dpi as f32 / 203.0
    };
    ((fd.metrics.cell_height as f32 * eff).round()).clamp(1.0, MAX_GLYPH_CELL as f32) as u32
}

/// Total width of the interpret line at the field font's size rules
/// (fixed `^A` width when set, else proportional advances), clamped.
fn line_width(b: &BarcodeField, fd: &FontData, dpi: u32) -> u32 {
    let eff = if b.font_height > 0 {
        b.font_height as f32 / fd.metrics.cell_height.max(1) as f32
    } else {
        dpi as f32 / 203.0
    };
    let mut total: u128 = 0;
    for ch in b.data.chars() {
        let adv = if b.font_width > 0 {
            b.font_width as u128
        } else {
            (ref_advance(&fd.metrics, ch) as f32 * eff).round().max(0.0) as u128
        };
        total += adv;
    }
    cmax(total)
}

/// Measure the R0 content box of a barcode field. `fd` is the resolved
/// interpret-line font (needed for its cell/width), or `None`.
fn extent(b: &BarcodeField, pat: &Pattern, fd: Option<&FontData>, dpi: u32) -> Extent {
    // ^BY/^BC defaults, mirror of bc::barcode_field (hand-built IR can
    // carry 0s that the parser never leaves behind).
    let module = if b.module == 0 { 2 } else { b.module };
    let height = if b.height == 0 { 10 } else { b.height };
    match pat {
        Pattern::Bars(bars) => {
            let quiet = module as u128 * 10; // 10 modules per side
            let pat_w: u128 = bars.iter().map(|x| x.width as u128).sum();
            let below = b.line == b'Y';
            let above = b.interpret;
            let lc = if below || above { line_cell(b, fd, dpi) as u128 } else { 0 };
            // Each printed interpretation line adds its own cell: `f=Y` puts
            // one below the bars (`v` in [h, h+lc)) and `g=Y` one above them
            // (negative `v`), so the content box spans [-lc, h+lc) - the box
            // height counts the two cells separately.
            let lc_span = (u128::from(below) + u128::from(above)) * lc;
            let fh = height as u128 + lc_span;
            let tw = if (below || above) && fd.is_some() {
                line_width(b, fd.unwrap(), dpi) as u128
            } else {
                0
            };
            Extent { fw: cmax((2 * quiet + pat_w).max(tw)), fh: cmax(fh) }
        }
        Pattern::Modules { width, height, .. } => Extent {
            fw: cmax(module as u128 * *width as u128),
            fh: cmax(module as u128 * *height as u128),
        },
    }
}

/// The field's (y, height) in label space for the auto-height scan: the
/// bar block (plus the interpret line, when one prints and its font
/// resolves - a missing font hides only the line, not the bars).
pub(crate) fn footprint(b: &BarcodeField, opts: &RenderOptions) -> Option<(u32, u32)> {
    let height = if b.height == 0 { 10 } else { b.height };
    let line = b.line == b'Y' || b.interpret;
    let lc = if line {
        resolve_font(&font_spec(b.font, &b.font_name), opts)
            .map(|fd| line_cell(b, Some(&fd), opts.dpi))
            .unwrap_or(0)
    } else {
        0
    };
    // Each printed line adds its own cell (f=Y below, g=Y above): the box
    // covers [-lc, h+lc) relative to the origin, and label-level transforms
    // (^POI/^PMY far corner, auto height) must see the full span.
    let lc_span = (u32::from(b.line == b'Y') + u32::from(b.interpret)) * lc;
    let y_off = if b.interpret { -(lc as i64) } else { 0 };
    let y = (b.y as i64 + y_off).max(0) as u32;
    Some((y, height.saturating_add(lc_span)))
}

/// Map a content dot through the field rotation (same orientation as
/// text.rs's transform: R = 90deg clockwise about the ^FO origin).
fn rotate(u: i64, v: i64, r: Rotation, fw: i64, fh: i64) -> (i64, i64) {
    match r {
        Rotation::R0 => (u, v),
        Rotation::R90 => (fh - 1 - v, u),
        Rotation::R180 => (fw - 1 - u, fh - 1 - v),
        Rotation::R270 => (v, fw - 1 - u),
    }
}

/// Inverse of [`rotate`] (bitmap dot -> content dot).
fn unrotate(bx: i64, by: i64, r: Rotation, fw: i64, fh: i64) -> (i64, i64) {
    match r {
        Rotation::R0 => (bx, by),
        Rotation::R90 => (by, fh - 1 - bx),
        Rotation::R180 => (fw - 1 - bx, fh - 1 - by),
        Rotation::R270 => (fw - 1 - by, bx),
    }
}

/// The canvas-clipped destination range of the rotated R0 rect
/// `[u0, u0+w) x [v0, v0+h)` after the placed origin shift - `None` when
/// the rect misses the canvas entirely. Ranges are inclusive.
#[allow(clippy::too_many_arguments)]
fn dest_bounds(
    bmp: &Bitmap,
    ox: i64,
    oy: i64,
    r: Rotation,
    fw: i64,
    fh: i64,
    u0: i64,
    v0: i64,
    w: i64,
    h: i64,
) -> Option<(i64, i64, i64, i64)> {
    let (u1, v1) = (u0 + w, v0 + h); // bounded by the 2^50 clamp -> no overflow
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (u, v) in [(u0, v0), (u1, v0), (u0, v1), (u1, v1)] {
        let (bx, by) = rotate(u, v, r, fw, fh);
        min_x = min_x.min(bx);
        min_y = min_y.min(by);
        max_x = max_x.max(bx);
        max_y = max_y.max(by);
    }
    min_x += ox;
    min_y += oy;
    max_x += ox;
    max_y += oy;
    if max_x < 0 || max_y < 0 || min_x >= bmp.width as i64 || min_y >= bmp.height as i64 {
        return None;
    }
    Some((
        min_x.max(0),
        max_x.min(bmp.width as i64 - 1),
        min_y.max(0),
        max_y.min(bmp.height as i64 - 1),
    ))
}

/// Paint one R0-space rectangle (a bar element, or one 2D module cell)
/// through the field rotation, clipped to the canvas.
#[allow(clippy::too_many_arguments)]
fn paint_rect(
    bmp: &mut Bitmap,
    ox: i64,
    oy: i64,
    r: Rotation,
    fw: i64,
    fh: i64,
    u0: i64,
    v0: i64,
    w: i64,
    h: i64,
) {
    let Some((x0, x1, y0, y1)) = dest_bounds(bmp, ox, oy, r, fw, fh, u0, v0, w, h) else {
        return;
    };
    let (u1, v1) = (u0 + w, v0 + h);
    for by in y0..=y1 {
        for bx in x0..=x1 {
            let (u, v) = unrotate(bx - ox, by - oy, r, fw, fh);
            if u >= u0 && u < u1 && v >= v0 && v < v1 {
                bmp.set(bx as u32, by as u32, true);
            }
        }
    }
}

/// Paint the interpretation line in the field font: one line of `b.data`
/// in a `cell`-tall box whose content top is `y_top` (the bar height for
/// `f=Y`, -cell for `g=Y`), glyphs rotated with the barcode plane.
#[allow(clippy::too_many_arguments)]
fn paint_line(
    bmp: &mut Bitmap,
    b: &BarcodeField,
    fd: &FontData,
    g: &Extent,
    y_top: i64,
    ox: i64,
    oy: i64,
    opts: &RenderOptions,
    warnings: &mut Vec<String>,
) {
    let Ok(face) = FontVec::try_from_vec(fd.ttf.clone()) else {
        warnings.push("stored font bytes are not a usable TrueType font; interpretation line skipped".into());
        return;
    };
    let upem = face.units_per_em().unwrap_or(2048.0);
    let ascent = face.ascent_unscaled();
    if ascent <= 0.0 || upem <= 0.0 {
        warnings.push("font has unusable vertical metrics; interpretation line skipped".into());
        return;
    }
    let eff = if b.font_height > 0 {
        b.font_height as f32 / fd.metrics.cell_height.max(1) as f32
    } else {
        opts.dpi as f32 / 203.0
    };
    let cell = ((fd.metrics.cell_height as f32 * eff).round()).clamp(1.0, MAX_GLYPH_CELL as f32) as u32;
    let baseline = ((fd.metrics.baseline as f32 * eff).round()).clamp(1.0, cell as f32) as u32;
    // Same fit as text.rs: the face's ascent lands on the metric baseline.
    let scale = baseline as f32 * upem / ascent;
    let fixed = if b.font_width > 0 { Some(b.font_width as i64) } else { None };
    let fw = g.fw as i64;
    let fh = g.fh as i64;
    let mut char_x: i64 = 0;
    for ch in b.data.chars() {
        let adv = match fixed {
            Some(f) => f,
            None => (ref_advance(&fd.metrics, ch) as f32 * eff).round().max(0.0) as i64,
        };
        if adv > 0 {
            // Beyond the content ceiling no glyph can be visible; stop
            // accumulating instead of risking i64 overflow on hostile data.
            if char_x >= MAX_CONTENT as i64 {
                break;
            }
            let u0 = char_x.min(MAX_CONTENT as i64);
            let gid = face.glyph_id(ch);
            // Skip glyphs whose cell misses the canvas before rasterizing.
            let on_canvas = dest_bounds(bmp, ox, oy, b.rotation, fw, fh, u0, y_top, adv, cell as i64).is_some();
            if gid != GlyphId(0) && on_canvas {
                let glyph = gid.with_scale_and_position(scale, point(u0 as f32, 0.0));
                if let Some(outlined) = face.outline_glyph(glyph) {
                    let pbox = outlined.px_bounds();
                    outlined.draw(|px, py, cov| {
                        if cov < 0.5 {
                            return;
                        }
                        // Mirror of text.rs's pixels: baseline-relative, y up.
                        let gx = pbox.min.x.floor() as i64 + px as i64 - u0;
                        let gy = baseline as i64 + pbox.min.y.floor() as i64 + py as i64;
                        if gx < 0 || gx >= adv || gy < 0 || gy >= cell as i64 {
                            return;
                        }
                        let (bx, by) = rotate(u0 + gx, y_top + gy, b.rotation, fw, fh);
                        // Negative destinations are dropped by set().
                        bmp.set((bx + ox) as u32, (by + oy) as u32, true);
                    });
                }
            }
        }
        char_x = char_x.saturating_add(adv);
    }
}

/// Paint a barcode field: resolve the ^BY/^BC defaults, draw the bars or
/// module map, then the interpretation line(s). Warnings cover an
/// unresolvable line font (the bars still paint).
pub(crate) fn paint(
    bmp: &mut Bitmap,
    b: &BarcodeField,
    pat: &Pattern,
    opts: &RenderOptions,
    warnings: &mut Vec<String>,
) {
    let line = b.line == b'Y' || b.interpret;
    let fd = if line {
        let spec = font_spec(b.font, &b.font_name);
        match resolve_font(&spec, opts) {
            Some(fd) => Some(fd),
            None => {
                warnings.push(format!(
                    "interpretation line font unavailable ({spec:?}); line skipped"
                ));
                None
            }
        }
    } else {
        None
    };
    let g = extent(b, pat, fd.as_ref(), opts.dpi);
    let (ox, oy) = place(opts.mode, bmp.width as i64, bmp.height as i64, b.x, b.y, g.fw, g.fh);
    let module = if b.module == 0 { 2 } else { b.module };
    let fw = g.fw as i64;
    let fh = g.fh as i64;
    match pat {
        Pattern::Bars(bars) => {
            let quiet = c50(module as u128 * 10);
            let h = c50(if b.height == 0 { 10 } else { b.height } as u128);
            let mut off: u128 = quiet as u128;
            for bar in bars {
                if bar.black {
                    paint_rect(bmp, ox, oy, b.rotation, fw, fh, c50(off), 0, c50(bar.width as u128), h);
                }
                off += bar.width as u128;
            }
        }
        Pattern::Modules { width, height, data } => {
            // Paint per on-canvas destination dot: each dot maps to one
            // module cell, so the loop is bounded by the canvas.
            let m = module as i64;
            if let Some((x0, x1, y0, y1)) = dest_bounds(bmp, ox, oy, b.rotation, fw, fh, 0, 0, fw, fh) {
                let stride = ((*width as usize) + 7) / 8;
                for by in y0..=y1 {
                    for bx in x0..=x1 {
                        let (u, v) = unrotate(bx - ox, by - oy, b.rotation, fw, fh);
                        if u < 0 || v < 0 {
                            continue;
                        }
                        let (cu, cv) = (u / m, v / m);
                        if cu >= *width as i64 || cv >= *height as i64 {
                            continue;
                        }
                        let byte = data.get(cv as usize * stride + (cu as usize >> 3)).copied().unwrap_or(0);
                        if byte & (0x80 >> (cu & 7)) != 0 {
                            bmp.set(bx as u32, by as u32, true);
                        }
                    }
                }
            }
        }
    }
    if let Some(fd) = fd {
        if b.line == b'Y' {
            let h = if b.height == 0 { 10 } else { b.height };
            paint_line(bmp, b, &fd, &g, h as i64, ox, oy, opts, warnings);
        }
        if b.interpret {
            let lc = line_cell(b, Some(&fd), opts.dpi) as i64;
            paint_line(bmp, b, &fd, &g, -lc, ox, oy, opts, warnings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::DefaultFontProvider;
    use crate::state::{Memory, PrinterMode};

    fn bc() -> BarcodeField {
        BarcodeField {
            x: 0,
            y: 0,
            symbology: b'Q',
            data: String::new(),
            module: 4,
            ratio: 30,
            height: 60,
            line: b'N',
            interpret: false,
            ucc: false,
            model: b'N',
            check: b'N',
            start: b'A',
            stop: b'A',
            qr_ec: b'Q',
            qr_mask: 7,
            dm_quality: 0,
            dm_format: 6,
            dm_aspect: 1,
            az_level: 0,
            az_menu: false,
            az_append: 1,
            pdf_security: 0,
            pdf_cols: 0,
            pdf_rows: 0,
            pdf_truncate: false,
            font: [b'A', b' '],
            font_height: 9,
            font_width: 5,
            font_name: None,
            rotation: Rotation::R0,
        }
    }

    /// Paint a pattern into a fresh bitmap with the bundled fonts.
    fn paint_into(pat: Pattern, f: BarcodeField, w: u32, h: u32) -> (Bitmap, Vec<String>) {
        let fonts = DefaultFontProvider::new();
        let opts = RenderOptions {
            dpi: 203,
            fonts: &fonts,
            memory: &Memory::default(),
            mode: &PrinterMode::default(),
        };
        let mut bmp = Bitmap::new(w, h);
        let mut warnings = Vec::new();
        paint(&mut bmp, &f, &pat, &opts, &mut warnings);
        (bmp, warnings)
    }

    /// `Pattern::Modules` blits each set module cell as modulexmodule dots:
    /// a 3x2 map (rows 101 / 010, MSB-first) at module 4 -> 12x8 dots with
    /// black cells (0,0), (2,0), (1,1).
    #[test]
    fn modules_blit_module_cells() {
        let pat = Pattern::Modules { width: 3, height: 2, data: vec![0b1010_0000, 0b0100_0000] };
        let (bmp, warnings) = paint_into(pat, bc(), 32, 16);
        assert!(warnings.is_empty(), "no warnings for a line-free 2D paint: {warnings:?}");
        for x in 0..32 {
            for y in 0..16 {
                let expect = (x < 4 || (8..12).contains(&x)) && y < 4
                    || (4..8).contains(&x) && (4..8).contains(&y);
                assert_eq!(bmp.get(x, y), expect, "module pixel {x},{y}");
            }
        }
    }

    /// Hostile module on a module map: u32::MAX cells can never be fully
    /// off-canvas when the field sits at the origin (the first cell covers
    /// content [0, 2^3^2)), so the hostile cases park the field off-canvas;
    /// the dest-pixel loop clamps and never panics or overflows. Also
    /// hostile map dimensions with a sane module.
    #[test]
    fn hostile_modules_never_panic() {
        let pat = Pattern::Modules { width: 2, height: 2, data: vec![0xFF, 0xFF] };
        let mut f = bc();
        f.module = u32::MAX;
        f.x = 100; // the first cell still legitimately covers the canvas at x=0
        let (bmp, _) = paint_into(pat.clone(), f, 8, 8);
        assert!((0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))), "nothing intersects");

        let mut f = bc();
        f.module = u32::MAX;
        f.x = 100;
        f.rotation = Rotation::R90;
        let (bmp, _) = paint_into(pat.clone(), f, 8, 8);
        assert!((0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))), "rotated nothing intersects");

        let pat = Pattern::Modules { width: u32::MAX, height: u32::MAX, data: vec![0xFF] };
        let mut f = bc();
        f.module = 2;
        f.x = 100;
        let (bmp, _) = paint_into(pat, f, 8, 8);
        assert!((0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))), "hostile dims clamped");
    }

    /// A truncated module payload reads as white - never a panic. The
    /// 9-wide map's single byte covers row 0's cells 0..7 (all 0xFF);
    /// cell 8 and rows 1..2 have no bytes and print white.
    #[test]
    fn modules_truncated_data_reads_white() {
        let pat = Pattern::Modules { width: 9, height: 3, data: vec![0xFF] };
        let (bmp, _) = paint_into(pat, bc(), 40, 16);
        assert!(bmp.get(0, 0), "first module of row 0 is in the payload");
        assert!(!bmp.get(32, 0), "row 0's 9th cell has no byte and prints white");
        // Module rows 1..2 (content v in [4, 12)) have no bytes at all.
        assert!((4..16).all(|y| (0..40).all(|x| !bmp.get(x, y))), "rows 1..2 have no bytes");
    }
}
