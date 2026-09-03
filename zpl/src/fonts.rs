//! Font provider, metric tables, and downloaded-font handling (Task 11).
//!
//! # Seam (documented decision)
//!
//! [`FontProvider::get`] serves **only built-in Zebra fonts** (`^A` letters
//! `A`-`H` and font `0`). Downloaded fonts (`^A@`, [`FontSpec::Downloaded`])
//! are resolved by the renderer, which holds [`RenderOptions::memory`]: it
//! looks the name up in `Memory.fonts` and builds the [`FontData`] with
//! [`font_data_from_ttf`]. The committed trait surface (`fn get(&self, spec)`,
//! Task 10) therefore stays unchanged.
//!
//! # Metric sourcing (the plan's authority rule)
//!
//! Cell heights come from the ZPL II Programming Guide's font tables
//! (Table 31, "8 dot/mm (203 dpi) Printhead" character matrices); the
//! per-font **cap model** - how the rendered cap height and baseline
//! derive from the requested cell - is calibrated to a cached parity
//! corpus of reference renders (fonts `0` and `F` measured at every size
//! in the font sweeps). The bundled face is a metrically-condensed sans
//! stand-in for the real bitmapped fonts; all letters map to this one
//! face for now (per-glyph bitmap parity is a later refinement).
//!
//! The reference renders the built-in fonts **bitmap-style**: the cap
//! ink starts at the top of the `^A` cell and the baseline sits one row
//! below the cap's last ink row. Font `0`'s cap is `floor(0.75 x cell)`
//! rows (the guide's `3 x HEIGHT/4` rule, measured exactly); font `F`
//! snaps to stored bitmap sizes, `21 x ceil(cell/30)` rows. Baseline offsets
//! are therefore `cap - 1`, not the guide's separate baseline numbers -
//! those stay in the table below as the default-cell cap rows.
//!
//! * Advances: our bundled face measured at the ppem where the face's cap
//!   height spans the baseline at the default cell. All 95 printable
//!   ASCII advances (index `char - 32`) are 203 dpi reference values;
//!   the renderer scales by `dpi/203` (and by `h/cell` for an explicit
//!   `^A` height).

use ab_glyph::{point, Font, FontVec};

/// A font request: a built-in Zebra font (`^A` letter) or a downloaded font
/// by name (`^A@`).
#[derive(Clone, Debug)]
pub enum FontSpec {
    /// A built-in font by `^A` letter: `b'A'..=b'H'` or `b'0'` (the
    /// scalable default font). Unknown letters are unserved (caller warns).
    Zebra(u8),
    /// A downloaded font (`^A@d:f.x`), resolved from `Memory.fonts` by the
    /// renderer.
    Downloaded(String),
}

/// A font the renderer can draw from: the TTF bytes plus metrics.
#[derive(Debug)]
pub struct FontData {
    /// The TrueType/OpenType font bytes.
    pub ttf: Vec<u8>,
    /// Metric data at the 203 dpi reference size.
    pub metrics: FontMetrics,
}

/// How a built-in font's rendered cap height derives from the drawn cell
/// (the `^A` height). The reference renders built-ins bitmap-style: the
/// cap ink starts at the cell top and the baseline is the cap's bottom
/// row, so the baseline offset is `cap_rows(cell) - 1` (see
/// [`FontMetrics::baseline_for_cell`]). Downloadable TrueType fonts
/// (`CapModel::Ascent`) keep classic typefacing: the face's ascent lands
/// on the metric baseline.
#[derive(Clone, Debug)]
pub enum CapModel {
    /// Cap rows = `floor(percent x cell / 100)` - a native ratio family (font
    /// `0`: 75, the guide's `3 x HEIGHT/4`).
    Ratio { percent: u16 },
    /// Cap rows = `base x ceil(cell / divisor)` - stored bitmap sizes (font
    /// `F`: base 21, divisor 30, measured from the parity sweep).
    Snap { base: u32, divisor: u32 },
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
    /// Cap-anchoring model (bitmap-style vs TrueType).
    pub cap: CapModel,
}

/// Cap ink rows (from the cell top, the baseline row included) for a
/// drawn cell of `cell` dots: `floor(percent x cell / 100)` or
/// `base x ceil(cell / divisor)` (the two calibrated families; `Ascent`
/// never reaches this - see [`CapModel`]).
fn cap_rows_for(cap: &CapModel, cell: u32) -> u32 {
    match *cap {
        CapModel::Ratio { percent } => cell.saturating_mul(u32::from(percent)) / 100,
        CapModel::Snap { base, divisor } => {
            let d = divisor.max(1);
            base.saturating_mul(cell.saturating_add(d - 1) / d)
        }
        CapModel::Ascent => cell,
    }
}

/// Baseline row offset (rows down from the cell top to the baseline row,
/// the cap's bottom row) for a drawn cell of `cell` dots under `cap`.
/// Bitmap-snap fonts may ink past the cell (their cap exceeds the `^A`
/// height), so there is no upper clamp - the renderer's draw band covers
/// it.
pub(crate) fn baseline_for_cell(cap: &CapModel, cell: u32) -> u32 {
    cap_rows_for(cap, cell).saturating_sub(1).max(1)
}

/// The bitmap cell's left pad: dots from the cell's left edge to the
/// glyph ink. Font 0 inks `ceil(h/16)` in from the cell edge (measured 1..5
/// dots across the parity sweep); the snap families (F, A) ink from the
/// edge itself.
pub(crate) fn left_pad(cap: &CapModel, cell: u32) -> u32 {
    match *cap {
        CapModel::Ratio { .. } => (cell + 8) / 16,
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

/// The bundled face, embedded at compile time: DejaVu fonts 2.37
/// `DejaVuSansCondensed.ttf` (Bitstream Vera-licensed; the license text is
/// committed beside the font at `zpl/tests/fixtures/LICENSE-DejaVu.txt`).
const BUNDLED_TTF: &[u8] = include_bytes!("../tests/fixtures/DejaVuSansCondensed.ttf");

/// Guide-anchored (letter, cell height, baseline, cap model) entries.
///
/// Cell heights: ZPL II Programming Guide Table 31, "8 dot/mm (203 dpi)
/// Printhead" character matrices. Baselines: Table 29, "Intercharacter
/// Gap and Baseline Parameters" - the guide's baseline of the default
/// matrix, which the parity measurements show is the default-cell cap in
/// rows. Font `0` (scalable default font) is the guide's `Default: 15 x
/// 12` matrix with baseline `3 x HEIGHT/4` (`3*15/4 = 11.25` rounded to
/// 11), measured as an exact `0.75` cap ratio. Font `F` is a bitmap-snap
/// family (base 21 rows per 30-dot band, measured from the parity sweep).
/// The `A-H` entries preserve their guide baseline as the default-cell
/// cap ratio. The integration test `zpl/tests/fonts.rs` duplicates these
/// values; a change to either side is a review event.
const GUIDE_CELLS: &[(u8, u32, u32, CapModel)] = &[
    (b'A', 9, 7, CapModel::Snap { base: 15, divisor: 9 }),
    (b'B', 11, 11, CapModel::Ratio { percent: 100 }),
    (b'C', 18, 14, CapModel::Ratio { percent: 78 }),
    (b'D', 18, 14, CapModel::Ratio { percent: 78 }),
    (b'E', 28, 23, CapModel::Ratio { percent: 82 }),
    (b'F', 26, 21, CapModel::Snap { base: 21, divisor: 30 }),
    (b'G', 60, 48, CapModel::Ratio { percent: 80 }),
    (b'H', 21, 21, CapModel::Ratio { percent: 100 }),
    (b'0', 15, 11, CapModel::Ratio { percent: 75 }),
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

/// Per-letter metrics for the bundled face: each letter's cell/baseline
/// come from the guide and the cap model is calibrated; the ppem is
/// fitted so the face's cap height spans the baseline at the default
/// cell (`cap_rows(cell) - 1` rows), and advances are measured at that
/// ppem. `baseline` keeps the guide value (the default-cell cap rows).
fn builtin_metric(
    face: &FontVec,
    letter: u8,
    cell_height: u32,
    baseline: u32,
    cap: &CapModel,
) -> FontMetrics {
    let upem = face.units_per_em().unwrap_or(2048.0);
    // Baseline rows below the cell top at the default cell; the ppem
    // solves `cap_height_units * ppem / upem == baseline - 1`.
    let b = cap_rows_for(cap, cell_height).saturating_sub(1).max(1) as f32;
    let ppem = (b * upem / cap_units(face, upem)).max(1.0);
    // Font 0's reference advances run ~0.917x the face's at the cap
    // scale (the bitmap's M advances 11 dots at the default cell where
    // the face measures 12): fit the advance table to the reference. The
    // other letters have no measured advance data yet - 1.0.
    let adv_fit = if letter == b'0' { 11.0 / 12.0 } else { 1.0 };
    FontMetrics {
        cell_height,
        baseline,
        cap: cap.clone(),
        advances: measure_advances(face, (ppem * adv_fit).max(1.0)),
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
    /// Parse the bundled face and precompute the per-letter metric tables.
    /// Panics only if the committed fixture stops parsing (a build-time
    /// programming error, not a runtime condition).
    pub fn new() -> Self {
        let face = FontVec::try_from_vec(BUNDLED_TTF.to_vec())
            .expect("bundled font (DejaVuSansCondensed.ttf) failed to parse");
        let metrics = GUIDE_CELLS
            .iter()
            .map(|&(letter, cell, baseline, ref cap)| {
                builtin_metric(&face, letter, cell, baseline, cap)
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
        let idx = GUIDE_CELLS.iter().position(|&(c, _, _, _)| c == letter)?;
        Some(FontData {
            ttf: BUNDLED_TTF.to_vec(),
            metrics: self.metrics[idx].clone(),
        })
    }
}

impl Default for DefaultFontProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the [`FontData`] for a downloaded font (`^A@`) stored in
/// `Memory.fonts`. The bytes are parsed once and measured at a fixed
/// reference size of 100 ppem; like the built-in tables these are 203 dpi
/// reference values the renderer rescales (`dpi/203`, and from
/// `cell_height` to the field's requested height). `None` when the bytes
/// are not a usable TrueType font - the caller warns and skips the field.
///
/// Any fixed reference ppem would do (the renderer rescales); 100 keeps
/// whole-dot rounding from collapsing the small advances.
pub fn font_data_from_ttf(ttf: &[u8]) -> Option<FontData> {
    let face = FontVec::try_from_vec(ttf.to_vec()).ok()?;
    let ppem = 100.0;
    let upem = face.units_per_em().unwrap_or(2048.0);
    let (ascent, descent) = (face.ascent_unscaled(), face.descent_unscaled());
    let cell_height = ((ascent - descent) * ppem / upem).round().max(1.0) as u32;
    let baseline = (ascent * ppem / upem).round().max(1.0) as u32;
    Some(FontData {
        ttf: ttf.to_vec(),
        metrics: FontMetrics {
            cell_height,
            baseline,
            advances: measure_advances(&face, ppem),
            cap: CapModel::Ascent,
        },
    })
}
