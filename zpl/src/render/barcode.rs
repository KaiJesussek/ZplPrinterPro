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
//! * **Interpret line** - `f=Y` prints the decoded field data (^FH
//!   applied) below the bars, `g=Y` (`interpret`) above them (both may be
//!   set). The default render is built-in font 0 at a cell of **12 dots
//!   per module**, capped at 120 (measured on the reference: module 2 ->
//!   24-dot cell, module 5 -> 60, module 10+ -> 120; independent of the
//!   bar height - the reference scales with `^BY w`, not `h`), centered
//!   on the symbol. The line sits 6 dots past the bar block edge. The
//!   reference also honors an explicit `^A ... ^BC` font request
//!   (cell = `^A` height, face = `^A` font) when it follows `^BY`; that
//!   ordering quirk (`^A` before `^BY` is dropped) is not modeled - the
//!   built-in font-0 default is, which every corpus case uses.
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
//!   degrees (clockwise)", ...). The R0 content plane is the **bar box**
//!   `[0,fw) x [0,h)` anchored at the ^FO origin; the interpret line hangs
//!   OUTSIDE it at `v` in `[h, h+cell)` (`f=Y`) or `[-cell, 0)`
//!   (`g=Y`), mapped through the same rotation (R90: `(u,v) ->
//!   (fh-1-v, u)` with `fh = h`). The line below the bars therefore lands
//!   on the left of an R90 symbol and on the right of a B270 symbol, the
//!   bars stay anchored at the origin for every rotation, and a `g=Y`
//!   line (above) rotates to the right - measured dot-for-dot on the
//!   reference (rotated 1D probes and the corpus 08 "ZEBRA" label).
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
use crate::fonts::{left_pad, matrix_pad, CapModel, FontData};
use crate::ir::{BarcodeField, OriginMode, Rotation};
use crate::render::text::{advance_at, draw_baseline, font_scale, scan_bitmap_ink};
use crate::render::{font_spec, place, resolve_font, Bitmap, RenderOptions};
use crate::state::PrinterMode;

/// Content-coordinate ceiling: any dot beyond this can never intersect the
/// 32000-dot canvas (even shifted by ^LH/^LS/^LT/^PO/^PM), and clamping to
/// it keeps every bounds sum well inside i64 (2*2^50 <= 2^51).
const MAX_CONTENT: u64 = 1 << 50;

/// The R0 content box of a painted barcode, in field-relative coordinates.
/// The rotation constants match text.rs's transform: the box maps
/// `u in [0, fw), v in [0, fh)` to the post-rotation plane. `fh` is the
/// BAR box height only - the interpret line hangs OUTSIDE the box (below
/// the bars at `v in [h, h+cell)`, above them at `[-cell, 0)`), so
/// rotations keep the bars origin-anchored and the line lands on the
/// rotated "below" side (left for R90, right for B270 - measured on the
/// reference; the label-level far-corner transforms (`^POI`/`^PM`) also
/// anchor on the bar box, verified against the reference).
pub(crate) struct Extent {
    /// Content width: quiet zones + symbol, or the module map, or the
    /// interpret line's advance (whichever is wider).
    pub fw: u32,
    /// Content height: the bar height, or the module map height (+ the
    /// QR's bottom blank band).
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

/// The interpret-line cell height in dots. The reference renders the
/// default line in built-in font 0 at a size proportional to the MODULE
/// width (^BY w), not the bar height (measured: module 1..9 scale the
/// glyph one-to-one, module 10+ hold; bar heights 10..800 at a fixed
/// module render identically; `^CF` is ignored, and the line is centered
/// on the symbol - see [`paint_line`]): the cell is **12 dots per
/// module**, capped at 120. An explicit `^A` font request sizes the line
/// instead (cell = `^A` height), but the reference drops it when placed
/// before `^BY`; that quirk is not modeled (see the module docs).
fn line_cell(b: &BarcodeField) -> u32 {
    let m = if b.module == 0 { 2 } else { b.module };
    m.saturating_mul(12).clamp(1, 120)
}

/// Total width of the interpret line at the font-0 line cell (proportional
/// advances; the field's fixed `^A` width does not apply - the line is a
/// font-0 field in the reference), clamped.
fn line_width(b: &BarcodeField, fd: &FontData, cell: u32) -> u32 {
    let mut total: u128 = 0;
    for ch in b.data.chars() {
        total += u128::from(advance_at(&fd.metrics, ch, cell));
    }
    cmax(total)
}

/// Measure the R0 content box of a barcode field. The box is the BAR
/// block (fw x bar-height) anchored at the field origin - interpret lines
/// hang outside it (see the module docs and [`Extent`]).
fn extent(b: &BarcodeField, pat: &Pattern, tw: u32) -> Extent {
    // ^BY/^BC defaults, mirror of bc::barcode_field (hand-built IR can
    // carry 0s that the parser never leaves behind).
    let module = if b.module == 0 { 2 } else { b.module };
    let height = if b.height == 0 { 10 } else { b.height };
    match pat {
        Pattern::Bars(bars) => {
            // The reference starts the first bar at the field origin
            // (verified on the corpus 1D cases 08/10/30 - no interior
            // quiet zone; the label margin is the quiet zone).
            let quiet = 0u128;
            let pat_w: u128 = bars.iter().map(|x| x.width as u128).sum();
            Extent {
                fw: cmax((2 * quiet + pat_w).max(u128::from(tw))),
                fh: cmax(height as u128),
            }
        }
        // The reference anchors the QR symbol `^BY` height below the
        // field origin (measured: corpus 11 default height 10 -> ink from
        // y+10; corpus 16 with ^BY2,3,60 -> y+60). The blank band is part
        // of the content box so rotation/mirror place the far corner
        // right. DataMatrix paints at the origin.
        Pattern::Modules { width, height, .. } => {
            // The ^BY band only precedes the symbol at ^FO; at ^FT the
            // reference drops it (the symbol hangs [-(S+3m), -3m) above
            // the origin, measured - see [`origin_anchor`]).
            let qr_band = if b.symbology == b'Q' && b.origin == OriginMode::Block {
                u128::from(if b.height == 0 { 10 } else { b.height })
            } else {
                0
            };
            Extent {
                fw: cmax(module as u128 * *width as u128),
                fh: cmax(module as u128 * *height as u128 + qr_band),
            }
        }
    }
}

/// The R0-rect origin shift for `^FT` (Dot) fields: the reference puts
/// the origin at the rotated "baseline" corner of the bar box, so the
/// painted rect's top-left = field origin - this anchor. The dest rect is
/// `fw` wide x `fh` tall for R0/R180 but `fh` x `fw` for R90/R270, so the
/// R270 anchor takes the axes swapped. `^FO` (Block) anchors the top-left
/// for every rotation (measured on the reference - four barcodes at one
/// ^FT origin tile the four quadrants around it: rects lie right+down,
/// right+down, left+down, left+up for N/R/I/B, a cross).
fn origin_anchor(b: &BarcodeField, fw: u32, fh: u32, two_d: bool) -> (i64, i64) {
    match (b.origin, b.rotation) {
        (OriginMode::Block, _) => (0, 0),
        // 2D symbols at `^FT`: the reference hangs the symbol ABOVE the
        // origin with its bottom margin clear - measured `^BQ` renders
        // [origin-(S+3m), origin-3m) (3 MODULES, mag-independent ratio),
        // while `^BXN` (DataMatrix) renders [origin-(S+3), origin-3)
        // (3 DOTS regardless of magnification - probed at mag 3 and 6);
        // every rotation (the QR is square, so no corner can be told).
        (OriginMode::Dot, _) if two_d => {
            let m = i64::from(if b.module == 0 { 2 } else { b.module });
            let gap = if b.symbology == b'Q' { 3 * m } else { 3 };
            (0, fh as i64 + gap)
        }
        (OriginMode::Dot, Rotation::R90) => (0, 0),
        (OriginMode::Dot, Rotation::R0) => (0, fh as i64),
        (OriginMode::Dot, Rotation::R180) => (fw as i64, 0),
        (OriginMode::Dot, Rotation::R270) => (fh as i64, fw as i64),
    }
}

/// The field's (y, height) in label space for the auto-height scan: the
/// bar block (plus the interpret line, when one prints and its font
/// resolves - a missing font hides only the line, not the bars).
pub(crate) fn footprint(b: &BarcodeField, _opts: &RenderOptions) -> Option<(u32, u32)> {
    let m = if b.module == 0 { 2 } else { b.module };
    let lc_span = (u32::from(b.line == b'Y') + u32::from(b.interpret)) * line_cell(b);
    // 2D symbols: the module map (+ the QR's ^BY band at ^FO) is the
    // content; at ^FT the reference drops the band and hangs the symbol
    // above the origin with its bottom 3 modules clear (see
    // [`origin_anchor`]).
    if let Pattern::Modules { height, .. } = crate::bc::barcode_field(b) {
        let s = (u64::from(m) * u64::from(height)).min(u64::from(u32::MAX)) as u32;
        if b.origin == OriginMode::Dot {
            let dy = u64::from(s) + 3 * u64::from(m);
            let y = (b.y as i64 - dy.min(i64::MAX as u64) as i64).max(0) as u32;
            return Some((y, s));
        }
        let shift = if b.symbology == b'Q' {
            if b.height == 0 {
                10
            } else {
                b.height
            }
        } else {
            0
        };
        return Some((b.y, s.saturating_add(shift)));
    }
    // 1D: each printed line adds its own cell (f=Y below, g=Y above),
    // hung off the rect. An ^FT field's rect hangs off the baseline
    // corner, so the scan starts at the rect top; for R270 the rotated
    // rect is `fw` tall (the pattern width), so that case derives the
    // span from the encoded pattern.
    let height = if b.height == 0 { 10 } else { b.height };
    let fw = if b.origin == OriginMode::Dot && b.rotation == Rotation::R270 {
        pattern_width(b)
    } else {
        0
    };
    let (_, d_y) = origin_anchor(b, fw, height, false);
    let span = if fw > 0 { fw } else { height };
    let y_off = if b.interpret { -(lc_span as i64) } else { 0 };
    let y = (b.y as i64 - d_y + y_off).max(0) as u32;
    Some((y, span.saturating_add(lc_span)))
}

/// The R0 pattern width in dots (1D bar runs sum; 2D = module map dots).
fn pattern_width(b: &BarcodeField) -> u32 {
    let m = if b.module == 0 { 2 } else { b.module };
    match crate::bc::barcode_field(b) {
        Pattern::Bars(bars) => bars
            .iter()
            .map(|x| u64::from(x.width))
            .sum::<u64>()
            .min(u64::from(u32::MAX)) as u32,
        Pattern::Modules { width, .. } => {
            (u64::from(m) * u64::from(width)).min(u64::from(u32::MAX)) as u32
        }
    }
}

/// Map a content dot through the field rotation (same orientation as
/// text.rs's transform: R = 90deg clockwise about the ^FO origin).
/// The `^POI`/`^PMY` label transform additionally mirrors BARCODE content
/// (measured on the reference: under both the bar sequence reverses and
/// the interpret line crosses to the opposite side, while text fields
/// stay upright). The mirror is expressed on the rect-relative axes so
/// every on-rect position follows the field unit.
#[derive(Clone, Copy, Default)]
struct Flip {
    x: bool,
    y: bool,
}

impl Flip {
    fn of(mode: &PrinterMode) -> Self {
        let poi = mode.orientation == 1;
        // POI = 180deg (both axes); PMY = horizontal; POI+PMY = vertical.
        Self {
            x: poi != mode.mirror,
            y: poi,
        }
    }

    fn fwd(self, fw: i64, fh: i64, bx: i64, by: i64) -> (i64, i64) {
        (
            if self.x { fw - 1 - bx } else { bx },
            if self.y { fh - 1 - by } else { by },
        )
    }
}

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
    flip: Flip,
) -> Option<(i64, i64, i64, i64)> {
    let (u1, v1) = (u0 + w, v0 + h); // bounded by the 2^50 clamp -> no overflow
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (u, v) in [(u0, v0), (u1, v0), (u0, v1), (u1, v1)] {
        let (bx, by) = rotate(u, v, r, fw, fh);
        let (bx, by) = flip.fwd(fw, fh, bx, by);
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
    flip: Flip,
) {
    let Some((x0, x1, y0, y1)) = dest_bounds(bmp, ox, oy, r, fw, fh, u0, v0, w, h, flip) else {
        return;
    };
    let (u1, v1) = (u0 + w, v0 + h);
    for by in y0..=y1 {
        for bx in x0..=x1 {
            let (mx, my) = flip.fwd(fw, fh, bx - ox, by - oy);
            let (u, v) = unrotate(mx, my, r, fw, fh);
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
    cell: u32,
    bar_w: i64,
    ox: i64,
    oy: i64,
    flip: Flip,
    _opts: &RenderOptions,
    warnings: &mut Vec<String>,
) {
    let Ok(mut face) = FontVec::try_from_vec(fd.ttf.clone()) else {
        warnings.push(
            "stored font bytes are not a usable TrueType font; interpretation line skipped".into(),
        );
        return;
    };
    let upem = face.units_per_em().unwrap_or(2048.0);
    let ascent = face.ascent_unscaled();
    if ascent <= 0.0 || upem <= 0.0 {
        warnings.push("font has unusable vertical metrics; interpretation line skipped".into());
        return;
    }
    // The line cell is the built-in [`line_cell`] height (12 dots per
    // module, capped); the field font's ^A size rules do not apply to the
    // default line (see the module docs).
    let eff = 1.0;
    let baseline = draw_baseline(&fd.metrics, eff, cell);
    // Bitmap-snap fonts can ink past the cell (font A renders 15-row
    // digits in a 9-row line); extend the draw band to the cap, like
    // text.rs's `draw_cell`.
    let draw_cell = if matches!(fd.metrics.cap, CapModel::Ascent) {
        cell
    } else {
        cell.max(baseline.saturating_add(1))
    };
    // Same fit as text.rs: cap-anchored for bitmap-style fonts, the
    // ascender fit for TrueType. The matrix top pad is a position term -
    // the face fits the cap alone (see text.rs paint_text).
    let pad_rows = matrix_pad(&fd.metrics.cap, cell);
    let scale = font_scale(&face, baseline - pad_rows, &fd.metrics);
    let x_scale = if matches!(fd.metrics.cap, CapModel::Ascent)
        || matches!(fd.metrics.cap, CapModel::Ratio { .. })
    {
        1.0
    } else {
        fd.metrics.x_scale
    };
    let px_scale = ab_glyph::PxScale {
        x: scale * x_scale,
        y: scale,
    };
    // Bitmap-style fonts ink one row below the metric baseline with no
    // side bearing (mirror of text.rs's paint).
    let bitmap = !matches!(fd.metrics.cap, CapModel::Ascent);
    let paint_baseline = if bitmap {
        baseline + 1 + pad_rows
    } else {
        baseline
    };
    let pad = left_pad(&fd.metrics.cap, cell) as f32;
    let fw = g.fw as i64;
    let fh = g.fh as i64;
    // The reference centers the interpretation line on the symbol (measured
    // case 08/21: "ZEBRA" and "12345678" sit centered under the bars). The
    // line's proportional advance box is centered in the bar block; a fixed
    // `^A` width does not apply to the built-in line.
    let line_w: i64 = b
        .data
        .chars()
        .map(|c| u128::from(advance_at(&fd.metrics, c, cell)) as i64)
        .sum();
    let center = ((bar_w - line_w) / 2).max(0);
    let mut char_x: i64 = center;
    for ch in b.data.chars() {
        let adv = advance_at(&fd.metrics, ch, cell) as i64;
        if adv > 0 {
            // Beyond the content ceiling no glyph can be visible; stop
            // accumulating instead of risking i64 overflow on hostile data.
            if char_x >= MAX_CONTENT as i64 {
                break;
            }
            let u0 = char_x.min(MAX_CONTENT as i64);
            let gid = face.glyph_id(ch);
            // Skip glyphs whose cell misses the canvas before rasterizing.
            let on_canvas = dest_bounds(
                bmp,
                ox,
                oy,
                b.rotation,
                fw,
                fh,
                u0,
                y_top,
                adv,
                draw_cell as i64,
                flip,
            )
            .is_some();
            if gid != GlyphId(0) && on_canvas {
                // Bitmap-style interpret lines ink from the cell edge:
                // position the outline so its ink starts at `u0`.
                let u0f = if bitmap {
                    let bare = gid.with_scale_and_position(px_scale, point(0.0, 0.0));
                    let bx0 = face
                        .outline_glyph(bare)
                        .map(|o| o.px_bounds().min.x.floor())
                        .unwrap_or(0.0);
                    u0 as f32 - bx0 + pad
                } else {
                    u0 as f32
                };
                let glyph = gid.with_scale_and_position(px_scale, point(u0f, 0.0));
                if let Some(outlined) = face.outline_glyph(glyph) {
                    let pbox = outlined.px_bounds();
                    scan_bitmap_ink(
                        &mut face,
                        gid,
                        px_scale,
                        point(u0f, 0.0),
                        &pbox,
                        |gx, gy_up| {
                            // Mirror of text.rs's pixels: baseline-relative, y up.
                            let gx = gx - u0;
                            let gy = paint_baseline as i64 + gy_up;
                            if gx < 0 || gx >= adv || gy < 0 || gy >= draw_cell as i64 {
                                return;
                            }
                            let (bx, by) = rotate(u0 + gx, y_top + gy, b.rotation, fw, fh);
                            let (bx, by) = flip.fwd(fw, fh, bx, by);
                            // Negative destinations are dropped by set().
                            bmp.set((bx + ox) as u32, (by + oy) as u32, true);
                        },
                    );
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
    // The interpretation line prints in the built-in font 0 at a cell
    // proportional to the bar height (the reference's convention - see
    // [`line_cell`]); the field's ^A font state is not consulted.
    let fd: Option<FontData> = if line {
        resolve_font(&font_spec(*b"0 ", &None), opts)
    } else {
        None
    };
    // The interpret line hangs outside the bar box (below/above the
    // bars), so the box is the bar block itself - only the line's WIDTH
    // feeds the extent (a line wider than the symbol widens the R0 box;
    // it is still centered on the bars - see [`paint_line`]).
    let tw = if line {
        fd.as_ref()
            .map(|f| line_width(b, f, line_cell(b)))
            .unwrap_or(0)
    } else {
        0
    };
    let g = extent(b, pat, tw);
    // `^FT` anchors the rect on the rotated baseline corner instead of
    // the top-left: `place` repositions the anchored content RECT, so
    // the label transforms (^LH/^LS/^LT/^POI/^PMY) move the rect as a
    // unit (a shift after `place` would not compose - ^POI rotates the
    // anchor vector the wrong way, measured off by 2x the anchor).
    let (d_x, d_y) = origin_anchor(b, g.fw, g.fh, matches!(pat, Pattern::Modules { .. }));
    let (ox, oy) = place(
        opts.mode,
        bmp.width as i64,
        bmp.height as i64,
        b.x as i64 - d_x,
        b.y as i64 - d_y,
        g.fw,
        g.fh,
    );
    let flip = Flip::of(opts.mode);
    let module = if b.module == 0 { 2 } else { b.module };
    let fw = g.fw as i64;
    let fh = g.fh as i64;
    match pat {
        Pattern::Bars(bars) => {
            let quiet = 0u128; // reference: first bar at the field origin
            let h = c50(if b.height == 0 { 10 } else { b.height } as u128);
            let mut off: u128 = quiet;
            for bar in bars {
                if bar.black {
                    // A per-bar height override (Planet's short bars)
                    // paints bottom-anchored inside the field band.
                    let bh = i64::from(bar.height.unwrap_or(h as u32));
                    paint_rect(
                        bmp,
                        ox,
                        oy,
                        b.rotation,
                        fw,
                        fh,
                        c50(off),
                        h - bh,
                        c50(bar.width as u128),
                        bh,
                        flip,
                    );
                }
                off += bar.width as u128;
            }
        }
        Pattern::Modules {
            width,
            height,
            data,
        } => {
            // One destination dot per module cell - bounded by the
            // canvas. The QR symbol paints `^BY` height below the field
            // origin (corpus measured: 11 -> y+10, 16 -> y+60); DataMatrix
            // paints at the origin. Module maps are compact row-major
            // bits (bit = row*width + col) - a padded ceil(width/8)-byte
            // stride drifts every row once width is not a multiple of 8
            // (it garbled the QR/DM rasters before the fix).
            // The QR's ^BY pre-band only exists at ^FO; under ^FT the
            // reference drops it (see [`origin_anchor`]).
            let shift = if b.symbology == b'Q' && b.origin == OriginMode::Block {
                i64::from(if b.height == 0 { 10 } else { b.height })
            } else {
                0
            };
            let m = module as i64;
            if let Some((x0, x1, y0, y1)) =
                dest_bounds(bmp, ox, oy, b.rotation, fw, fh, 0, 0, fw, fh, flip)
            {
                for by in y0..=y1 {
                    for bx in x0..=x1 {
                        let (mx, my) = flip.fwd(fw, fh, bx - ox, by - oy);
                        let (u, v) = unrotate(mx, my, b.rotation, fw, fh);
                        // The QR's blank band [0, shift) is not symbol
                        // content: truncating division would smear its
                        // low rows onto symbol row 0 (a 5-px ghost band
                        // above the reference's top edge), so skip it
                        // before mapping content dots to cells.
                        if u.is_negative() || v < shift {
                            continue;
                        }
                        let (cu, cv) = (u / m, (v - shift) / m);
                        if cu >= *width as i64 || cv >= *height as i64 {
                            continue;
                        }
                        let bit = cv as usize * *width as usize + cu as usize;
                        let byte = data.get(bit >> 3).copied().unwrap_or(0);
                        if !(byte >> (7 - bit % 8)).is_multiple_of(2) {
                            bmp.set(bx as u32, by as u32, true);
                        }
                    }
                }
            }
        }
    }
    if let Some(fd) = fd {
        let cell = line_cell(b);
        let bar_w: i64 = match pat {
            Pattern::Bars(bars) => bars.iter().map(|x| x.width as i64).sum(),
            Pattern::Modules { .. } => g.fw as i64,
        };
        // The line's ink starts ~6 dots past the bar block, in every
        // rotation (measured on the reference: R0 line ink at bar+6,
        // R90/B270 at origin-6/+6, R180 at -6; constant across modules
        // and `^A` heights) - the cell box hangs 6 dots off the bars.
        const LINE_GAP: i64 = 6;
        if b.line == b'Y' {
            let h = if b.height == 0 { 10 } else { b.height };
            paint_line(
                bmp,
                b,
                &fd,
                &g,
                h as i64 + LINE_GAP,
                cell,
                bar_w,
                ox,
                oy,
                flip,
                opts,
                warnings,
            );
        }
        if b.interpret {
            paint_line(
                bmp,
                b,
                &fd,
                &g,
                -(cell as i64) - LINE_GAP,
                cell,
                bar_w,
                ox,
                oy,
                flip,
                opts,
                warnings,
            );
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
            origin: OriginMode::Block,
            symbology: b'X', // neutral 2D: not QR, so module tests paint at the origin
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
            qr_mask: 0,
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
            font: *b"A ",
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
    /// a 3x2 map (rows 101 / 010 in the compact row-major bit layout -
    /// bit = row*width + col) at module 4 -> 12x8 dots with black cells
    /// (0,0), (2,0), (1,1).
    #[test]
    fn modules_blit_module_cells() {
        let pat = Pattern::Modules {
            width: 3,
            height: 2,
            data: vec![0b1010_1000], // row 0 bits 0..2 = 101, row 1 bits 3..5 = 010
        };
        let (bmp, warnings) = paint_into(pat, bc(), 32, 16);
        assert!(
            warnings.is_empty(),
            "no warnings for a line-free 2D paint: {warnings:?}"
        );
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
        let pat = Pattern::Modules {
            width: 2,
            height: 2,
            data: vec![0xFF, 0xFF],
        };
        let mut f = bc();
        f.module = u32::MAX;
        f.x = 100; // the first cell still legitimately covers the canvas at x=0
        let (bmp, _) = paint_into(pat.clone(), f, 8, 8);
        assert!(
            (0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))),
            "nothing intersects"
        );

        let mut f = bc();
        f.module = u32::MAX;
        f.x = 100;
        f.rotation = Rotation::R90;
        let (bmp, _) = paint_into(pat.clone(), f, 8, 8);
        assert!(
            (0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))),
            "rotated nothing intersects"
        );

        let pat = Pattern::Modules {
            width: u32::MAX,
            height: u32::MAX,
            data: vec![0xFF],
        };
        let mut f = bc();
        f.module = 2;
        f.x = 100;
        let (bmp, _) = paint_into(pat, f, 8, 8);
        assert!(
            (0..8).all(|x| (0..8).all(|y| !bmp.get(x, y))),
            "hostile dims clamped"
        );
    }

    /// A truncated module payload reads as white - never a panic. The
    /// 9-wide map's single byte covers row 0's cells 0..7 (all 0xFF);
    /// cell 8 and rows 1..2 have no bytes and print white.
    #[test]
    fn modules_truncated_data_reads_white() {
        let pat = Pattern::Modules {
            width: 9,
            height: 3,
            data: vec![0xFF],
        };
        let (bmp, _) = paint_into(pat, bc(), 40, 16);
        assert!(bmp.get(0, 0), "first module of row 0 is in the payload");
        assert!(
            !bmp.get(32, 0),
            "row 0's 9th cell has no byte and prints white"
        );
        // Module rows 1..2 (content v in [4, 12)) have no bytes at all.
        assert!(
            (4..16).all(|y| (0..40).all(|x| !bmp.get(x, y))),
            "rows 1..2 have no bytes"
        );
    }

    /// Module maps are compact row-major bits (bit = row*width + col),
    /// NOT byte-padded rows. A 10-wide map's second row starts at bit 10
    /// (byte 1, bit 2) and its col 8 sits at bit 18 - a ceil(width/8)
    /// byte stride would read byte 3 and print white (the bug that
    /// garbled the QR/DM rasters). Map: row 0 all dark, row 1 dark at
    /// col 8 only.
    #[test]
    fn modules_compact_rows_are_continuous() {
        let pat = Pattern::Modules {
            width: 10,
            height: 2,
            data: vec![0xFF, 0xFF, 0x20], // bits 0..=15 dark, bit 18 dark
        };
        let (bmp, _) = paint_into(pat, bc(), 40, 20);
        assert!(bmp.get(32, 5), "row 1 col 8 (bit 18) must paint");
        assert!(!bmp.get(36, 5), "row 1 col 9 has no bit and prints white");
    }

    /// The reference anchors the QR symbol `^BY` height below the field
    /// origin (corpus 11: height 10 -> ink from y+10; corpus 16: ^BY2,3,60
    /// -> y+60). The blank band must map to NO cells - truncating
    /// division used to smear the band's low rows onto symbol row 0.
    #[test]
    fn modules_qr_anchors_below_by_height() {
        let pat = Pattern::Modules {
            width: 2,
            height: 2,
            data: vec![0b1100_0000, 0b1100_0000],
        };
        let mut f = bc();
        f.symbology = b'Q';
        f.height = 10;
        f.module = 4;
        let (bmp, _) = paint_into(pat, f, 40, 40);
        assert!(!bmp.get(0, 0), "blank band above the symbol stays white");
        assert!(bmp.get(2, 12), "symbol row 0 paints at y + by_height");
        assert!(bmp.get(6, 12), "symbol remains on the field x origin");
        assert!(!bmp.get(9, 12), "row 0 col 1 is white in this map");
    }
}
