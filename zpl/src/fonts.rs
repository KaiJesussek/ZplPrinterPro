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
//! Cell heights and baselines come **only** from the ZPL II Programming
//! Guide's font tables; advance widths are **self-consistent measurements**
//! of our own bundled free font - nothing reference-derived is committed.
//!
//! * Cell height `cell_height`: Guide Table 31 "8 dot/mm (203 dpi)
//!   Printhead" character matrices (Fonts and Barcodes chapter).
//! * Baseline: Guide Table 29 "Intercharacter Gap and Baseline Parameters"
//!   (baseline in dots down from the top of the matrix). Font `0`'s entry
//!   is the guide's `3 x HEIGHT/4` for its default 15 by 12 matrix:
//!   `3*15/4 = 11.25`, rounded to 11.
//! * Advances: our bundled face anchored to the guide's baseline - the
//!   face is measured at the ppem where its hhea ascent equals the
//!   published baseline, and `h_advance` is taken at that ppem. All 95
//!   printable ASCII advances (index `char - 32`) are 203 dpi reference
//!   values; the renderer scales by `dpi/203`.
//!
//! The bundled face is a metrically-condensed sans stand-in for the real
//! Zebra bitmapped fonts; all letters map to this one face for now
//! (per-font metric parity is the external-reference refinement procedure,
//! later).

use ab_glyph::{Font, FontVec};

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

/// Font metrics at the 203 dpi reference size; the renderer scales by
/// `dpi/203` (rounded). For `FontSpec::Downloaded` the reference size is
/// the fixed 100 ppem of [`font_data_from_ttf`]; the renderer additionally
/// scales from `cell_height` to the field's requested height.
#[derive(Clone, Debug)]
pub struct FontMetrics {
    /// Character-cell height in dots at 203 dpi.
    pub cell_height: u32,
    /// Baseline position in dots down from the top of the cell.
    pub baseline: u32,
    /// Horizontal advance per character, `advances[char - 32]` for chars
    /// 32..=126 (0 = space, 94 = `~`); a 0 entry means the glyph is
    /// missing and prints as a space.
    pub advances: Vec<u16>,
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

/// Guide-anchored (letter, cell height, baseline) triplets.
///
/// Cell heights: ZPL II Programming Guide Table 31, "8 dot/mm (203 dpi)
/// Printhead" character matrices. Baselines: Table 29, "Intercharacter Gap
/// and Baseline Parameters". Font `0` (scalable default font) is the
/// guide's `Default: 15 x 12` matrix with baseline `3 x HEIGHT/4`
/// (`3*15/4 = 11.25` rounded to 11). The integration test
/// `zpl/tests/fonts.rs` duplicates these values; a change to either side
/// is a review event.
const GUIDE_CELLS: &[(u8, u32, u32)] = &[
    (b'A', 9, 7),
    (b'B', 11, 11),
    (b'C', 18, 14),
    (b'D', 18, 14),
    (b'E', 28, 23),
    (b'F', 26, 21),
    (b'G', 60, 48),
    (b'H', 21, 21),
    (b'0', 15, 11),
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

/// Per-letter metrics for the bundled face: each letter's cell/baseline
/// come from the guide; the ppem is fitted so the face's ascent lands on
/// the published baseline, and advances are measured at that ppem.
fn builtin_metric(face: &FontVec, cell_height: u32, baseline: u32) -> FontMetrics {
    let upem = face.units_per_em().unwrap_or(2048.0);
    let ascent = face.ascent_unscaled();
    debug_assert!(ascent > 0.0 && upem > 0.0);
    // ppem at which round(ascent * ppem / upem) == baseline.
    let ppem = (baseline as f32 * upem / ascent).round().max(1.0);
    let baseline_measured = (ascent * ppem / upem).round().max(1.0) as u32;
    FontMetrics {
        cell_height,
        baseline: baseline_measured,
        advances: measure_advances(face, ppem),
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
            .map(|&(_, cell, baseline)| builtin_metric(&face, cell, baseline))
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
        let idx = GUIDE_CELLS.iter().position(|&(c, _, _)| c == letter)?;
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
        },
    })
}
