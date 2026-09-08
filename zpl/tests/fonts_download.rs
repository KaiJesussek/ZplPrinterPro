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
    // Guide format (p.170): ~DBd:o.x,a,h,w,base,space,#char,copyright,data
    // with the data as #code.h.w.x.y.i. followed by O/F row lines (the
    // guide's own TIMES example shape: F marks black, O white).
    let src =
        "~DBR:TIMES.FNT,N,8,6,6,2,2,ZEBRA 1992,#065.4.4.0.2.6.OFFOFOOF\nOFFO\nOFFOOFFO\nOFFO^XA^XZ";
    let (_, ctx) = common::parse_ctx(src.as_bytes(), &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "clean download: {:?}",
        ctx.diags.items
    );
    let stored = ctx.memory.fonts.get("TIMES.FNT").expect("stored");
    assert!(
        stored.starts_with(b"R:TIMES.FNT"),
        "the full record (header + data) is stored for later parsing"
    );

    // The stored record parses into a renderable bitmap font.
    let font = zpl::fonts::parse_bitmap_font(stored).expect("record parses");
    assert_eq!(font.cell_h, 8, "header height");
    assert_eq!(font.baseline, 6, "header baseline");
    assert_eq!(font.space, 2, "header space width");
    let a = font.glyphs.get(&65).expect("char 065 = 'A'");
    assert_eq!(a.h, 4, "glyph rows");
    assert_eq!(a.advance, 6, "glyph advance (typesetting motion)");
    assert!(!a.rows.is_empty());

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

#[test]
fn db_bitmap_font_renders_glyphs() {
    // A tiny solid glyph (char 66 = 'B', code 066): a 3x3 block. Downloaded
    // at cell 5 (h=5, w=5, base=4, space=2) and drawn by ^A@N,10,10 at
    // double scale (eff = 10/5 = 2), the glyph inks a 6x6 block two rows
    // down from the cell top (y = 1 -> row 2..7) at the field origin.
    let src = "~DBR:T.FNT,N,5,5,4,2,1,X,#066.3.3.0.1.4.FFF\nFFF\nFFF^XA^FO10,10^A@N,10,10,R:T.FNT^FDB^FS^XZ";
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = zpl::state::ParseContext::new(&mut diags, &mut memory);
    zpl::cmd::parse_stream(&mut zpl::scanner::Scanner::new(src.as_bytes()), &mut ctx);
    let label = ctx.pending_label.take().expect("format closed");
    assert_eq!(ctx.diags.items.len(), 0, "clean: {:?}", ctx.diags.items);
    assert_eq!(ctx.memory.fonts.len(), 1);
    let opts = RenderOptions {
        dpi: 203,
        fonts: &DefaultFontProvider::new(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = render(&label, &opts).expect("render");
    let bmp = &out.bitmap;
    let ink: Vec<(u32, u32)> = (0..bmp.height)
        .flat_map(|y| (0..bmp.width).map(move |x| (x, y)))
        .filter(|&(x, y)| bmp.get(x, y))
        .collect();
    assert!(!ink.is_empty(), "the bitmap font glyph must paint");
    let xs: Vec<u32> = ink.iter().map(|&(x, _)| x).collect();
    let ys: Vec<u32> = ink.iter().map(|&(_, y)| y).collect();
    let (x0, x1) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
    let (y0, y1) = (*ys.iter().min().unwrap(), *ys.iter().max().unwrap());
    // 3x3 glyph at eff 2 = 6x6, x offset 0, y offset 1 -> rows 12..17,
    // cols 10..15.
    assert!(
        (y0, y1) == (12, 17),
        "glyph vertical span (rows {y0}..{y1}, want 12..17)"
    );
    assert!(
        (x0, x1) == (10, 15),
        "glyph horizontal span (cols {x0}..{x1}, want 10..15)"
    );
    // The 3x3 solid block fills 36 dots at 2x.
    assert_eq!(ink.len(), 36, "solid 3x3 scaled by 2");
}
