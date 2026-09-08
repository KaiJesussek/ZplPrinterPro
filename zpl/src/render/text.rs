//! Text-field painting (Task 13): `ab_glyph` glyph rasterization, `^FB`
//! block wrapping, field rotation (`^A`/`^FW`), `^FR` reverse video, and
//! the `^FT` baseline (typeset) origin.
//!
//! # Semantics (guide-anchored, reference-verified)
//!
//! * **Cell size** - `^A` p.60: `h` is "Character Height (in dots)",
//!   `w` is "width (in dots)"; "If the value is not given or a 0 (zero) is
//!   entered, the height or width is determined by the standard font
//!   matrix." So `font_height` is the cell height in dots (0 = font
//!   default scaled by dpi/203) and `font_width` **fixes every advance**
//!   (0 = proportional per-character advances). Metrics are the Task 11
//!   tables at the 203 dpi reference; `eff = h/cell` (explicit height) or
//!   `dpi/203` (default) scales advances, baseline, and glyph size.
//! * **Rotation** - `^FW` p.208 / `^A`: `R` = "rotated 90 degrees
//!   (clockwise)", `I` = "inverted 180 degrees", `B` = "read from the
//!   bottom up, 270 degrees"; `^FO` p.201 sets "the upper-left corner of
//!   the field area ... independent of the rotation". Verified against a
//!   reference ZPL renderer (dev-time oracle, nothing derived committed):
//!   the rotated **area's top-left stays on the field origin** (^FO), the
//!   drawing rotates inside it, glyphs are rotated 90deg clockwise *per
//!   cell*, and advances stack along the rotated flow axis. `^FT` p.205:
//!   "The typesetting origin of the field is fixed with respect to the
//!   contents of the field" - the baseline *point* of the (last, for ^FB)
//!   line stays on the origin, and the rotation formulas are re-anchored
//!   so the point `(pen 0, baseline)` of the last line lands on it.
//! * **Wrapping** - `^FB` p.186: word wrap at spaces, "Normal carriage
//!   returns, line feeds, and **word spaces at line breaks are discarded**";
//!   a word too long for the line is character-broken at the right edge
//!   (the reference hyphenates; this renderer does not - `ponytail:` add
//!   the hyphen pass when the block formatter grows one), `max_lines`
//!   caps (0 = unlimited), `line_spacing` 0 = font height, justification
//!   per line. `^FB` p.186: "If the value is less than the font width or
//!   not specified, the text does not print" - a block narrower than one
//!   character (width 0 included) is skipped with a warning. `^TB` shares
//!   the wrap path but has no such rule (its width is 1..label width).
//! * **Reverse video** - `^FR` p.203: field prints white on black; the
//!   documented approximation: fill the field's layout rectangle black,
//!   then paint the glyphs white (`ponytail:` ceiling - real Zebra
//!   inverts per dot within the field box; swap for a per-dot invert when
//!   overlapping fields demand it).
//!
//! # Safety
//!
//! The renderer never panics and never allocates unboundedly: glyph
//! rasterization is capped at [`MAX_GLYPH_CELL`] (the rasterizer
//! allocates a float per pixel of the glyph's pixel box), glyphs whose
//! destination rectangle misses the canvas are skipped before any
//! rasterization, all pen/origin arithmetic is i64, and every pixel
//! write goes through [`Bitmap::set`] (bounds-checked).

use ab_glyph::{point, Font, FontVec, GlyphId};

use crate::fonts::{
    baseline_for_cell, cap_units, left_pad, matrix_pad, CapModel, FontData, FontMetrics,
};
use crate::ir::{Justification, OriginMode, Rotation, TextField};
use crate::render::draw::fill_rect;
use crate::render::{Bitmap, RenderOptions};

/// Rasterization ceiling. `ab_glyph`'s rasterizer allocates one `f32`
/// coverage value per pixel of the glyph's box, so an uncapped cell
/// (`^A` h allows 32000) is a multi-gigabyte allocation. Cells above the
/// cap draw at the cap.
/// `ponytail:` per-scanline streaming would render true 32000-dot text;
/// no label in scope needs it, and the cap keeps hostile input cheap.
/// Shared with the barcode interpret-line painting (Task 14).
pub(crate) const MAX_GLYPH_CELL: u32 = 1024;

/// `^GS` graphic symbols (guide p.217): the field prints one built-in
/// symbol by the `^FD` letter instead of a text glyph.
fn symbol_for(c: char) -> char {
    match c {
        'A' => '\u{00A9}', // (C)
        'B' => '\u{00AE}', // (R)
        'C' => '\u{2122}', // (TM)
        _ => c,
    }
}

/// The drawn baseline row offset (rows below the cell top) for a field at
/// `eff` scale in a `cell`-dot cell. Bitmap-style built-ins anchor the cap
/// to the cell top (`cap_rows - 1`); TrueType (downloaded) fonts keep the
/// classic ascent fit from the guide baseline. Shared with the barcode
/// interpret-line painting.
pub(crate) fn draw_baseline(m: &FontMetrics, eff: f32, cell: u32) -> u32 {
    match m.cap {
        CapModel::Ascent => (m.baseline as f32 * eff).round().clamp(1.0, cell as f32) as u32,
        _ => baseline_for_cell(&m.cap, cell),
    }
}

/// Paint scale for a resolved face at a drawn `baseline` offset:
/// bitmap-style fonts scale so the face's cap is one row taller than the
/// baseline (the row above the cell top clips, so the ink spans rows
/// `[0, baseline]` - the bitmap convention where the baseline row is the
/// cap's last ink row); TrueType keeps the ascender fit. Shared with the
/// barcode interpret-line painting.
pub(crate) fn font_scale(face: &FontVec, baseline: u32, m: &FontMetrics) -> f32 {
    let upem = face.units_per_em().unwrap_or(2048.0);
    if matches!(m.cap, CapModel::Ascent) {
        return baseline as f32 * upem / face.ascent_unscaled();
    }
    (baseline + 1) as f32 * upem / cap_units(face, upem)
}

/// Advance of `c` at the reference metrics. The Task 11 deferred minor:
/// a 0 entry in `advances` (a glyph absent from the font's cmap) - and
/// any character outside the printable ASCII table the table covers -
/// advances like a **space**, never as `.notdef`.
/// Shared with the barcode interpret-line painting (Task 14).
pub(crate) fn ref_advance(m: &FontMetrics, c: char) -> u16 {
    let u = c as u32;
    let slot = if (32..=126).contains(&u) {
        m.advances.get((u - 32) as usize).copied().unwrap_or(0)
    } else {
        0
    };
    if slot == 0 {
        m.advances.first().copied().unwrap_or(0)
    } else {
        slot
    }
}

/// Drawn horizontal advance of `c` in dots at the drawn cell of `cell`
/// dots. Font 0's measured tenths table wins when present; every other
/// char scales the reference (cell-15) advance by `cell / cell_height`.
/// Shared with the barcode interpret-line painting (Task 14).
pub(crate) fn advance_at(m: &FontMetrics, c: char, cell: u32) -> u32 {
    if let Some(t) = m
        .tenths
        .and_then(|t| t.iter().find(|&&(ch, _)| ch == c).map(|&(_, v)| v))
        .filter(|&t| t > 0)
    {
        return (u32::from(t) * cell + 500) / 1000;
    }
    let ref15 = u32::from(ref_advance(m, c));
    let eff = cell as f32 / m.cell_height.max(1) as f32;
    (ref15 as f32 * eff).round().max(0.0) as u32
}

/// A wrapped text line with its total advance width.
struct Line {
    chars: Vec<char>,
    width: u64,
}

/// Font-0 block advance corrections at the drawn cell 24 (R4
/// calibration, measured from the parity block refs). The reference's
/// font-0 advances at 24 are the R1-fitted tenths minus a rounding edge
/// on six chars (least-squares over every ink-run gap of cases 07's two
/// `^FB` blocks; e.g. `x` measures 8 dots = its ink width, not the
/// tenths' 12). Every other height keeps the fitted tenths (verified
/// across the case-17 sweep).
fn font0_adv24(m: &FontMetrics, c: char) -> Option<u32> {
    if m.cell_height != 15 || m.tenths.is_none() {
        return None;
    }
    let v = match c {
        'l' => 5,
        'c' => 10,
        'o' => 11,
        'x' => 8,
        // `i`/`h`/`w`/`d` keep their tenths-adjacent LSQ values: i/h hold
        // 6/11 ("This", "the" deltas), w/d drop to 16/11 ("width"/"with"
        // in the left block measure 16/11 there).
        'w' => 16,
        'd' => 11,
        _ => return None,
    };
    Some(v)
}

/// Per-character advance at paint scale, honoring the `^A` width rule.
struct Advancer<'a> {
    metrics: &'a FontMetrics,
    eff: f32,
    /// Fixed advance in dots when `^A` w > 0; 0 = proportional.
    fixed: u64,
    /// True inside a `^FB` block drawn at cell 24: the measured block
    /// advance corrections apply (see [`font0_adv24`]).
    fb24: bool,
    /// `^FPg` additional inter-character gap, added to every char advance.
    gap: u64,
}

impl Advancer<'_> {
    fn dots(&self, c: char) -> u64 {
        if self.fb24 {
            if let Some(v) = font0_adv24(self.metrics, c) {
                return u64::from(v);
            }
        }
        if self.fixed > 0 {
            return self.fixed + self.gap;
        }
        let cell = ((self.metrics.cell_height as f32 * self.eff).round())
            .clamp(1.0, MAX_GLYPH_CELL as f32) as u32;
        u64::from(advance_at(self.metrics, c, cell)) + self.gap
    }
}

/// The resolved field layout: wrapped lines, scale, and extents in
/// unrotated (R0) space, all independent of the TTF bytes (metrics only).
struct FieldLayout {
    lines: Vec<Line>,
    cell: u64,
    /// The glyph draw band in rows. The ruler `cell` is the `^A` height
    /// (pitch, block height); bitmap-snap fonts can ink past it (font F
    /// renders 21-row caps in a 10-row cell), so the draw band extends to
    /// the cap's bottom row rather than clipping the glyph.
    draw_cell: u64,
    baseline: u64,
    pitch: u64,
    /// Justified line-start offsets (R0 space, relative to the origin).
    starts: Vec<u64>,
    /// Content box in R0 space: x in [box_left, box_right), y in [0, block_h).
    box_left: u64,
    box_right: u64,
    block_h: u64,
}

impl FieldLayout {
    /// A line's justified start offset.
    fn start(&self, i: usize) -> u64 {
        self.starts[i]
    }

    /// Total unrotated block height in dots.
    fn block_height(&self) -> u64 {
        self.block_h
    }
}

/// Greedy word wrap (guide `^FB`): break at spaces (discarded at breaks),
/// a word too long for the line is character-broken at the right edge.
fn wrap_words(data: &str, wrap_w: u64, adv: &Advancer<'_>, max_lines: u32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let mut cur_w: u64 = 0;
    let flush = |lines: &mut Vec<Line>, cur: &mut Vec<char>, cur_w: &mut u64| {
        if !cur.is_empty() || lines.is_empty() {
            let width = *cur_w;
            lines.push(Line {
                chars: core::mem::take(cur),
                width,
            });
        }
        *cur_w = 0;
    };
    let capped = |lines: &Vec<Line>| max_lines > 0 && lines.len() >= max_lines as usize;
    for word in data.split([' ', '\n']) {
        if capped(&lines) {
            break;
        }
        if word.is_empty() {
            // A space run: keep one trailing space if it still fits; a
            // space at a line break is discarded (guide ^FB).
            if !cur.is_empty() && cur_w + adv.dots(' ') <= wrap_w {
                cur.push(' ');
                cur_w += adv.dots(' ');
            }
            continue;
        }
        let word_w = word.chars().map(|c| adv.dots(c)).sum::<u64>();
        let sep = if cur.is_empty() { 0 } else { adv.dots(' ') };
        if cur_w + sep + word_w <= wrap_w {
            if sep > 0 {
                cur.push(' ');
                cur_w += sep;
            }
            cur.extend(word.chars());
            cur_w += word_w;
        } else {
            // Only a non-empty line ends here; an empty `cur` must not
            // become a leading blank line (default max_lines=1 would then
            // drop all of an over-wide opening word). The at-least-one-line
            // guarantee belongs to the final flush below.
            if !cur.is_empty() {
                flush(&mut lines, &mut cur, &mut cur_w);
            }
            if capped(&lines) {
                break;
            }
            // Place the word on the fresh line, character-breaking it if
            // it alone exceeds the width.
            for ch in word.chars() {
                if !cur.is_empty() && cur_w + adv.dots(ch) > wrap_w {
                    flush(&mut lines, &mut cur, &mut cur_w);
                    if capped(&lines) {
                        break;
                    }
                }
                cur.push(ch);
                cur_w += adv.dots(ch);
            }
        }
    }
    if max_lines == 0 || lines.len() < max_lines as usize {
        flush(&mut lines, &mut cur, &mut cur_w);
    }
    lines
}

/// The width of one character of this font at the field's scale: the fixed
/// `^A` width when set, else the widest glyph advance in the font's metric
/// table (the guide's ^FB p.186 "the font width" - a block narrower than
/// it cannot fit any character).
pub(crate) fn font_width_dots(t: &TextField, m: &FontMetrics, dpi: u32) -> u64 {
    // Bitmap-style fonts advance proportionally - the `^A` width rule
    // applies only to TrueType (downloaded) fonts, matching the
    // reference's native-advance bitmaps.
    if matches!(m.cap, CapModel::Ascent) && t.font_width > 0 {
        t.font_width as u64
    } else {
        let eff = if t.font_height > 0 {
            t.font_height as f32 / m.cell_height.max(1) as f32
        } else {
            dpi as f32 / 203.0
        };
        let cell = ((m.cell_height as f32 * eff).round()).clamp(1.0, MAX_GLYPH_CELL as f32) as u32;
        if let Some(t) = m.tenths {
            // widest advance of the font-0 table at the drawn cell
            let widest = t
                .iter()
                .map(|&(_, v)| (u32::from(v) * cell + 500) / 1000)
                .max()
                .unwrap_or(1);
            return u64::from(widest.max(1));
        }
        let widest = m.advances.iter().copied().max().unwrap_or(1).max(1);
        ((widest as f32 * eff).round()).max(1.0) as u64
    }
}

/// Compute the field layout from metrics; `None` when the field must not
/// print (a `^FB` block narrower than one character - guide p.186: "If
/// the value is less than the font width or not specified, the text does
/// not print").
fn layout(t: &TextField, m: &FontMetrics, dpi: u32) -> Option<FieldLayout> {
    let eff = if t.font_height > 0 {
        t.font_height as f32 / m.cell_height.max(1) as f32
    } else {
        dpi as f32 / 203.0
    };
    let cell = ((m.cell_height as f32 * eff).round()).clamp(1.0, MAX_GLYPH_CELL as f32) as u64;
    let baseline = draw_baseline(m, eff, cell as u32) as u64;
    let draw_cell = if matches!(m.cap, CapModel::Ascent) {
        cell
    } else {
        cell.max(baseline.saturating_add(1))
    };
    let spacing = t.block.as_ref().map(|b| b.line_spacing).unwrap_or(0);
    let mut pitch = (cell as i64 + spacing as i64).max(1) as u64;
    if matches!(t.direction, crate::ir::TextDirection::V) {
        pitch = pitch.saturating_add(t.char_gap as u64);
    }
    // Bitmap-style fonts ignore the `^A` width (they advance at native
    // proportional widths); TrueType (downloaded) fonts honor it.
    let fixed = if matches!(m.cap, CapModel::Ascent) && t.font_width > 0 {
        t.font_width as u64
    } else {
        0
    };
    let adv = Advancer {
        metrics: m,
        eff,
        fixed,
        fb24: t.block.is_some() && cell == 24,
        gap: t.char_gap as u64,
    };
    let font_w = font_width_dots(t, m, dpi);
    let (wrap_w, justification, max_lines) = match &t.block {
        Some(b) => {
            if b.fb && (b.width as u64) < font_w {
                // Guide ^FB p.186: "If the value is less than the font
                // width or not specified, the text does not print." A
                // block narrower than one character prints nothing;
                // width 0 and an omitted width are the default case.
                return None;
            }
            (b.width as u64, b.justification.clone(), b.max_lines)
        }
        None => (u64::MAX, Justification::Left, 0),
    };
    let mut data = if t.symbol {
        symbol_for(t.data.chars().next().unwrap_or(' ')).to_string()
    } else {
        t.data.clone()
    };
    // ^FPV stacks the characters top-to-bottom (one per line); ^FPR
    // prints them right-to-left (the character order reverses).
    if matches!(t.direction, crate::ir::TextDirection::R) {
        data = data.chars().rev().collect();
    }
    let raw_lines: Vec<Line> = if matches!(t.direction, crate::ir::TextDirection::V) {
        data.chars()
            .map(|c| Line {
                chars: vec![c],
                width: adv.dots(c),
            })
            .collect()
    } else {
        wrap_words(&data, wrap_w, &adv, max_lines)
    };
    let last_i = raw_lines.len().saturating_sub(1);
    let mut lines = Vec::with_capacity(raw_lines.len());
    let mut starts = Vec::with_capacity(raw_lines.len());
    let mut box_left = u64::MAX;
    let mut box_right = 0u64;
    for (i, l) in raw_lines.into_iter().enumerate() {
        let w = l.width;
        let start = match justification {
            Justification::Left => 0,
            // The ref measures a right-justified line with the trailing
            // space of the wrapped word included (measured: non-last
            // lines sit one space closer to the box edge than a bare
            // content measure) - the last line has none.
            Justification::Right => {
                let tail = if i == last_i { 0 } else { adv.dots(' ') };
                wrap_w.saturating_sub(w.saturating_add(tail))
            }
            Justification::Center => (wrap_w.saturating_sub(w)) / 2,
        };
        box_left = box_left.min(start);
        box_right = box_right.max(start.saturating_add(w));
        starts.push(start);
        lines.push(l);
    }
    if lines.is_empty() {
        lines.push(Line {
            chars: Vec::new(),
            width: 0,
        });
        starts.push(0);
        box_left = 0;
        box_right = 0;
    }
    let block_h = (lines.len() as u64 - 1) * pitch + cell;
    Some(FieldLayout {
        lines,
        cell,
        draw_cell,
        baseline,
        pitch,
        starts,
        box_left,
        box_right,
        block_h,
    })
}

/// Extents for `place`/auto height: (y-top in label space, x extent, y
/// extent) of the field in its own rotation. `y` may drop below the field
/// origin for `^FT` (block grows up from the baseline).
fn extents(t: &TextField, l: &FieldLayout) -> (i64, u64, u64) {
    let block_h = l.block_height();
    let last_pitch = (l.lines.len() as u64 - 1) * l.pitch;
    match t.rotation {
        Rotation::R0 => {
            let y = if t.origin == OriginMode::Dot {
                t.y as i64 - (l.baseline + last_pitch) as i64
            } else {
                t.y as i64
            };
            (y, l.box_right, block_h)
        }
        Rotation::R180 => {
            // R180 ^FT mirrors the block so the baseline point sits at the
            // origin: content spans [baseline - cell + 1, last_pitch +
            // baseline) - BELOW the origin, not above it like R0. The
            // re-anchor uses the paint baseline (glyphs ink one row below
            // the metric baseline for bitmap-style fonts).
            let y = if t.origin == OriginMode::Dot {
                t.y as i64 + l.baseline as i64 + 1 - l.cell as i64 + 1
            } else {
                t.y as i64
            };
            (y, l.box_right, block_h)
        }
        Rotation::R90 | Rotation::R270 => {
            let y = t.y as i64 + l.box_left as i64;
            let w = (l.lines.len() as u64 - 1) * l.pitch + l.cell;
            (y, w, l.box_right - l.box_left)
        }
    }
}

/// The field's (y, height) in label space for the auto-height scan:
/// nothing when the field doesn't print or its font is unavailable.
pub(crate) fn text_footprint(t: &TextField, opts: &RenderOptions) -> Option<(u32, u32)> {
    let spec = crate::render::field_font_spec(t);
    let fd = crate::render::resolve_font(&spec, opts)?;
    let l = layout(t, &fd.metrics, opts.dpi)?;
    let (y, _fw, fh) = extents(t, &l);
    let y = y.max(0) as u32;
    Some((y, fh as u32))
}

/// The formatted width in dots (pre-rotation), for ^FT's auto-origin
/// runner ("position after the last formatted text field", guide p.205).
pub(crate) fn text_width(t: &TextField, opts: &RenderOptions) -> Option<u32> {
    let spec = crate::render::field_font_spec(t);
    let fd = crate::render::resolve_font(&spec, opts)?;
    let l = layout(t, &fd.metrics, opts.dpi)?;
    let (_y, fw, _fh) = extents(t, &l);
    Some(fw as u32)
}

/// Paint a text field into the bitmap. Warnings are appended for fields
/// that cannot print (a ^FB block narrower than one character, unusable
/// TTF).
pub(crate) fn paint_text(
    bmp: &mut Bitmap,
    t: &TextField,
    fd: &FontData,
    opts: &RenderOptions,
    warnings: &mut Vec<String>,
) {
    let Some(l) = layout(t, &fd.metrics, opts.dpi) else {
        if let Some(b) = t.block.as_ref().filter(|b| b.fb) {
            warnings.push(format!(
                "text block width {} smaller than one character; field not printed (guide ^FB)",
                b.width
            ));
        }
        return;
    };
    if l.lines.iter().all(|ln| ln.chars.is_empty()) {
        return; // empty field: nothing to paint, nothing to warn about
    }
    let Ok(mut face) = FontVec::try_from_vec(fd.ttf.clone()) else {
        warnings
            .push("stored font bytes are not a usable TrueType font; field skipped".to_string());
        return;
    };
    let upem = face.units_per_em().unwrap_or(2048.0);
    let ascent = face.ascent_unscaled();
    if ascent <= 0.0 || upem <= 0.0 {
        warnings.push("font has unusable vertical metrics; field skipped".to_string());
        return;
    }
    // Scale so the face's cap height spans the cap-anchored baseline
    // (bitmap-style fonts; downloaded TrueType keeps the ascent fit) -
    // the drawn cell then matches the reference's top-anchored cap. The
    // matrix faces' top pad is a position offset, not a scale term: the
    // face must fit the cap alone or the pad would enlarge the glyph.
    let pad_rows = matrix_pad(&fd.metrics.cap, l.cell as u32);
    let scale = font_scale(&face, l.baseline as u32 - pad_rows, &fd.metrics);
    // Bitmap faces stretch horizontally by their calibrated x-scale (the
    // reference matrices print wider than the source face); the scalable
    // font 0 and downloaded TrueType keep the face's own proportions.
    let x_scale = if matches!(fd.metrics.cap, CapModel::Ascent)
        || matches!(fd.metrics.cap, CapModel::Ratio { .. })
    {
        1.0
    } else {
        fd.metrics.x_scale
    };
    // The reference matrix faces G/V grow sub-linearly in width above
    // their native cell (measured ~1.5x per doubling); the face's width
    // power damps both the advance (`eff`) and the painted ink.
    let eff_raw = if t.font_height > 0 {
        t.font_height as f32 / fd.metrics.cell_height.max(1) as f32
    } else {
        opts.dpi as f32 / 203.0
    };
    let eff = eff_raw.powf(fd.metrics.width_pow);
    let x_scale = x_scale * eff_raw.powf(fd.metrics.width_pow - 1.0);
    let adv = Advancer {
        metrics: &fd.metrics,
        eff,
        fixed: if matches!(fd.metrics.cap, CapModel::Ascent) && t.font_width > 0 {
            t.font_width as u64
        } else {
            0
        },
        fb24: t.block.is_some() && l.cell == 24,
        gap: t.char_gap as u64,
    };

    let n_lines = l.lines.len() as i64;
    let last_pitch = (n_lines - 1) * l.pitch as i64;
    // Bitmap-style fonts draw the glyph's ink one row lower than the
    // metric baseline (the bitmap "baseline row" is the cap's last ink
    // row) and anchored at the advance column's left edge (no side
    // bearing) - the reference bitmaps ink from their cell edge.
    let paint_baseline = if matches!(fd.metrics.cap, CapModel::Ascent) {
        l.baseline as i64
    } else {
        l.baseline as i64 + 1
    };
    let bitmap_ink = !matches!(fd.metrics.cap, CapModel::Ascent);
    // The ref's ink sits at the cell edge plus the glyph's own natural
    // left bearing (measured: M@24 +2, Z@24 +1, K@28 +2, Z@10 +0) - not
    // at a constant `(cell+8)/16`. Keep the constant for the other caps
    // (no per-glyph ref geometry yet). `ponytail:` drop the constant
    // once the remaining faces have probe refs.
    let pad =
        if t.block.is_some() && matches!(fd.metrics.cap, CapModel::Ratio { .. }) && l.cell == 24 {
            // `^FB` blocks at 24 ink one dot left of `^FT` text at the same
            // size (ref: block Z@21, a@21, j@21 vs ^FT H@22, B@22) - the
            // block adds its own 1-dot inset on top of the field origin.
            1
        } else {
            left_pad(&fd.metrics.cap, l.cell as u32) as i64
        };
    // The matrix top pad puts the cap *below* the cell top: the anchor
    // climbs by the pad rows the scale no longer carries.
    let paint_baseline = paint_baseline
        + if bitmap_ink {
            matrix_pad(&fd.metrics.cap, l.cell as u32) as i64
        } else {
            0
        };
    // The last line's first character advance: the ^FT R270 re-anchor
    // measures its baseline point from it.
    let first_adv: i64 = l
        .lines
        .last()
        .and_then(|ln| ln.chars.first())
        .map(|c| adv.dots(*c) as i64)
        .unwrap_or(0);

    // `^FT` R270 measures its baseline re-anchor from the last line's
    // first character advance; the shrink term is that advance minus one
    // (the advance column's last row).
    let ft_adv = first_adv.saturating_sub(1);
    // `^FT` anchors the content rect on the rotated baseline corner;
    // `place` repositions that RECT, so the label transforms
    // (^LH/^LS/^LT/^POI/^PMY) move it as a unit (a post-place shift
    // would not compose - ^POI mirrors the anchor the wrong way). The
    // paint functions keep their origin-relative offsets, so they get
    // the placed rect minus its top-left offset.
    let (rx, ry, rw, rh) = field_rect(t, &l, l.baseline as i64, paint_baseline, ft_adv);
    let (ox, oy) = crate::render::place(
        opts.mode,
        bmp.width as i64,
        bmp.height as i64,
        t.x as i64 + rx,
        t.y as i64 + ry,
        rw as u32,
        rh as u32,
    );
    let (ox, oy) = (ox - rx, oy - ry);
    if t.reverse {
        let paint_baseline = if matches!(fd.metrics.cap, CapModel::Ascent) {
            l.baseline as i64
        } else {
            l.baseline as i64 + 1
        } + if matches!(fd.metrics.cap, CapModel::Matrix { .. }) {
            matrix_pad(&fd.metrics.cap, l.cell as u32) as i64
        } else {
            0
        };
        paint_reverse_box(
            bmp,
            ox,
            oy,
            &l,
            t,
            ft_adv,
            l.baseline as i64,
            paint_baseline,
        );
    }

    let glyph_black = !t.reverse; // reverse: white glyphs on the black box
    for (li, line) in l.lines.iter().enumerate() {
        let start = l.start(li) as i64;
        let line_top = li as i64 * l.pitch as i64;
        let mut pen: i64 = 0;
        for &ch in &line.chars {
            let gid = face.glyph_id(ch);
            let ch_adv = adv.dots(ch) as i64;
            if gid != GlyphId(0) {
                paint_glyph(
                    bmp,
                    &mut face,
                    scale,
                    x_scale,
                    gid,
                    ch_adv,
                    ox,
                    oy,
                    start + pen,
                    line_top,
                    t.rotation,
                    t.origin,
                    l.draw_cell as i64,
                    paint_baseline,
                    l.box_right as i64,
                    l.block_h as i64,
                    last_pitch,
                    ft_adv,
                    glyph_black,
                    bitmap_ink,
                    pad,
                );
            }
            pen = pen.saturating_add(ch_adv);
        }
    }
}

/// The field's content rect relative to the origin (label space), per
/// rotation/origin: `[rx, rx+rw) x [ry, ry+rh)`. `^FT` (Dot) anchors the
/// rotated baseline corner; `place` repositions this rect, so the label
/// transforms compose. Shared by the reverse-video box and the placement
/// anchor in [`paint_text`].
fn field_rect(
    t: &TextField,
    l: &FieldLayout,
    baseline: i64,
    paint_baseline: i64,
    ft_adv: i64,
) -> (i64, i64, i64, i64) {
    let box_w = (l.box_right - l.box_left) as i64;
    let n_lines = l.lines.len() as i64;
    let block_w = (n_lines - 1) * l.pitch as i64 + l.cell as i64;
    let cell = l.cell as i64;
    let last_pitch = (n_lines - 1) * l.pitch as i64;
    match (t.rotation, t.origin) {
        (Rotation::R0, OriginMode::Block) => (l.box_left as i64, 0, box_w, l.block_h as i64),
        (Rotation::R0, OriginMode::Dot) => (
            l.box_left as i64,
            -(baseline + last_pitch),
            box_w,
            l.block_h as i64,
        ),
        (Rotation::R90, OriginMode::Block) => (0, l.box_left as i64, block_w, box_w),
        (Rotation::R90, OriginMode::Dot) => (
            paint_baseline - last_pitch - cell + 1,
            l.box_left as i64,
            block_w,
            box_w,
        ),
        (Rotation::R180, OriginMode::Block) => (0, 0, box_w, l.block_h as i64),
        (Rotation::R180, OriginMode::Dot) => (
            -(l.box_right as i64 - 1),
            paint_baseline - cell + 1,
            box_w,
            l.block_h as i64,
        ),
        (Rotation::R270, OriginMode::Block) => (0, l.box_left as i64, block_w, box_w),
        (Rotation::R270, OriginMode::Dot) => (
            -(paint_baseline + last_pitch),
            l.box_left as i64 - ft_adv,
            block_w,
            box_w,
        ),
    }
}

/// Fill the field's layout rectangle black (reverse video background),
/// in rotated space.
#[allow(clippy::too_many_arguments)]
fn paint_reverse_box(
    bmp: &mut Bitmap,
    ox: i64,
    oy: i64,
    l: &FieldLayout,
    t: &TextField,
    ft_adv: i64,
    // Metric baseline (R0 block anchor; the reference's `^FT` point is
    // the cap's last ink row).
    baseline: i64,
    // Paint baseline (rotated arms: the glyphs ink one row below the
    // metric baseline for bitmap-style fonts).
    paint_baseline: i64,
) {
    let (x, y, w, h) = field_rect(t, l, baseline, paint_baseline, ft_adv);
    fill_rect(bmp, ox + x, oy + y, w, h, b'B');
}

/// A dot of the upright cell maps through rotation (and the `^FT`
/// baseline-point re-anchor) to a bitmap coordinate. Coordinates are
/// relative to the placed origin: `gx` in the character's advance
/// column, `gy` rows down from the cell top.
#[allow(clippy::too_many_arguments)]
fn rotate_point(
    gx: i64,
    gy: i64,
    line_top: i64,
    cell_left: i64,
    rotation: Rotation,
    origin: OriginMode,
    cell: i64,
    baseline: i64,
    box_right: i64,
    block_h: i64,
    last_pitch: i64,
    pen_adv: i64,
    first_adv_total: i64,
) -> (i64, i64) {
    let ft = origin == OriginMode::Dot;
    match rotation {
        Rotation::R0 => {
            let y = if ft {
                line_top - last_pitch + gy - baseline
            } else {
                line_top + gy
            };
            (cell_left + gx, y)
        }
        Rotation::R90 => {
            // Upright top row (gy = 0) faces right after the rotation;
            // reading order flows +y.
            let x = if ft {
                line_top - last_pitch + baseline - gy
            } else {
                line_top + cell - 1 - gy
            };
            (x, cell_left + gx)
        }
        Rotation::R180 => {
            // Cells flow right-to-left within the block ("inverted 180").
            let x = if ft {
                -cell_left - gx
            } else {
                box_right - 1 - cell_left - gx
            };
            let y = if ft {
                last_pitch - line_top + baseline - gy
            } else {
                block_h - 1 - line_top - gy
            };
            (x, y)
        }
        Rotation::R270 => {
            // "Read from the bottom up": reading order flows -y, glyphs
            // 90deg counterclockwise inside the cell.
            let x = if ft {
                line_top - last_pitch + gy - baseline
            } else {
                line_top + gy
            };
            let y = cell_left + pen_adv - 1 - gx - if ft { first_adv_total } else { 0 };
            (x, y)
        }
    }
}

/// The dot coverage needed to print ink (fraction of the dot the outline
/// covers). 0.5 = area-majority 1-bit rendering; the 2x supersample makes
/// the measurement exact at the dot grid (a raw threshold at 1x samples
/// the same area, phase wobbles included). Kept as a named constant so
/// the R1 calibration is a one-place change.
const BITMAP_INK_THRESHOLD: f32 = 0.5;

/// Scan a glyph's ink as 1-bit dots on the pen-anchored dot grid (the
/// reference prints bitmap ink: a dot is black when the outline covers
/// at least [`BITMAP_INK_THRESHOLD`] of it - no anti-aliasing). The
/// outline is rasterized at 2x resolution and each dot's sample block
/// is accumulated back to one per-dot coverage; the supersample shares
/// the 1x fractional origin (`b`), so the ink box (anchored at the
/// pen/pad) cannot shift by even one dot, and the summed samples
/// partition the dot, so `0.5` here is exactly the 1x coverage rule.
///
/// `put` receives pixel coordinates in the same convention as
/// `px_bounds()` pixels: `floor(bounds.min) + dot` for each inked dot.
/// Shared with the barcode interpret-line painting (Task 14).
pub(crate) fn scan_bitmap_ink<F>(
    face: &mut FontVec,
    gid: GlyphId,
    px_scale: ab_glyph::PxScale,
    pos: ab_glyph::Point,
    b: &ab_glyph::Rect,
    mut put: F,
) where
    F: FnMut(i64, i64),
{
    let glyph = gid.with_scale_and_position(
        ab_glyph::PxScale {
            x: px_scale.x * 2.0,
            y: px_scale.y * 2.0,
        },
        point(pos.x * 2.0, pos.y * 2.0),
    );
    let Some(out) = face.outline_glyph(glyph) else {
        return;
    };
    // Which half of each dot the 2x pixel grid starts in (0 or 1): the
    // 2x grid is anchored at `floor(2*b.min)`, one dot at `floor(b.min) +
    // i` spans 2x pixels `2i - f` and `2i - f + 1` with `f =
    // floor(2*frac(b.min))`, so dot = `(px + f) / 2`.
    let fx = ((b.min.x * 2.0).floor() - b.min.x.floor() * 2.0) as i64;
    let fy = ((b.min.y * 2.0).floor() - b.min.y.floor() * 2.0) as i64;
    let ox = b.min.x.floor() as i64;
    let oy = b.min.y.floor() as i64;
    let mut cur: Option<(i64, i64, f32)> = None;
    out.draw(|px, py, cov| {
        if cov <= 0.0 {
            return;
        }
        let dx = (px as i64 + fx) / 2;
        let dy = (py as i64 + fy) / 2;
        match cur.take() {
            Some((cdx, cdy, acc)) if cdx == dx && cdy == dy => {
                cur = Some((dx, dy, acc + cov));
            }
            prev => {
                if let Some((pxx, pyy, acc)) = prev {
                    if acc >= BITMAP_INK_THRESHOLD {
                        put(ox + pxx, oy + pyy);
                    }
                }
                cur = Some((dx, dy, cov));
            }
        }
    });
    if let Some((pxx, pyy, acc)) = cur {
        if acc >= BITMAP_INK_THRESHOLD {
            put(ox + pxx, oy + pyy);
        }
    }
}

/// Rasterize one glyph and map it through the field rotation. The glyph
/// is skipped entirely (no raster allocation) when its destination cell
/// misses the canvas; every write goes through [`Bitmap::set`].
#[allow(clippy::too_many_arguments)]
fn paint_glyph(
    bmp: &mut Bitmap,
    face: &mut FontVec,
    scale: f32,
    x_scale: f32,
    gid: GlyphId,
    pen_adv: i64,
    ox: i64,
    oy: i64,
    cell_left: i64,
    line_top: i64,
    rotation: Rotation,
    origin: OriginMode,
    cell: i64,
    baseline: i64,
    box_right: i64,
    block_h: i64,
    last_pitch: i64,
    // `^FT` R270 baseline re-anchor shrink: last line's first char
    // advance minus one.
    first_adv: i64,
    black: bool,
    // Bitmap-style: anchor the ink at the advance column's left edge
    // (drop the face's side bearing, like the reference bitmaps).
    bitmap_ink: bool,
    // Bitmap cell left pad (the reference inks `pad` dots in from the
    // cell edge).
    pad: i64,
) {
    // The mapping is affine over the cell, so its destination bounds are
    // the min/max of the four corners.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (gx, gy) in [(0i64, 0i64), (pen_adv, 0i64), (0, cell), (pen_adv, cell)] {
        let (bx, by) = rotate_point(
            gx, gy, line_top, cell_left, rotation, origin, cell, baseline, box_right, block_h,
            last_pitch, pen_adv, first_adv,
        );
        min_x = min_x.min(bx);
        min_y = min_y.min(by);
        max_x = max_x.max(bx);
        max_y = max_y.max(by);
    }
    // `rotate_point` coordinates are relative to the placed origin; the
    // destination bounds shift by (ox, oy) like every painted pixel.
    min_x += ox;
    min_y += oy;
    max_x += ox;
    max_y += oy;
    if max_x < 0 || max_y < 0 || min_x >= bmp.width as i64 || min_y >= bmp.height as i64 {
        return;
    }

    // Draw the glyph at the pen: its pixels are pen-absolute, so the
    // cell column is `pixel - cell_left` for every character. Bitmap-style
    // text positions the outline so its ink starts exactly at the pen
    // (the reference bitmaps ink from their cell edge; the face's side
    // bearing would otherwise shift the ink by its fractional column).
    // The scale is non-uniform for the stretched bitmap families: the
    // glyph's vertical scale spans the cap, its horizontal scale carries
    // the calibrated x_scale.
    let px_scale = ab_glyph::PxScale {
        x: scale * x_scale,
        y: scale,
    };
    let (pos, outlined) = if bitmap_ink {
        let bare = gid.with_scale_and_position(px_scale, point(0.0, 0.0));
        let bx0 = face
            .outline_glyph(bare)
            .map(|o| o.px_bounds().min.x.floor())
            .unwrap_or(0.0);
        let pos = point(cell_left as f32 - bx0 + pad as f32, 0.0);
        let Some(outlined) = face.outline_glyph(gid.with_scale_and_position(px_scale, pos)) else {
            return;
        };
        (pos, outlined)
    } else {
        let pos = point(cell_left as f32, 0.0);
        let Some(outlined) = face.outline_glyph(gid.with_scale_and_position(px_scale, pos)) else {
            return;
        };
        (pos, outlined)
    };
    let b = outlined.px_bounds();
    scan_bitmap_ink(face, gid, px_scale, pos, &b, |gx_w, gy_up| {
        let cell_rel_x = gx_w - cell_left;
        // Display rows increase downward from the baseline (`gy_up` is the
        // baseline-relative pixel row), so the row from the cell top is
        // baseline + gy_up (the cap-anchored top reaches the cell top).
        let cell_rel_y = baseline + gy_up;
        if cell_rel_x < 0 || cell_rel_x >= pen_adv || cell_rel_y < 0 || cell_rel_y >= cell {
            return;
        }
        let (bx, by) = rotate_point(
            cell_rel_x, cell_rel_y, line_top, cell_left, rotation, origin, cell, baseline,
            box_right, block_h, last_pitch, pen_adv, first_adv,
        );
        bmp.set((bx + ox) as u32, (by + oy) as u32, black);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::TextBlock;

    /// A zebra-0-shaped table: reference cell 15, ratio cap, uniform
    /// 8-dot advances, a (hollow) tenths table present.
    fn zebra0_metrics() -> FontMetrics {
        FontMetrics {
            cell_height: 15,
            baseline: 11,
            advances: vec![8u16; 95],
            tenths: Some(&[]),
            cap: CapModel::Ratio { percent: 100 },
            x_scale: 1.0,
            width_pow: 1.0,
        }
    }

    /// R4: the cell-24 advance corrections apply only inside an `^FB`
    /// block at the drawn cell 24; the same charset elsewhere keeps the
    /// scale of the base table (font 0 at 24: `x` = 8 inside blocks vs
    /// `(8 * 24 / 15).round() = 13` outside).
    #[test]
    fn fb24_corrections_apply_only_inside_blocks() {
        let m = zebra0_metrics();
        let block = Advancer {
            metrics: &m,
            eff: 24.0 / 15.0,
            fixed: 0,
            fb24: true,
            gap: 0,
        };
        assert_eq!(
            block.dots('x'),
            8,
            "correction table wins inside a 24-dot block"
        );
        assert_eq!(block.dots('d'), 11);
        let plain = Advancer {
            metrics: &m,
            eff: 24.0 / 15.0,
            fixed: 0,
            fb24: false,
            gap: 0,
        };
        assert_eq!(plain.dots('x'), 13, "outside blocks the base table scales");
    }

    /// R4: a right-justified non-last line is measured with the trailing
    /// space of the wrapped word (ref: case 07 B2 starts 8 dots left of
    /// the content-only measure); the last line is measured bare.
    #[test]
    fn right_justify_measures_trailing_space() {
        let m = FontMetrics {
            cell_height: 15,
            baseline: 11,
            advances: vec![10u16; 95],
            tenths: None,
            cap: CapModel::Ratio { percent: 100 },
            x_scale: 1.0,
            width_pow: 1.0,
        };
        // advances 10 @15, drawn at cell 15 (eff 1): "ab cd" = 50,
        // "efgh" = 40, "ij kl" = 50, box 60.
        let t = TextField {
            auto_x: false,
            auto_y: false,
            x: 0,
            y: 0,
            origin: OriginMode::Block,
            rotation: Rotation::R0,
            direction: crate::ir::TextDirection::H,
            char_gap: 0,
            font: *b"00",
            font_height: 15,
            font_width: 0,
            reverse: false,
            block: Some(TextBlock {
                width: 60,
                max_lines: 3,
                line_spacing: 0,
                justification: Justification::Right,
                fb: true,
            }),
            data: "ab cd efgh ij kl".to_string(),
            font_name: None,
            symbol: false,
            re_eval: false,
        };
        let fl = layout(&t, &m, 203).expect("block lays out");
        assert_eq!(
            fl.starts,
            vec![0, 10, 10],
            "L1/L2 include the trailing space (50+10 and 40+10) into the 60 box; L3 is flush bare"
        );
    }

    /// A table where a middle slot is missing (0) and the space slot is
    /// nonzero: missing characters advance like a space.
    fn metrics_with_missing() -> FontMetrics {
        let mut advances = vec![0u16; 95];
        advances[0] = 12; // ' ' advance
        advances[b'X' as usize - 32] = 20;
        FontMetrics {
            cell_height: 15,
            baseline: 11,
            advances,
            tenths: None,
            cap: CapModel::Ratio { percent: 75 },
            x_scale: 1.0,
            width_pow: 1.0,
        }
    }

    #[test]
    fn advance_lookup_missing_char_uses_space() {
        let m = metrics_with_missing();
        assert_eq!(ref_advance(&m, ' '), 12, "space uses its own advance");
        assert_eq!(ref_advance(&m, 'X'), 20, "present glyph uses its advance");
        // 'Y' is absent from the table (0 entry) -> space advance.
        assert_eq!(ref_advance(&m, 'Y'), 12, "0 entry prints as a space");
        // Outside printable ASCII (e.g. codepage (C)) -> space advance too.
        assert_eq!(
            ref_advance(&m, '\u{00A9}'),
            12,
            "out-of-table char prints as space"
        );
        // A fully-zero table still cannot panic and collapses to 0.
        let empty = FontMetrics {
            cell_height: 15,
            baseline: 11,
            advances: vec![0; 95],
            tenths: None,
            cap: CapModel::Ratio { percent: 75 },
            x_scale: 1.0,
            width_pow: 1.0,
        };
        assert_eq!(ref_advance(&empty, 'X'), 0);
    }

    /// The measured font-0 calibration: baseline row offsets (rows below
    /// the cell top) for the sweep sizes, `floor(0.75 x cell) - 1` exactly.
    #[test]
    fn font0_cap_model_baselines_match_reference_table() {
        for &(cell, baseline) in &[
            (10, 6),
            (14, 9),
            (16, 11),
            (20, 14),
            (24, 17),
            (30, 21),
            (40, 29),
            (60, 44),
            (80, 59),
        ] {
            let m = FontMetrics {
                cell_height: 15,
                baseline: 11,
                advances: vec![0; 95],
                cap: CapModel::Ratio { percent: 75 },
                x_scale: 1.0,
                width_pow: 1.0,
                tenths: None,
            };
            assert_eq!(
                baseline_for_cell(&m.cap, cell),
                baseline,
                "font 0 baseline offset at cell {cell}"
            );
        }
        // Font F's bitmap snap: 21 rows per 30-dot band.
        let f = FontMetrics {
            cell_height: 26,
            baseline: 21,
            advances: vec![0; 95],
            cap: CapModel::Snap {
                base: 21,
                divisor: 30,
            },
            x_scale: 1.0,
            width_pow: 1.0,
            tenths: None,
        };
        for &(cell, baseline) in &[(10, 20), (30, 20), (31, 41), (40, 41), (80, 62)] {
            assert_eq!(
                baseline_for_cell(&f.cap, cell),
                baseline,
                "font F at {cell}"
            );
        }
    }

    #[test]
    fn fixed_width_overrides_proportional_advance() {
        let m = metrics_with_missing();
        let adv = Advancer {
            metrics: &m,
            eff: 1.0,
            fixed: 40,
            fb24: false,
            gap: 0,
        };
        assert_eq!(adv.dots('X'), 40);
        assert_eq!(adv.dots(' '), 40);
    }
}
