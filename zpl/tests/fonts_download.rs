//! Downloaded-font CELL model tests (Task R5): the guide's downloadable-
//! font metric model end to end - `~DY` downloads the bytes (p.181-183),
//! `^A@` selects the font and carries the field's cell (p.62), and
//! `font_data_from_ttf` derives the cell height, baseline, and advances
//! from the downloaded font's own header tables (`head`/`hhea`/`hmtx`;
//! "Baseline numbers define where the baseline is located in relationship
//! to the top of the matrix", Fonts and Barcodes p.1581-1582).
//!
//! No reference oracle exists for downloaded fonts (the reference service
//! does not process them): these tests pin the guide's spec with values
//! recomputed independently from the same bytes, so a fixture change
//! cannot hide a broken metric rule.

use ab_glyph::{Font, FontVec};
use zpl::diag::Diagnostics;
use zpl::fonts::{font_data_from_ttf, CapModel, DefaultFontProvider};
use zpl::ir::Field;
use zpl::render::{render, RenderOptions};
use zpl::state::Memory;

mod common;

/// The bundled fixture face, committed next to its license at
/// `zpl/tests/fixtures/` (DejaVu 2.37 `DejaVuSansCondensed.ttf` +
/// `LICENSE-DejaVu.txt`). Compact enough to hex a full `~DY` ASCII-hex
/// download inside a test; tests never fetch it.
const FIXTURE_TTF: &[u8] = include_bytes!("fixtures/DejaVuSansCondensed.ttf");

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        write!(out, "{b:02X}").expect("write into a String");
    }
    out
}

/// `~DY` header (guide p.181): `d:f` name, `b` = A (ASCII hex), `x` =
/// extension (`T` = `.TTF`, `E` = `.TTE`), `t` = byte count, `w` = bytes
/// per row, `data`. A full download of real font bytes stores them
/// verbatim under the normalized name, and the `w` field is ignored for
/// fonts (guide p.183: "`.TTF` images: value ignored") - a nonzero row
/// width changes nothing.
#[test]
fn dy_downloads_store_font_bytes_verbatim() {
    for (ext, name) in [("T", "FONTFILE.TTF"), ("E", "FONTFILE.TTE")] {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let src = format!(
            "~DYE:{name},A,{ext},{},0,{}^XA^XZ",
            FIXTURE_TTF.len(),
            hex(FIXTURE_TTF)
        );
        let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
        assert_eq!(
            ctx.diags.items.len(),
            0,
            "clean download: {:?}",
            ctx.diags.items
        );
        assert_eq!(
            ctx.memory.fonts.get(name).map(Vec::as_slice),
            Some(FIXTURE_TTF),
            "the {ext} download stored the font bytes verbatim"
        );
    }
    // w (bytes per row) is ignored for fonts - same bytes with w = 123.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let src = format!(
        "~DYE:FONTFILE.TTF,A,T,{},123,{}^XA^XZ",
        FIXTURE_TTF.len(),
        hex(FIXTURE_TTF)
    );
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(ctx.diags.items.len(), 0, "{:?}", ctx.diags.items);
    assert_eq!(
        ctx.memory.fonts.get("FONTFILE.TTF").map(Vec::as_slice),
        Some(FIXTURE_TTF),
        "w is ignored for .TTF downloads (guide p.183)"
    );
}

/// The downloaded-font cell model: `font_data_from_ttf` derives the cell
/// from the downloaded font's own header metrics. `^A@` p.62 - `h` is
/// "the height in dots of the entire character block", `w` its width; the
/// character block is the font's hhea vertical extent, the baseline sits
/// at the font's ascent down from the top of the cell (p.1581-1582), and
/// the advances are the font's own hmtx widths. Every value is recomputed
/// here independently from the same bytes (reference size 100 ppem).
#[test]
fn downloaded_cell_metrics_follow_the_fonts_own_header() {
    let fd = font_data_from_ttf(FIXTURE_TTF).expect("fixture parses as a TTF");
    let m = &fd.metrics;
    let face = FontVec::try_from_vec(FIXTURE_TTF.to_vec()).expect("fixture parses");
    let upem = face.units_per_em().expect("head unitsPerEm");
    assert!(upem > 0.0);
    let (ascent, descent) = (face.ascent_unscaled(), face.descent_unscaled());
    assert!(
        descent < 0.0,
        "hhea descent of the fixture is below the baseline"
    );

    // The character block: the hhea vertical extent at the reference size.
    let cell = ((ascent - descent) * 100.0 / upem).round().max(1.0) as u32;
    assert_eq!(m.cell_height, cell, "cell = the entire character block");

    // The baseline: the font's ascent down from the top of the cell,
    // clamped into the cell.
    let baseline = ((ascent * 100.0 / upem).round().max(1.0) as u32).min(cell);
    assert_eq!(m.baseline, baseline);
    assert!(m.baseline > 0 && m.baseline < m.cell_height);

    // TrueType model, not the bitmap cap rule.
    assert!(matches!(m.cap, CapModel::Ascent));

    // Advances: the font's own hmtx widths, one slot per printable char.
    assert_eq!(m.advances.len(), 95);
    for c in 32u8..=126 {
        let gid = face.glyph_id(c as char);
        let adv = (face.h_advance_unscaled(gid) * 100.0 / upem).round() as u16;
        assert_eq!(
            m.advances[(c - 32) as usize],
            adv,
            "advance of '{}'",
            c as char
        );
    }
    assert!(
        m.advances.iter().all(|&a| a > 0),
        "every printable glyph of the fixture has a width"
    );
}

/// `^A@` selects the downloaded font and the IR carries the field's cell:
/// `h`/`w` land on the text field (50 x 50 dots - "in dots of the entire
/// character block", p.62), the name (device stripped) is preserved with
/// its extension, and the field renders warning-free. The drawn advance
/// scales the font's own width by `h / cell_height` (the cell model: the
/// character block spans `h`, so at cell `x` the advance is
/// `h_adv x x / (ascent - descent)`); the 100-ppem reference table
/// reproduces that direct scale within one dot, the reference-
/// quantization corner the README documents.
#[test]
fn at_select_carries_cell_and_scaled_advance() {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let src = format!(
        "~DYE:FONTFILE.TTF,A,T,{},0,{}^XA^PW200^LL60^FO10,10^A@N,50,50,E:FONTFILE.TTF^FDH^FS^XZ",
        FIXTURE_TTF.len(),
        hex(FIXTURE_TTF)
    );
    let (label, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "parse must be clean: {:?}",
        ctx.diags.items
    );
    let Field::Text(t) = &label.fields[0] else {
        panic!("expected a text field");
    };
    assert_eq!(t.font_name.as_deref(), Some("FONTFILE.TTF"));
    assert_eq!((t.font_height, t.font_width), (50, 50), "the ^A@ cell");

    // The drawn advance of 'H' tracks the direct cell-model scale
    // (`h_adv x cell / (ascent - descent)`), within the reference
    // quantization bound of one dot at any cell.
    let m = &font_data_from_ttf(FIXTURE_TTF).expect("metrics").metrics;
    let ref_adv = u32::from(m.advances[(b'H' - 32) as usize]);
    let drawn_at =
        |cell: u32| (ref_adv as f32 * cell as f32 / m.cell_height.max(1) as f32).round() as i64;
    let face = FontVec::try_from_vec(FIXTURE_TTF.to_vec()).expect("fixture parses");
    let h_adv = face.h_advance_unscaled(face.glyph_id('H'));
    let block = face.ascent_unscaled() - face.descent_unscaled();
    let direct_at = |cell: u32| (h_adv * cell as f32 / block).round() as i64;
    for cell in [50u32, 60, 100] {
        assert!(
            (drawn_at(cell) - direct_at(cell)).abs() <= 1,
            "at cell {cell}: scaled {} vs direct {}",
            drawn_at(cell),
            direct_at(cell)
        );
    }

    let opts = RenderOptions {
        dpi: 203,
        fonts: &DefaultFontProvider::new(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = render(&label, &opts).expect("render with the downloaded font");
    assert_eq!(out.warnings.len(), 0, "no warning: {:?}", out.warnings);
}

// --- ~DB bitmap font, ~DE encoding table, ~DV vector object ---------------

#[test]
fn db_downloads_bitmap_font_header_and_data() {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // Guide format: ~DBd:o.x,a,h,w,base,space,#char,copyright,data.
    // Character 65 = 'A': #065.8.6.0.0.8.<hex rows>.
    let src =
        "~DBR:TIMES.FNT,N,24,12,20,10,2,ZEBRA 1992,#065.8.6.0.0.8.0018002400420081008100420024001800^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "clean download: {:?}",
        ctx.diags.items
    );
    let stored = ctx.memory.fonts.get("TIMES.FNT").expect("stored");
    assert!(stored.starts_with(b"#065"), "structured data stored raw");
    // Missing required header values are rejected with a warning.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let src = "~DBR:TIMES.FNT^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert!(
        !ctx.diags.items.is_empty(),
        "missing header must warn: {:?}",
        ctx.diags.items
    );
    assert!(!ctx.memory.fonts.contains_key("TIMES.FNT"));
}

#[test]
fn de_downloads_encoding_table_decoded() {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // The guide's own example shape: s = the decoded byte count (the
    // "27848 two-digit hexadecimal values" of the JIS example), so the
    // 16 hex pairs here declare s = 16.
    let src = "~DEE:EXAMPLE.DAT,16,00310041003200420033004300440034^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "clean download: {:?}",
        ctx.diags.items
    );
    assert_eq!(
        ctx.memory.encodings.get("EXAMPLE.DAT"),
        Some(&vec![
            0x00, 0x31, 0x00, 0x41, 0x00, 0x32, 0x00, 0x42, //
            0x00, 0x33, 0x00, 0x43, 0x00, 0x44, 0x00, 0x34,
        ]),
        "decoded hex pairs stored"
    );
    // Declared size vs decoded mismatch -> warning, nothing stored.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let src = "~DEE:EXAMPLE.DAT,4,0031^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert!(
        !ctx.diags.items.is_empty(),
        "size mismatch must warn: {:?}",
        ctx.diags.items
    );
    assert!(ctx.memory.encodings.is_empty());
}

#[test]
fn dv_downloads_vector_object_by_extension() {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // ~DVd:f,b,x,t,w,data with x = T (TrueType): stores under .TTF.
    let src = "~DVE:FONT.TTF,A,T,4,0,41424344^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "clean download: {:?}",
        ctx.diags.items
    );
    assert_eq!(
        ctx.memory.fonts.get("FONT.TTF").map(Vec::as_slice),
        Some(&b"ABCD"[..]),
        "hex payload stored verbatim"
    );
}
