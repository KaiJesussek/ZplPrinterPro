//! Font provider and downloaded-font tests (Task 11): built-in Zebra font
//! coverage, guide-anchored metric tables, printable-ASCII advances, and
//! the `^A@` downloaded-font resolution seam through `Memory.fonts`.

use zpl::fonts::{font_data_from_ttf, DefaultFontProvider, FontProvider, FontSpec};
use zpl::render::RenderOptions;
use zpl::state::Memory;

mod common;

/// The bundled fixture face, committed next to its license at
/// `zpl/tests/fixtures/` (DejaVu 2.37 `DejaVuSansCondensed.ttf` +
/// `LICENSE-DejaVu.txt`). Tests never fetch it.
const FIXTURE_TTF: &[u8] = include_bytes!("fixtures/DejaVuSansCondensed.ttf");

/// Guide-anchored (letter, cell height, baseline) triplets: cell heights
/// are Table 31 "8 dot/mm (203 dpi) Printhead" character matrices, baselines
/// are Table 29 "Intercharacter Gap and Baseline Parameters" (both ZPL II
/// Programming Guide, "Fonts and Barcodes"). Font 0's baseline is the
/// guide's "3 x HEIGHT/4" = 3*15/4 = 11.25, rounded to 11. This duplicates
/// the tables in `zpl/src/fonts.rs`; a change to either side is a review
/// event.
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

fn provider() -> DefaultFontProvider {
    DefaultFontProvider::new()
}

/// Every built-in font letter is served, with guide-exact cell/baseline,
/// a baseline inside the cell, and a full nonzero advance table.
#[test]
fn builtin_letters_are_served_with_guide_metrics() {
    let p = provider();
    for &(letter, cell, baseline) in GUIDE_CELLS {
        let fd = p
            .get(&FontSpec::Zebra(letter))
            .unwrap_or_else(|| panic!("font {letter:?} must be served"));
        assert!(!fd.ttf.is_empty(), "bundled ttf bytes present for {letter:?}");
        assert!(
            ab_glyph::FontVec::try_from_vec(fd.ttf.clone()).is_ok(),
            "bundled ttf parses for {letter:?}"
        );
        let m = &fd.metrics;
        assert_eq!(m.cell_height, cell, "cell height for {letter:?}");
        assert_eq!(m.baseline, baseline, "baseline for {letter:?}");
        assert!(
            m.baseline <= m.cell_height,
            "baseline inside the cell for {letter:?}"
        );
        assert_eq!(m.advances.len(), 95, "advance table len (char - 32) for {letter:?}");
        assert!(
            m.advances.iter().all(|&a| a > 0),
            "every printable ASCII advance nonzero for {letter:?}"
        );
    }
}

/// Zebra letters outside A-H/0, other bytes, and (on the default provider)
/// Downloaded names are all unserved.
#[test]
fn unknown_letters_and_downloaded_spec_return_none() {
    let p = provider();
    for letter in [b'1', b'2', b'9', b'I', b'Z', b'a', b' ', 0x00, 0xFF] {
        assert!(
            p.get(&FontSpec::Zebra(letter)).is_none(),
            "Zebra {letter:02X} must be unserved"
        );
    }
    // Downloaded fonts are resolved from printer memory by the renderer,
    // never by the provider (documented Task 11 seam).
    assert!(p.get(&FontSpec::Downloaded("x".into())).is_none());
}

/// The downloaded-font construction path (what the renderer builds from
/// `Memory.fonts`): any parseable TTF yields self-consistent metrics, a
/// garbage blob yields `None`.
#[test]
fn downloaded_font_data_from_ttf_measures_sane_metrics() {
    let fd = font_data_from_ttf(FIXTURE_TTF).expect("fixture parses as a TTF");
    assert_eq!(fd.ttf, FIXTURE_TTF, "returns the same bytes");
    let m = &fd.metrics;
    assert!(m.cell_height > 0 && m.baseline > 0 && m.baseline < m.cell_height);
    assert_eq!(m.advances.len(), 95);
    assert!(m.advances.iter().all(|&a| a > 0));
    assert!(font_data_from_ttf(b"not a font at all").is_none());
}

/// A label whose text field names a stored font (`^A@` + `~DY`) resolves
/// through `Memory.fonts`: render succeeds, no warning.
#[test]
fn downloaded_font_resolves_through_render() {
    let mut diags = zpl::diag::Diagnostics::default();
    let mut memory = Memory::default();
    memory.fonts.insert("FONTFILE.TTF".into(), FIXTURE_TTF.to_vec());
    let (label, ctx) = common::parse_ctx(
        b"^XA^PW120^LL40^FO5,5^A@N,20,20,E:FONTFILE.TTF^FDhello^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.diags.items.len(), 0, "parse of ^A@ must be clean: {:?}", ctx.diags.items);
    let opts = RenderOptions {
        dpi: 203,
        fonts: &provider(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = zpl::render::render(&label, &opts).expect("render with a downloaded font");
    assert!(out.warnings.is_empty(), "no warning with the font stored: {:?}", out.warnings);
}

/// A plain text field resolves its built-in Zebra font (`^A`/`^CF`
/// default) through the same render seam with no warning.
#[test]
fn builtin_font_resolves_through_render() {
    let mut diags = zpl::diag::Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(
        b"^XA^PW120^LL40^FO5,5^AA,20,20^FDhello^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let opts = RenderOptions {
        dpi: 203,
        fonts: &provider(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = zpl::render::render(&label, &opts).expect("render");
    assert!(out.warnings.is_empty(), "built-in font must not warn: {:?}", out.warnings);
}

/// A text field naming a font that was never downloaded warns and skips;
/// the label still renders.
#[test]
fn missing_downloaded_font_warns_and_skips() {
    let mut diags = zpl::diag::Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(
        b"^XA^PW120^LL40^FO5,5^A@N,20,20,E:NOPE.TTF^FDhello^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let opts = RenderOptions {
        dpi: 203,
        fonts: &provider(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = zpl::render::render(&label, &opts).expect("render never fails on a missing font");
    assert!(!out.warnings.is_empty(), "missing font must warn: {:?}", out.warnings);
    assert!(
        out.warnings.iter().any(|w| w.contains("NOPE.TTF")),
        "warning names the font: {:?}",
        out.warnings
    );
}
