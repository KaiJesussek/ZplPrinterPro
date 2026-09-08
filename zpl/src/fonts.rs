//! Font provider, metric tables, and downloaded-font handling (Task 11).
//!
//! # Seam (documented decision)
//!
//! [`FontProvider::get`] serves **only built-in Zebra fonts** (`^A` letters
//! `A`-`H`, `P`-`V`, the `^GS` graphic-symbol font, and font `0`).
//! Downloaded fonts (`^A@`, [`FontSpec::Downloaded`]) are resolved by the
//! renderer, which holds [`RenderOptions::memory`]: it looks the name up in
//! `Memory.fonts` and builds the [`FontData`] with [`font_data_from_ttf`].
//! The committed trait surface (`fn get(&self, spec)`, Task 10) therefore
//! stays unchanged.
//!
//! # Downloaded fonts: the guide's cell model (transcribed)
//!
//! No reference oracle exists for downloads (the reference service does
//! not process them), so the downloaded-font model is guide-spec with the
//! current behavior kept where the guide is silent. The guide's own model:
//!
//! * `^A@` p.62 - `h` is "the height in dots of the entire character
//!   block", `w` "the width in dots of the entire character block"
//!   (scalable fonts; "Magnification factors are unnecessary, because
//!   characters are scaled"). A missing `h`/`w` uses the last accepted
//!   `^CF` value; `^A` p.60: "If the value is not given or a 0 (zero) is
//!   entered, the height or width is determined by the standard font
//!   matrix." For a downloaded font the standard matrix is its own bytes:
//!   the hhea vertical extent is the character block.
//! * The baseline is measured "from the top of the matrix" (Fonts and
//!   Barcodes p.1581-1582: "Baseline numbers define where the baseline is
//!   located in relationship to the top of the matrix"), so the baseline
//!   row down from the cell top is the downloaded font's own hhea ascent.
//! * Advances come from the font's own hmtx widths (downloaded fonts are
//!   proportional; `^A@` `w` fixes every advance to the character-block
//!   width when given).
//! * `~DY` p.181-183 supplies the bytes; its header fields `t` (byte
//!   count) and `w` (bytes per row - "value ignored" for `.TTF`/`.TTE`,
//!   p.183) are transmission framing, not metrics. The metric model lives
//!   in the downloaded font's own header tables (`head`/`hhea`/`hmtx`).
//!
//! [`CapModel::Ascent`] keeps the renderer on this model: the field's
//! requested `h` is the whole cell, the face's ascent lands on the
//! baseline (drawn via the ascender fit), and the ink is top-anchored
//! inside the cell (see `font_data_from_ttf` for the measured numbers).
//!
//! # Metric sourcing (the plan's authority rule)
//!
//! Cell heights come from the ZPL II Programming Guide's font tables
//! (Table 31, "8 dot/mm (203 dpi) Printhead" character matrices); the
//! per-font **cap model** - how the rendered cap height and baseline
//! derive from the requested cell - is calibrated to a cached parity
//! corpus of reference renders: font `0` and every bitmap family were
//! probed at their native cell and at 2x (corpus cases 17/18 and 20..29),
//! and each face's cap rows, baseline, advance, and drawn width are the
//! **measured** values from those renders (the guide's own per-face
//! baseline numbers are used as seeds, then replaced by the probe data -
//! the bitmap matrices are wider and shorter than the guide tables list).
//!
//! The reference renders the built-in fonts **bitmap-style**: the cap ink
//! starts at the top of the `^A` cell (some faces carry a top pad - the
//! OCR-B and P..V families ink a gap in from the cell top) and the
//! baseline sits one row below the cap's last ink row. Font `0`'s cap is
//! `floor(0.75 x cell)` rows (the guide's `3 x HEIGHT/4` rule, measured
//! exactly); the bitmap faces snap to stored sizes (`base x ceil(cell/divisor)`
//! rows - font F's `21 x ceil(cell/30)` band is measured across the whole
//! case-18 sweep; the other faces' native cell is their divisor). Baseline
//! offsets are therefore `pad + cap - 1`, not the guide's separate
//! baseline numbers.
//!
//! * Advances: the bundled face - the same metrically-condensed sans the
//!   reference bitmaps are cast from - is measured at the ppem where its
//!   cap height spans the drawn cap at the default cell, then multiplied
//!   by a per-face fit so the **reference** bitmap advance is reproduced
//!   (the reference matrices advance wider than the guide's `W + gap`
//!   numbers; the per-glyph residuals are the Task-L4b advance
//!   calibration). All 95 printable ASCII advances (index `char - 32`) are
//!   203 dpi reference values; the renderer scales by `dpi/203` (and by
//!   `h/cell` for an explicit `^A` height).
//! * Widths: each bitmap face additionally stretches its glyphs
//!   horizontally by a per-face [`FontMetrics::x_scale`] (the reference
//!   M is measured `1.1-1.7x` wider than the face's natural outline at the
//!   cap scale for the high-advance families; the P..V family runs at
//!   ~0.97), so the drawn ink width matches the probe without distorting
//!   the advance fit.
//! * The `^A` width parameter does not apply to the bitmap faces: the
//!   reference renders them at their native bitmap proportions whatever
//!   the width request (measured on the case-18 sweep and the probes).
//!   Font `0` is the one scalable font; its width behavior is calibrated
//!   separately (see [`builtin_metric`]).

use ab_glyph::{point, Font, FontVec};

/// A font request: a built-in Zebra font (`^A` letter) or a downloaded font
/// by name (`^A@`).
#[derive(Clone, Debug)]
pub enum FontSpec {
    /// A built-in font by `^A` letter: `b'A'..=b'H'`, `b'P'..=b'V'` (the
    /// bitmap matrices), `b'0'` (the scalable default font), or the
    /// `^GS` graphic-symbol font ([`SYMBOL_KEY`]). Unknown letters are
    /// unserved (caller warns).
    Zebra(u8),
    /// A downloaded font (`^A@d:f.x`), resolved from `Memory.fonts` by the
    /// renderer.
    Downloaded(String),
}

/// The internal key for the `^GS` graphic-symbol font (guide p.217): the
/// (C) (R) (TM) ... set cast from Noto Sans Symbols 2. `^GS` is its own command and
/// carries no `^A` letter, so the renderer routes symbol fields to this
/// key (no real ZPL font letter collides with `*`).
pub const SYMBOL_KEY: u8 = b'*';

/// A font the renderer can draw from: the TTF bytes plus metrics.
#[derive(Debug)]
pub struct FontData {
    /// The TrueType/OpenType font bytes.
    pub ttf: Vec<u8>,
    /// Metric data at the 203 dpi reference size.
    pub metrics: FontMetrics,
    /// A parsed `~DB` bitmap font (guide p.170) when the stored bytes are
    /// the structured `#code.h.w.x.y.i.data` text rather than a TTF.
    pub bitmap: Option<BitmapFont>,
}

/// A `~DB` downloaded bitmap font: the cell metrics plus per-character
/// glyph bitmaps parsed from the structured ASCII data.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapFont {
    /// Maximum cell height in dots (the `~DB` h parameter).
    pub cell_h: u32,
    /// Maximum cell width in dots (the `~DB` w parameter).
    pub cell_w: u32,
    /// Dots from the top of the cell to the character baseline.
    pub baseline: u32,
    /// Width (in dots) of a space or non-existent character.
    pub space: u32,
    /// Character definitions, keyed by character code (ASCII decimal for
    /// the standard 1-4 digit codes).
    pub glyphs: std::collections::BTreeMap<u32, BitmapGlyph>,
}

/// One character definition: bitmap rows plus cell-relative geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapGlyph {
    /// Bitmap height in dot rows.
    pub h: u32,
    /// Bitmap width in dots.
    pub w: u32,
    /// Cell-relative x offset (dots).
    pub x: i32,
    /// Cell-relative y offset (dots from the cell top).
    pub y: i32,
    /// Typesetting motion displacement: the advance in dots, inter-
    /// character spacing included.
    pub advance: u32,
    /// 1bpp rows, MSB-first, `w` bits per row.
    pub rows: Vec<u8>,
}

/// Parse a `~DB` download record (the raw parameter stream, guide p.170):
/// `d:o.x,a,h,w,base,space,#char,copyright,data` where `data` is the
/// structured `#xxxx.h.w.x.y.i.<row lines>` character text (rows of O/F
/// markers, one line per dot row, `F` = black). Returns `None` when the
/// record is not a well-formed bitmap font (the caller then treats the
/// stored object as an opaque download).
pub fn parse_bitmap_font(params: &[u8]) -> Option<BitmapFont> {
    // Split the first 8 commas: the header's 9 segments; the rest is the
    // character data.
    let commas: Vec<usize> = params
        .iter()
        .enumerate()
        .filter(|(_, &b)| b == b',')
        .take(8)
        .map(|(i, _)| i)
        .collect();
    if commas.len() < 7 {
        return None;
    }
    let seg = |idx: usize| -> Option<&[u8]> {
        let start = if idx == 0 { 0 } else { commas[idx - 1] + 1 };
        let end = commas.get(idx).copied().unwrap_or(params.len());
        params.get(start..end).map(str_trim)
    };
    let h = parse_u32(seg(2)?)?;
    let w = parse_u32(seg(3)?)?;
    let base = parse_u32(seg(4)?)?;
    let space = parse_u32(seg(5)?)?;
    let data = &params[commas[7] + 1..];
    let mut font = BitmapFont {
        cell_h: h,
        cell_w: w.max(1),
        baseline: base.min(h),
        space,
        glyphs: Default::default(),
    };
    // Character tokens: `#code.h.w.x.y.i.` followed by row lines until
    // the next `#` token.
    let bytes = data;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        // token head: code.h.w.x.y.i.
        let head = &bytes[i + 1..];
        let mut fields: Vec<&[u8]> = Vec::new();
        let mut cur = 0usize;
        for dot in head
            .iter()
            .enumerate()
            .take_while(|(_, &b)| b != b'\n' && b != b'#')
        {
            if *dot.1 == b'.' {
                fields.push(str_trim(&head[cur..dot.0]));
                cur = dot.0 + 1;
                if fields.len() == 6 {
                    break;
                }
            }
        }
        if fields.len() != 6 {
            i += 1;
            continue;
        }
        if fields.iter().any(|f| f.is_empty()) {
            i += 1;
            continue;
        }
        let code = parse_u32(fields[0]).unwrap_or(0);
        let gh = parse_u32(fields[1]).unwrap_or(h);
        let gw = parse_u32(fields[2]).unwrap_or(w);
        let gx = fields[3]
            .iter()
            .try_fold(0i32, |a, &b| {
                Some(a.saturating_mul(10).saturating_add((b - b'0') as i32))
            })
            .unwrap_or(0);
        let gy = fields[4]
            .iter()
            .try_fold(0i32, |a, &b| {
                Some(a.saturating_mul(10).saturating_add((b - b'0') as i32))
            })
            .unwrap_or(0);
        let advance = parse_u32(fields[5]).unwrap_or(space.max(1));
        // Advance past the token head (the 6th dot of code.h.w.x.y.i.).
        let head_end = {
            let mut p = i + 1;
            let mut dots = 0;
            while p < bytes.len() && dots < 6 {
                if bytes[p] == b'.' {
                    dots += 1;
                }
                p += 1;
            }
            p
        };
        let mut j = head_end;
        let mut rows: Vec<u8> = Vec::new();
        let mut row_count = 0u32;
        while j < bytes.len() && bytes[j] != b'#' && row_count < gh {
            // one row = one line
            let line_end =
                if let Some(rel) = bytes[j..].iter().position(|&b| b == b'\n' || b == b'\r') {
                    j + rel
                } else {
                    bytes.len()
                };
            let line = &bytes[j..line_end.min(j + gw as usize)];
            if !line.is_empty() {
                let mut byte = 0u8;
                let mut bits = 0u8;
                for &c in line {
                    if c == b'F' || c == b'f' {
                        byte |= 1 << (7 - bits);
                    }
                    bits += 1;
                    if bits == 8 {
                        rows.push(byte);
                        byte = 0;
                        bits = 0;
                    }
                }
                if bits > 0 {
                    rows.push(byte);
                }
                row_count += 1;
            }
            j = if line_end < bytes.len() {
                line_end + 1
            } else {
                bytes.len()
            };
            if j == bytes.len()
                && bytes[bytes.len() - 1] != b'\n'
                && bytes[bytes.len() - 1] != b'\r'
            {
                break;
            }
        }
        font.glyphs.insert(
            code,
            BitmapGlyph {
                h: row_count.max(1),
                w: gw.max(1),
                x: gx,
                y: gy,
                advance,
                rows,
            },
        );
        // advance i to the next '#' (or the end)
        let mut q = j;
        while q < bytes.len() && bytes[q] != b'#' {
            q += 1;
        }
        i = q;
    }
    if font.glyphs.is_empty() {
        return None;
    }
    Some(font)
}

fn str_trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|&c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|&c| !c.is_ascii_whitespace())
        .map_or(s, |p| p + 1);
    &b[s..e]
}

fn parse_u32(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(b.iter().fold(0u32, |a, &c| {
        a.saturating_mul(10).saturating_add((c - b'0') as u32)
    }))
}

/// Build a renderable `FontData` from a parsed bitmap font: the raw bytes
/// ride along in `ttf` (unused) and the metrics are synthesized from the
/// garbage-free `~DB` header (cell, baseline as `^CF`-style rows-from-top,
/// per-glyph advances).
pub fn font_data_from_bitmap(font: &BitmapFont, raw: &[u8]) -> FontData {
    let cell_height = font.cell_h.max(1);
    let mut advances = vec![font.space.max(1) as u16; 95];
    for (&code, g) in &font.glyphs {
        if (32..=126).contains(&code) {
            advances[(code - 32) as usize] = g.advance.clamp(1, u16::MAX as u32) as u16;
        }
    }
    let percent = ((font.baseline.max(1)) * 100 / cell_height).clamp(1, 100) as u16;
    FontData {
        ttf: raw.to_vec(),
        metrics: FontMetrics {
            cell_height,
            baseline: font.baseline.min(cell_height),
            advances,
            tenths: None,
            cap: CapModel::Ratio { percent },
            x_scale: 1.0,
            width_pow: 1.0,
        },
        bitmap: Some(font.clone()),
    }
}

/// How a built-in font's rendered cap height derives from the drawn cell
/// (the `^A` height). The reference renders built-ins bitmap-style: the
/// cap ink starts near the cell top and the baseline is the cap's bottom
/// row, so the baseline offset is `cap_rows(cell) - 1` (see
/// [`FontMetrics::baseline_for_cell`]). Downloadable TrueType fonts
/// (`CapModel::Ascent`) keep classic typefacing: the face's ascent lands
/// on the metric baseline.
#[derive(Clone, Debug)]
pub enum CapModel {
    /// Cap rows = `floor(percent x cell / 100)` - a native ratio family (font
    /// `0`: 75, the guide's `3 x HEIGHT/4`, measured exactly across the
    /// case-17 sweep).
    Ratio { percent: u16 },
    /// Cap rows = `base x ceil(cell / divisor)` - stored bitmap sizes (font
    /// `F`: base 21, divisor 30, measured across the case-18 sweep).
    Snap { base: u32, divisor: u32 },
    /// A reference bitmap-matrix face (guide Table 31, calibrated to the
    /// probe corpus): cap rows = `base x ceil(cell / divisor)` like
    /// [`CapModel::Snap`], plus a `pad`-row gap between the cell top and
    /// the ink (the OCR-B and P..V families ink a top gap of ~0.11-0.15
    /// of the cell; the other faces anchor at the top with `pad` 0). `lp`
    /// is the left pad (dots in from the cell edge - the G bitmap carries
    /// a ~0.1-cell left margin, the P..V family ~0.05). The per-face
    /// horizontal stretch and advance fit live in [`FontMetrics::x_scale`]
    /// and the advance table.
    Matrix {
        base: u32,
        divisor: u32,
        pad: u32,
        lp: u32,
    },
    /// TrueType: ascent-fitted baseline, no cap anchoring (downloaded
    /// fonts).
    Ascent,
}

/// Font metrics at the 203 dpi reference size; the renderer scales by
/// `dpi/203` (rounded). For `FontSpec::Downloaded` the reference size is
/// the fixed 100 ppem of [`font_data_from_ttf`]; the renderer additionally
/// scales from `cell_height` to the field's requested height.
#[derive(Clone, Debug)]
pub struct FontMetrics {
    /// Character-cell height in dots at 203 dpi.
    pub cell_height: u32,
    /// Baseline position in dots down from the top of the cell (guide
    /// value; the drawn baseline for a field comes from the cap model,
    /// [`FontMetrics::baseline_for_cell`]).
    pub baseline: u32,
    /// Horizontal advance per character, `advances[char - 32]` for chars
    /// 32..=126 (0 = space, 94 = `~`); a 0 entry means the glyph is
    /// missing and prints as a space.
    pub advances: Vec<u16>,
    /// Font-0 per-glyph advance override: `(char, tenths)` pairs where the
    /// drawn advance is `round(tenths * cell / 1000)` (the L4c corpus
    /// measurements of the reference bitmap font 0; absent chars fall back
    /// to `advances`). `None` for every other face.
    pub tenths: Option<&'static [(char, u16)]>,
    /// Cap-anchoring model (bitmap-style vs TrueType).
    pub cap: CapModel,
    /// Horizontal glyph scale relative to the vertical in dot-for-dot
    /// terms at the reference size: the drawn ink width equals the face's
    /// natural outline width at the cap scale times this factor. The
    /// reference bitmap families print wider than their source faces
    /// (the probe M widths divided by the face's natural M at the cap
    /// scale); 1.0 = the face's own proportions (font 0, downloaded
    /// fonts, `^GS`).
    pub x_scale: f32,
    /// Advance/ink width growth power relative to the face's native cell:
    /// `width propto cell ^ width_pow`. 1.0 = linear. The reference matrix
    /// faces G and V grow sub-linearly (~1.5x per doubling, measured from
    /// the 2x probe hops); identity keeps every other face linear.
    pub width_pow: f32,
}

/// Cap ink rows (from the cell top, the baseline row included) for a
/// drawn cell of `cell` dots. `Ratio` multiplies; `Snap`/`Matrix` snap to
/// stored bitmap bands (`base` rows per `divisor`-dot band; `Matrix` adds
/// the face's top pad); `Ascent` never reaches this - see [`CapModel`].
fn cap_rows_for(cap: &CapModel, cell: u32) -> u32 {
    match *cap {
        CapModel::Ratio { percent } => cell.saturating_mul(u32::from(percent)) / 100,
        CapModel::Snap { base, divisor } => {
            let d = divisor.max(1);
            base.saturating_mul(cell.saturating_add(d - 1) / d)
        }
        CapModel::Matrix {
            base,
            divisor,
            pad,
            lp: _,
        } => {
            let d = divisor.max(1);
            pad.saturating_add(base.saturating_mul(cell.saturating_add(d - 1) / d))
        }
        CapModel::Ascent => cell,
    }
}

/// Baseline row offset (rows down from the cell top to the baseline row,
/// the cap's bottom row) for a drawn cell of `cell` dots under `cap`.
/// Bitmap-snap fonts may ink past the cell (their cap exceeds the `^A`
/// height), so there is no upper clamp - the renderer's draw band covers
/// it.
/// Public for the integration tests that mirror [`GUIDE_CELLS`].
pub fn baseline_for_cell(cap: &CapModel, cell: u32) -> u32 {
    cap_rows_for(cap, cell).saturating_sub(1).max(1)
}

/// The bitmap cell's left pad: dots from the cell's left edge to the
/// glyph ink. Font 0 inks `ceil(h/16)` in from the cell edge (measured 1..5
/// dots across the parity sweep); the bitmap families (`Snap`, `Matrix`,
/// F, ...) ink from the edge itself.
/// The matrix faces' top gap: rows from the cell top to the cap ink (0
/// for every other family). The pad is a *position* offset - the paint
/// scale fits the cap alone, and the glyph is anchored `pad` rows lower -
/// so it must not feed the baseline (which counts rows to the cap's
/// bottom row) through the scale.
pub(crate) fn matrix_pad(cap: &CapModel, cell: u32) -> u32 {
    match *cap {
        CapModel::Matrix {
            base: _,
            divisor,
            pad,
            lp: _,
        } => {
            let d = divisor.max(1);
            pad.saturating_mul(cell.saturating_add(d - 1) / d)
        }
        _ => 0,
    }
}

pub(crate) fn left_pad(cap: &CapModel, cell: u32) -> u32 {
    match *cap {
        CapModel::Ratio { .. } => (cell + 8) / 16,
        // The matrix bitmaps ink a fixed pad in from the cell edge (the
        // face's own left margin); like the cap it snaps per band.
        CapModel::Matrix {
            base: _,
            divisor,
            pad: _,
            lp,
        } => {
            let d = divisor.max(1);
            lp.saturating_mul(cell.saturating_add(d - 1) / d)
        }
        _ => 0,
    }
}

/// Supplies built-in fonts to the renderer.
pub trait FontProvider {
    /// Look up the built-in font named by `spec`; `None` when it is
    /// unavailable (unknown Zebra letter, unmapped face - the caller warns
    /// and skips). `Downloaded` names are never served here: the renderer
    /// resolves them from printer memory (see the module docs).
    fn get(&self, spec: &FontSpec) -> Option<FontData>;
}

/// The bundled faces, embedded at compile time, one per built-in font
/// letter. Each is open-licensed and its license text is committed beside
/// it (the `LICENSE-*` files in the fixtures directory):
/// * `0` - Noto Sans Condensed Bold (SIL OFL 1.1): the reference's font-0
///   strokes measure ~1.6-1.8x DejaVu Sans Condensed at the same cap, so
///   the closest bundled weight wins on ink (the R1 round; advances stay
///   the measured `FONT0_TENTHS`, so only the ink changes).
/// * `A`-`D`, `F`, `G`, `P`-`V` - Noto Sans Condensed (SIL OFL 1.1), the
///   condensed sans the reference's bitmap families track.
/// * `E` - OCR-B (jaycee723/ocr-b, SIL OFL 1.1), the OCR letter face.
/// * `H` - OCR-A (WP OCR-A derivative of John Sauter's OCR-A, SIL OFL
///   1.1; the hhea vertical metrics were repaired to the font's own
///   OS/2 values - the published build ships ascent 0), the OCR-A face.
/// * `*` - Noto Sans Symbols 2 (SIL OFL 1.1), the `^GS` (C) (R) (TM) set.
const BUNDLED_FACES: &[(u8, &str, &[u8])] = &[
    (
        b'0',
        "Noto Sans Condensed Bold",
        include_bytes!("../tests/fixtures/NotoSans-CondensedBold.ttf"),
    ),
    (
        b'A',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'B',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'C',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'D',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (b'E', "OCR-B", include_bytes!("../tests/fixtures/OCR-B.ttf")),
    (
        b'F',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'G',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'H',
        "OCR-A",
        include_bytes!("../tests/fixtures/OCR-A-Regular.ttf"),
    ),
    (
        SYMBOL_KEY,
        "Noto Sans Symbols 2",
        include_bytes!("../tests/fixtures/NotoSansSymbols2-Regular.ttf"),
    ),
    (
        b'P',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'Q',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'R',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'S',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'T',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'U',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
    (
        b'V',
        "Noto Sans Condensed",
        include_bytes!("../tests/fixtures/NotoSans-Condensed.ttf"),
    ),
];

/// Calibrated (letter, cell, cap model, advance fit, x-scale) entries.
///
/// The cell is the guide's Table 31 native matrix height at 203 dpi; the
/// cap model is the parity-calibrated bitmap rule (see [`CapModel`]).
/// `adv_fit` scales the face's measured advances so the reference
/// bitmap's advance at the default cell is reproduced (the reference
/// matrices advance wider than the guide's `W + gap`); `x_scale`
/// stretches the drawn ink horizontally to the reference bitmap's width.
/// Both were fitted to the probe corpus (cases 20-29) at the native cell
/// and at 2x, reported in the L4b task summary.
///
/// Font `0` (scalable default font) is the guide's `Default: 15 x 12`
/// matrix with baseline `3 x HEIGHT/4` (`3*15/4 = 11.25` rounded to 11),
/// measured as an exact `0.75` cap ratio; its advance fit (11/12) and
/// left pad (h/16) were measured across the case-17 sweep. Font `F` is a
/// bitmap-snap family (base 21 rows per 30-dot band, measured across the
/// case-18 sweep). The `P`-`V` entries between the two probed faces
/// (`P`, `V`) interpolate the measured cap/pad ratios. The integration
/// test `zpl/tests/fonts.rs` duplicates these values; a change to either
/// side is a review event.
const GUIDE_CELLS: &[(u8, u32, CapModel, f32, f32, f32)] = &[
    (
        b'A',
        9,
        CapModel::Matrix {
            base: 7,
            divisor: 12,
            pad: 0,
            lp: 0,
        },
        1.468,
        1.927,
        1.0,
    ),
    (
        b'B',
        11,
        CapModel::Matrix {
            base: 11,
            divisor: 11,
            pad: 0,
            lp: 0,
        },
        1.219,
        1.850,
        1.0,
    ),
    (
        b'C',
        18,
        CapModel::Matrix {
            base: 14,
            divisor: 18,
            pad: 0,
            lp: 0,
        },
        1.303,
        1.779,
        1.0,
    ),
    (
        b'D',
        18,
        CapModel::Matrix {
            base: 14,
            divisor: 18,
            pad: 0,
            lp: 0,
        },
        1.645,
        1.656,
        1.0,
    ),
    (
        b'E',
        28,
        CapModel::Matrix {
            base: 20,
            divisor: 28,
            pad: 3,
            lp: 0,
        },
        1.347,
        2.074,
        1.0,
    ),
    (
        b'F',
        26,
        CapModel::Snap {
            base: 21,
            divisor: 30,
        },
        1.084,
        1.677,
        1.0,
    ),
    (
        b'G',
        60,
        CapModel::Matrix {
            base: 48,
            divisor: 60,
            pad: 0,
            lp: 6,
        },
        1.441,
        1.673,
        0.585,
    ),
    (
        b'H',
        21,
        CapModel::Matrix {
            base: 21,
            divisor: 21,
            pad: 0,
            lp: 0,
        },
        1.578,
        2.116,
        1.0,
    ),
    (
        SYMBOL_KEY,
        24,
        CapModel::Matrix {
            base: 15,
            divisor: 24,
            pad: 0,
            lp: 0,
        },
        1.0,
        1.0,
        1.0,
    ),
    (
        b'P',
        20,
        CapModel::Matrix {
            base: 12,
            divisor: 20,
            pad: 3,
            lp: 1,
        },
        0.739,
        1.051,
        1.0,
    ),
    (
        b'Q',
        28,
        CapModel::Matrix {
            base: 17,
            divisor: 28,
            pad: 4,
            lp: 2,
        },
        0.715,
        1.021,
        1.0,
    ),
    (
        b'R',
        35,
        CapModel::Matrix {
            base: 21,
            divisor: 35,
            pad: 5,
            lp: 2,
        },
        0.715,
        1.021,
        1.0,
    ),
    (
        b'S',
        40,
        CapModel::Matrix {
            base: 25,
            divisor: 40,
            pad: 6,
            lp: 3,
        },
        0.715,
        1.021,
        1.0,
    ),
    (
        b'T',
        48,
        CapModel::Matrix {
            base: 30,
            divisor: 48,
            pad: 7,
            lp: 3,
        },
        0.715,
        1.021,
        1.0,
    ),
    (
        b'U',
        59,
        CapModel::Matrix {
            base: 37,
            divisor: 59,
            pad: 9,
            lp: 4,
        },
        0.715,
        1.021,
        1.0,
    ),
    (
        b'V',
        80,
        CapModel::Matrix {
            base: 50,
            divisor: 80,
            pad: 12,
            lp: 4,
        },
        0.691,
        0.991,
        1.0,
    ),
    (b'0', 15, CapModel::Ratio { percent: 75 }, 0.917, 1.0, 1.0),
];

/// Measure the printable-ASCII advance table for `face` at `ppem` (pixels
/// per em), our 203 dpi reference size. `advances[i]` is the advance of
/// char `i + 32` in hundredths-of-a-dot-per-em units converted to dots,
/// rounded to whole dots.
fn measure_advances(face: &FontVec, ppem: f32) -> Vec<u16> {
    let upem = face.units_per_em().unwrap_or(2048.0);
    (32..=126)
        .map(|c| {
            let id = face.glyph_id(char::from_u32(c).expect("32..=126 is valid char"));
            let dots = face.h_advance_unscaled(id) * ppem / upem;
            dots.round().clamp(0.0, u16::MAX as f32) as u16
        })
        .collect()
}

/// The face's cap height in font units: the flat top of `H` (the glyph
/// whose top edge is the cap height). The reference anchors bitmap-style
/// text so the cap starts at the cell top; the paint scale fits this
/// metric. Falls back to the ascent when the outline is unavailable.
/// Shared with the barcode interpret-line painting.
pub(crate) fn cap_units(face: &FontVec, upem: f32) -> f32 {
    let gid = face.glyph_id('H');
    face.outline_glyph(gid.with_scale_and_position(upem, point(0.0, 0.0)))
        .map(|o| -o.px_bounds().min.y)
        .filter(|c| c.is_finite() && *c > 0.0)
        .unwrap_or_else(|| face.ascent_unscaled())
}

/// Measured per-glyph advances of the reference bitmap font 0 (L4c): each
/// `(char, tenths)` entry gives the advance at a 100-dot cell in tenths of
/// a dot - drawn advance = `round(tenths * cell / 1000)`. These are the
/// corpus measurements (cases 01-07/15-17 at the listed heights); chars
/// absent from the table keep the face-derived `advances` fallback. The
/// reference matrix advances are NOT the bundled face's proportions
/// (H ~ 0.58 em vs the face's 0.67 at cell 15), and the space is ~0.30 em.
pub const FONT0_TENTHS: &[(char, u16)] = &[
    (' ', 324),
    ('-', 780),
    ('0', 500),
    ('1', 433),
    ('2', 500),
    ('3', 474),
    ('8', 500),
    (':', 250),
    ('B', 500),
    ('C', 621),
    ('F', 540),
    ('G', 580),
    ('H', 580),
    ('K', 518),
    ('M', 725),
    ('N', 600),
    ('P', 525),
    ('R', 575),
    ('S', 554),
    ('T', 540),
    ('U', 540),
    ('W', 780),
    ('Z', 500),
    ('a', 474),
    ('b', 500),
    ('c', 480),
    ('d', 500),
    ('e', 505),
    ('f', 270),
    ('g', 521),
    ('h', 472),
    ('i', 267),
    ('j', 270),
    ('k', 437),
    ('l', 241),
    ('m', 580),
    ('n', 460),
    ('o', 480),
    ('p', 479),
    ('r', 312),
    ('s', 437),
    ('t', 354),
    ('u', 500),
    ('v', 567),
    ('w', 688),
    ('x', 479),
    ('y', 367),
    (',', 283),
    ('.', 283),
    ('I', 283),
    ('(', 300),
    (')', 300),
    ('\u{00A3}', 459),
    ('\u{00C2}', 450),
    ('\u{00E4}', 360),
    ('\u{00E9}', 500),
    ('\u{00F1}', 500),
    ('\u{00F6}', 500),
    ('\u{00FB}', 480),
    ('\u{0192}', 550),
    ('\u{251C}', 339),
    ('\u{255D}', 299),
    ('\u{20AC}', 550),
];

/// Per-letter metrics for the bundled face: the cap model is calibrated (a
/// bitmap snap/matrix rule or font 0's 75% ratio); the ppem is fitted so
/// the face's cap height spans the drawn cap at the default cell
/// (`cap_rows(cell) - 1` rows), and advances are measured at that ppem
/// then scaled by the per-face `adv_fit`. `baseline` keeps the guide
/// value (the default-cell cap rows).
#[allow(clippy::too_many_arguments)]
fn builtin_metric(
    face: &FontVec,
    letter: u8,
    cell_height: u32,
    baseline: u32,
    cap: &CapModel,
    adv_fit: f32,
    x_scale: f32,
    width_pow: f32,
) -> FontMetrics {
    let upem = face.units_per_em().unwrap_or(2048.0);
    // Baseline rows below the cell top at the default cell; the ppem
    // solves `cap_height_units * ppem / upem == baseline - 1`. The ppem
    // fits the face's cap alone - the matrix faces' top pad shifts the
    // glyph down but does not scale it.
    let cap_rows = cap_rows_for(cap, cell_height);
    let b = cap_rows
        .saturating_sub(matrix_pad(cap, cell_height))
        .saturating_sub(1)
        .max(1) as f32;
    let ppem = (b * upem / cap_units(face, upem)).max(1.0);
    FontMetrics {
        cell_height,
        baseline,
        cap: cap.clone(),
        advances: measure_advances(face, (ppem * adv_fit).max(1.0)),
        tenths: (letter == b'0').then_some(FONT0_TENTHS),
        x_scale,
        width_pow,
    }
}

/// Supplies the bundled face for every built-in Zebra font letter.
///
/// The per-letter metric tables are precomputed at construction; `get`
/// clones the bundled bytes fresh per lookup (`ttf` is owned per the
/// [`FontData`] contract - the renderer may cache). The parsed face is
/// dropped after construction, so the provider itself stays small.
pub struct DefaultFontProvider {
    metrics: Vec<FontMetrics>,
}

impl DefaultFontProvider {
    /// Parse the bundled faces and precompute the per-letter metric tables.
    /// Panics only if the committed fixture stops parsing (a build-time
    /// programming error, not a runtime condition).
    pub fn new() -> Self {
        let metrics = GUIDE_CELLS
            .iter()
            .map(|&(letter, cell, ref cap, adv_fit, x_scale, width_pow)| {
                let bytes = BUNDLED_FACES
                    .iter()
                    .find(|&&(c, _, _)| c == letter)
                    .map(|&(_, _, b)| b)
                    .expect("every guide letter has a bundled face");
                let face =
                    FontVec::try_from_vec(bytes.to_vec()).expect("bundled font failed to parse");
                let baseline = cap_rows_for(cap, cell).saturating_sub(1).max(1);
                builtin_metric(
                    &face, letter, cell, baseline, cap, adv_fit, x_scale, width_pow,
                )
            })
            .collect();
        DefaultFontProvider { metrics }
    }
}

impl FontProvider for DefaultFontProvider {
    fn get(&self, spec: &FontSpec) -> Option<FontData> {
        let letter = match spec {
            FontSpec::Zebra(l) => *l,
            // Downloaded fonts live in printer memory; the renderer
            // resolves them (module docs). Not our job, never ours.
            FontSpec::Downloaded(_) => return None,
        };
        let idx = GUIDE_CELLS
            .iter()
            .position(|&(c, _, _, _, _, _)| c == letter)?;
        let bytes = BUNDLED_FACES
            .iter()
            .find(|&&(c, _, _)| c == letter)
            .map(|&(_, _, b)| b)
            .expect("every guide letter has a bundled face");
        Some(FontData {
            ttf: bytes.to_vec(),
            metrics: self.metrics[idx].clone(),
            bitmap: None,
        })
    }
}

impl Default for DefaultFontProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the [`FontData`] for a downloaded font (`^A@`) stored in
/// `Memory.fonts`, on the guide's scalable-font cell model (module docs).
///
/// The cell is the downloaded font's own: measured once at a fixed
/// reference size of 100 ppem - like the built-in tables these are 203 dpi
/// reference values the renderer rescales (`dpi/203`, and from
/// `cell_height` to the field's requested height). Any fixed reference
/// ppem would do (the renderer rescales); 100 keeps whole-dot rounding
/// from collapsing the small advances.
///
/// * `cell_height` - the entire character block (`^A@` p.62): the hhea
///   vertical extent `ascent - descent` at the reference size, so an
///   `^A@` `h` of `H` dots draws the face with its em box spanning `H`
///   (the renderer's `eff = h / cell_height`).
/// * `baseline` - the baseline row down from the top of the cell
///   (p.1581-1582): the font's own hhea ascent at the reference size,
///   clamped into the cell (a corrupt hhea where the ascent exceeds the
///   vertical extent must not push the baseline past the cell).
/// * `advances` - the font's own hmtx widths at the reference size; the
///   renderer scales them by the drawn cell (a missing glyph prints as a
///   space, per the module's documented rule).
/// * `cap` - [`CapModel::Ascent`], so the renderer draws the glyphs
///   top-anchored with the ascender fit instead of the bitmap cap rule.
///
/// `None` when the bytes are not a usable TrueType font (unparseable, or
/// zero units-per-em) - the caller warns and skips the field. A parseable
/// font with degenerate hhea vertical metrics (e.g. ascent 0) yields
/// `Some` with a 1-dot cell; the renderer's paint guard warns and skips
/// the field, so a corrupt download prints nothing, never garbage.
pub fn font_data_from_ttf(ttf: &[u8]) -> Option<FontData> {
    let face = FontVec::try_from_vec(ttf.to_vec()).ok()?;
    let ppem = 100.0;
    let upem = face.units_per_em().unwrap_or(2048.0);
    if upem <= 0.0 {
        return None;
    }
    let (ascent, descent) = (face.ascent_unscaled(), face.descent_unscaled());
    let cell_height = ((ascent - descent) * ppem / upem).round().max(1.0) as u32;
    let baseline = ((ascent * ppem / upem).round().max(1.0) as u32).min(cell_height);
    Some(FontData {
        ttf: ttf.to_vec(),
        bitmap: None,
        metrics: FontMetrics {
            cell_height,
            baseline,
            advances: measure_advances(&face, ppem),
            tenths: None,
            cap: CapModel::Ascent,
            x_scale: 1.0,
            width_pow: 1.0,
        },
    })
}
