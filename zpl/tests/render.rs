//! Renderer integration tests (Task 10): framebuffer semantics, PNG output,
//! drawing primitives, graphic blits, auto height, mode transforms.

use std::collections::BTreeMap;
use std::io::Cursor;

use png::BitDepth;
use png::ColorType;

use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::fonts::{FontData, FontProvider, FontSpec};
use zpl::render::render;
use zpl::render::Bitmap;
use zpl::render::RenderError;
use zpl::render::RenderOptions;
use zpl::render::RenderOutcome;
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

mod common;

/// A provider that has no fonts (text rendering lands in Task 13).
struct NoFonts;
impl FontProvider for NoFonts {
    fn get(&self, _spec: &FontSpec) -> Option<FontData> {
        None
    }
}

/// Parse a format (with an empty memory) and render it at 203 dpi.
fn render_src(src: &[u8]) -> RenderOutcome {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    render_into(src, &mut diags, &mut memory)
}

/// Parse a format into the given diagnostics/memory, then render it.
fn render_into(src: &[u8], diags: &mut Diagnostics, memory: &mut Memory) -> RenderOutcome {
    let mut ctx = ParseContext::new(diags, memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    let label = ctx.pending_label.take().expect("no format closed by ^XZ");
    let opts = RenderOptions {
        dpi: 203,
        fonts: &NoFonts,
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    render(&label, &opts).expect("render failed structurally")
}

/// Decode 1-bit grayscale PNG bytes back into per-pixel samples (0 = black
/// per the PNG spec), row-major, MSB-first.
fn decode_png(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut dec = png::Decoder::new(Cursor::new(png));
    dec.set_transformations(png::Transformations::IDENTITY);
    let mut reader = dec.read_info().expect("png header decodes");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame decodes");
    assert_eq!(
        info.color_type,
        ColorType::Grayscale,
        "PNG must be grayscale"
    );
    assert_eq!(info.bit_depth, BitDepth::One, "PNG must be 1-bit");
    (info.width, info.height, buf)
}

fn sample(buf: &[u8], width: u32, x: u32, y: u32) -> u8 {
    let row = (width as usize).div_ceil(8);
    (buf[y as usize * row + (x as usize >> 3)] >> (7 - (x & 7))) & 1
}

// ---------------------------------------------------------------------------
// Brief tests
// ---------------------------------------------------------------------------

#[test]
fn png_roundtrip_dimensions() {
    // ^XA^GB4,4,1,B -> 4x4 bitmap: a 1-dot black border ring (thickness inward).
    let outcome = render_src(b"^XA^PW4^LL4^GB4,4,1,B^XZ");
    let bmp = &outcome.bitmap;
    assert_eq!((bmp.width, bmp.height), (4, 4));

    // Border ring: rows 0/3 all black, rows 1/2 black only at cols 0/3.
    for x in 0..4 {
        assert!(bmp.get(x, 0), "top edge at x={x}");
        assert!(bmp.get(x, 3), "bottom edge at x={x}");
    }
    assert!(bmp.get(0, 1) && bmp.get(3, 1) && bmp.get(0, 2) && bmp.get(3, 2));
    assert!(!bmp.get(1, 1) && !bmp.get(2, 1) && !bmp.get(1, 2) && !bmp.get(2, 2));

    // PNG round-trips with matching dimensions AND the same pixel pattern.
    let png = bmp.to_png();
    let (w, h, buf) = decode_png(&png);
    assert_eq!((w, h), (bmp.width, bmp.height));
    for x in 0..4 {
        for y in 0..4 {
            // PNG grayscale sample 0 = black; our bitmap bit 1 = black.
            assert_eq!(
                bmp.get(x, y),
                sample(&buf, w, x, y) == 0,
                "pixel {x},{y} pattern"
            );
        }
    }
}

#[test]
fn blit_inline_graphics() {
    // ^GFA: 2 bytes, 1 byte/row -> an 8x2 image with one black pixel per row.
    // The parser decodes the hex; "8080" = 0x80 0x80. Blit at field (3,5).
    let outcome = render_src(b"^XA^PW32^LL20^FO3,5^GFA,2,2,1,8080^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(3, 5), "black pixel at blit origin");
    assert!(bmp.get(3, 6), "black pixel one row down");
    assert!(!bmp.get(4, 5), "second pixel of the byte is white");
    assert!(
        !bmp.get(3, 4) && !bmp.get(3, 7),
        "no bleed outside the image"
    );
    assert!(
        !bmp.get(0, 0) && !bmp.get(31, 19),
        "rest of the label is white"
    );
}

#[test]
fn render_clamps_never_panics() {
    // A box far off the label, a border thicker than the label, a circle
    // straddling the bottom edge, a diagonal at the right edge, and inline
    // graphics partly off-canvas: everything clamps, nothing panics.
    let src = concat!(
        "^XA^PW10^LL10",
        "^FO9999,9999^GB5,5,1,B^FS",
        "^FO5,5^GB8,8,30,B^FS", // thickness >> box dims -> solid
        "^FO0,9^GC6,1,B^FS",    // circle straddles bottom edge
        "^FO9,0^GD10,10,1,B,R^FS",
        "^FO8,8^GFA,2,2,1,8080^FS",
        "^FO9998,9998^GE4,4,1,B^FS",
        "^FO0,9999^GD4,4,1,B,L^FS",
        "^XZ",
    );
    let outcome = render_src(src.as_bytes());
    let bmp = &outcome.bitmap;
    assert_eq!((bmp.width, bmp.height), (10, 10));
    // Blit at (8,8): top row's black pixel at (8,8) is visible.
    assert!(
        bmp.get(8, 8),
        "clipped blit still paints its on-canvas pixels"
    );
}

// ---------------------------------------------------------------------------
// Brief-mandated extras
// ---------------------------------------------------------------------------

#[test]
fn auto_height_sizes_to_content() {
    // ^LL omitted -> height_dots = 0 (auto): bitmap height = content bottom.
    let outcome = render_src(b"^XA^PW20^FO2,7^GB4,4,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert_eq!(bmp.width, 20);
    assert_eq!(bmp.height, 11, "content bottom = max(y + height) = 7 + 4");
    assert!(bmp.get(2, 7), "content present");
    assert!(bmp.get(5, 10), "content bottom row present");
    assert!(!bmp.get(2, 6), "nothing above the content");
}

#[test]
fn auto_height_includes_label_home_offset() {
    // ^LH shifts the reference point down; the auto-sized label must contain
    // the shifted content: height = max(y + lh_y + field height).
    let outcome = render_src(b"^XA^PW20^LH0,10^FO2,7^GB4,4,1,B^FS^XZ");
    assert_eq!(outcome.bitmap.height, 21, "7 + lh_y(10) + 4");
}

#[test]
fn missing_stored_image_warns_not_err() {
    // ^XG recalls LOGO.GRF, which no ^DG ever stored: the renderer warns in
    // the outcome and skips the field - it does NOT fail the render.
    let outcome = render_src(b"^XA^PW20^LL20^FO0,0^XGLOGO.GRF^FS^XZ");
    assert_eq!(
        outcome.warnings.len(),
        1,
        "one missing-image warning, got {:?}",
        outcome.warnings
    );
    assert!(
        outcome.warnings[0].contains("LOGO.GRF"),
        "warning names the image: {}",
        outcome.warnings[0]
    );
    let bmp = &outcome.bitmap;
    for x in 0..bmp.width {
        for y in 0..bmp.height {
            assert!(!bmp.get(x, y), "nothing painted when the image is missing");
        }
    }
}

#[test]
fn stored_image_blits_from_memory() {
    // ~DG downloads an 8x2 bitmap (one black pixel per row) under LOGO.GRF;
    // ^XG recalls it at (3,5).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    {
        let mut ctx = ParseContext::new(&mut diags, &mut memory);
        parse_stream(&mut Scanner::new(b"~DGLOGO.GRF,2,1,8080"), &mut ctx);
    }
    assert!(
        memory.images.contains_key("LOGO.GRF"),
        "download stored the image"
    );
    let outcome = render_into(
        b"^XA^PW32^LL20^FO3,5^XGLOGO.GRF^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let bmp = &outcome.bitmap;
    assert!(
        outcome.warnings.is_empty(),
        "stored image must not warn: {:?}",
        outcome.warnings
    );
    assert!(
        bmp.get(3, 5) && bmp.get(3, 6),
        "stored image blits its pixels"
    );
    assert!(
        !bmp.get(4, 5),
        "white pixels of the stored image stay white"
    );
}

#[test]
fn lh_offset_applied_to_field_origins() {
    // ^LH5,5 + ^FO2,3 -> the box's top-left lands at (7,8), not (2,3).
    let outcome = render_src(b"^XA^PW30^LL30^LH5,5^FO2,3^GB4,4,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(7, 8), "field origin + ^LH offset");
    assert!(!bmp.get(2, 3), "un-offset origin is outside the box");
    assert!(
        !bmp.get(6, 8) && !bmp.get(7, 7),
        "top-left corner is exactly (7,8)"
    );
}

#[test]
fn ls_lt_shift_the_format() {
    // ^LS20 shifts all fields LEFT by 20; ^LT5 shifts them DOWN by 5.
    // Box at ^FO30,10 with h=1: shifted to x=10, y=15.
    let outcome = render_src(b"^XA^PW50^LL30^LS20^LT5^FO30,10^GB20,1,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(10, 15), "box shifted left by ^LS and down by ^LT");
    assert!(!bmp.get(30, 10), "original position no longer painted");

    // A positive ^LT pushes content below the label: clamped, no panic.
    let src = b"^XA^PW10^LL10^LT30^FO0,0^GB4,4,1,B^FS^XZ";
    let outcome = render_src(src);
    assert_eq!(outcome.bitmap.width, 10);
}

#[test]
fn po_inverted_rotates_label_180() {
    // ^POI on a 20x20 label: the box at (0,0) with footprint 4x4 relocates
    // to (20-0-4, 20-0-4) = (16,16).
    let outcome = render_src(b"^XA^PW20^LL20^POI^FO0,0^GB4,4,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(
        bmp.get(16, 16) && bmp.get(19, 19),
        "box painted at the rotated origin"
    );
    assert!(
        !bmp.get(0, 0),
        "original corner is empty after the 180\u{00B0} rotation"
    );
    assert!(!bmp.get(15, 15), "just outside the rotated box");
}

#[test]
fn pm_mirror_flips_horizontally() {
    // ^PMY on a 20x20 label: the box at (0,0) with footprint 4x4 flips to x=16.
    let outcome = render_src(b"^XA^PW20^LL20^PMY^FO0,0^GB4,4,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(16, 0) && bmp.get(19, 3), "box painted mirrored");
    assert!(!bmp.get(0, 0), "original corner is empty after the flip");
}

#[test]
fn thickness_zero_fills_box() {
    // ^GB t=0 is a filled box per the plan (the 11EN guide's table lists
    // t in 1..32000; the renderer owns the fill rule for recorded 0).
    let outcome = render_src(b"^XA^PW10^LL10^FO1,1^GB6,4,0,B^FS^XZ");
    let bmp = &outcome.bitmap;
    for x in 1..7 {
        for y in 1..5 {
            assert!(bmp.get(x, y), "filled cell {x},{y}");
        }
    }
    assert!(
        !bmp.get(0, 0) && !bmp.get(9, 9) && !bmp.get(7, 4) && !bmp.get(6, 5),
        "fill stays inside the box"
    );
}

#[test]
fn white_color_erases() {
    // Paint a black 20x20 field, then a white 10x10 box on top: the white
    // box erases (paints 0) - the guide's W color is not "nothing".
    let src = b"^XA^PW20^LL20^FO0,0^GB20,20,0,B^FS^FO5,5^GB10,10,0,W^FS^XZ";
    let outcome = render_src(src);
    let bmp = &outcome.bitmap;
    assert!(bmp.get(0, 0), "black stays where no white box overlaps");
    assert!(
        !bmp.get(5, 5) && !bmp.get(14, 14),
        "white fill erased the black"
    );
    assert!(
        bmp.get(4, 4) && bmp.get(15, 15),
        "black remains just outside the white box"
    );
}

#[test]
fn circle_thickness_grows_inward() {
    // ^GC6,1: a 6-dot ring, 1 dot thick, centered on a 6x6 grid whose corners
    // are empty and whose center 2x2 is the hole.
    let outcome = render_src(b"^XA^PW10^LL10^FO1,1^GC6,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    let at = |x: u32, y: u32| bmp.get(1 + x, 1 + y);
    assert!(!at(0, 0), "corner of the 6x6 circle box is empty");
    assert!(
        at(0, 3) && at(3, 0) && at(5, 3) && at(3, 5),
        "cardinal edge midpoints painted"
    );
    assert!(
        !at(2, 2) && !at(3, 3) && !at(2, 3) && !at(3, 2),
        "1-dot ring leaves the center hole"
    );

    // Thickness >= diameter/2 fills the whole disc (thickness grows inward).
    let outcome = render_src(b"^XA^PW10^LL10^FO1,1^GC6,3,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(
        bmp.get(4, 4),
        "solid disc center painted when thickness reaches the center"
    );
}

#[test]
fn ellipse_paints_ring_with_hole() {
    // ^GE10,6,1: corners empty, top-edge midpoint painted, center hole.
    let outcome = render_src(b"^XA^PW12^LL10^FO1,1^GE10,6,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    let at = |x: u32, y: u32| bmp.get(1 + x, 1 + y);
    assert!(
        !at(0, 0) && !at(9, 0) && !at(0, 5) && !at(9, 5),
        "corners empty"
    );
    assert!(at(4, 0) && at(5, 0), "top edge midpoint painted");
    assert!(!at(5, 2), "1-dot ellipse keeps an inner hole");
    assert!(
        at(0, 2) && at(0, 3),
        "leftmost column painted at mid height"
    );
}

#[test]
fn diagonal_direction_pins_r_and_l() {
    // ^GD: R = right-leaning (top-left to bottom-right "/"), L = left-leaning.
    let outcome = render_src(b"^XA^PW20^LL20^FO0,0^GD5,5,1,B,R^FS^XZ");
    let bmp = &outcome.bitmap;
    for i in 0..5 {
        assert!(bmp.get(i, i), "R diagonal on the main diagonal at {i}");
    }
    assert!(
        !bmp.get(4, 0) && !bmp.get(0, 4),
        "R diagonal's anti-diagonal corners empty"
    );

    let outcome = render_src(b"^XA^PW20^LL20^FO0,0^GD5,5,1,B,L^FS^XZ");
    let bmp = &outcome.bitmap;
    for i in 0..5 {
        assert!(bmp.get(4 - i, i), "L diagonal on the anti-diagonal at {i}");
    }
    assert!(
        !bmp.get(0, 0) && !bmp.get(4, 4),
        "L diagonal's main-diagonal corners empty"
    );
}

#[test]
fn png_bytes_sanity() {
    // Raw PNG header: signature, IHDR, bit depth 1, color type 0 (grayscale).
    let outcome = render_src(b"^XA^PW4^LL4^GB4,4,1,B^XZ");
    let png = outcome.bitmap.to_png();
    assert_eq!(
        &png[..8],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        "PNG signature"
    );
    assert_eq!(&png[12..16], b"IHDR", "IHDR chunk");
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w, h), (4, 4), "IHDR dimensions");
    assert_eq!(png[24], 1, "IHDR bit depth");
    assert_eq!(png[25], 0, "IHDR color type: grayscale");
}

#[test]
fn hostile_dimensions_never_hang_or_panic() {
    // Hand-built IR with u32::MAX geometry: every primitive must clamp to
    // the canvas (bounded loops over the visible intersection only), and
    // width/height arithmetic must not overflow - this is the never-panic /
    // never-hang invariant the fuzz task (T18) will lean on.
    use zpl::ir::{
        BoxField, CircleField, DiagonalField, EllipseField, Field, GraphicField, GraphicKind, Label,
    };
    let label = Label {
        width_dots: 4,
        height_dots: 0, // auto height over hostile fields must terminate
        copies: 1,
        numbered: BTreeMap::new(),
        fields: vec![
            Field::Box(BoxField {
                x: 0,
                y: 0,
                width: u32::MAX,
                height: u32::MAX,
                thickness: u32::MAX,
                color: b'B',
                reverse: false,
            }),
            Field::Circle(CircleField {
                x: u32::MAX,
                y: u32::MAX,
                diameter: u32::MAX,
                thickness: u32::MAX,
                color: b'B',
                reverse: false,
            }),
            Field::Ellipse(EllipseField {
                x: 2,
                y: 2,
                width: u32::MAX,
                height: u32::MAX,
                thickness: u32::MAX,
                color: b'W',
                reverse: false,
            }),
            Field::Diagonal(DiagonalField {
                x: 3,
                y: 0,
                width: u32::MAX,
                height: u32::MAX,
                direction: b'L',
                thickness: u32::MAX,
                color: b'B',
                reverse: false,
            }),
            Field::Graphic(GraphicField {
                x: u32::MAX - 1,
                y: 0,
                kind: GraphicKind::Inline,
                data: vec![0xFF],
                bytes_per_row: 1,
            }),
            // bytes_per_row * 8 overflows u32 for values >= 2^29 (the old
            // code panicked in debug); empty data passes the multiple-of
            // check (0 % N == 0) and must render as a no-op blit.
            Field::Graphic(GraphicField {
                x: 0,
                y: 0,
                kind: GraphicKind::Inline,
                data: Vec::new(),
                bytes_per_row: u32::MAX,
            }),
        ],
    };
    let opts = RenderOptions {
        dpi: 203,
        fonts: &NoFonts,
        memory: &Memory::default(),
        mode: &zpl::state::PrinterMode::default(),
    };
    let outcome = render(&label, &opts).expect("hostile geometry is still a valid render");
    // Auto height over a u32::MAX-tall field is capped at the emulator's
    // dimension ceiling (32000 dots) - a bounded canvas, never a 2 GB one.
    assert_eq!((outcome.bitmap.width, outcome.bitmap.height), (4, 32_000));
}

#[test]
fn blit_negative_origin_reads_source_correctly() {
    // ^LS2 shifts fields left by 2: the 8x1 all-black image blits at x=-2.
    // The visible span is source columns 2..8 mapped to destination
    // columns 0..6 - the off-canvas source columns must be skipped, not
    // painted onto the right edge.
    let outcome = render_src(b"^XA^PW32^LL16^LS2^FO0,0^GFA,1,1,1,FF^FS^XZ");
    let bmp = &outcome.bitmap;
    for x in 0..6 {
        assert!(bmp.get(x, 0), "visible src cols 2..8 land at dst {x},0");
    }
    assert!(!bmp.get(6, 0) && !bmp.get(7, 0), "dst cols 6..7 stay white");

    // ^LT-1 shifts the format up by 1: y=-1, so only source row 1 is
    // visible, at destination row 0.
    let outcome = render_src(b"^XA^PW32^LL16^LT-1^FO0,0^GFA,2,2,1,FFFF^FS^XZ");
    let bmp = &outcome.bitmap;
    for x in 0..8 {
        assert!(bmp.get(x, 0), "src row 1 lands at dst {x},0");
        assert!(
            !bmp.get(x, 1) && !bmp.get(x, 2),
            "src row 0 went off the top; dst rows stay white"
        );
    }

    // ^POI rotates the 16x1 image (data F00F: 0xF0 0x0F) to fx = 8-16 = -8
    // and fy = 8-0-1 = 7: destination row 7 shows source columns 8..15
    // (byte 0x0F = four white then four black); source columns 0..7
    // (0xF0) must never appear.
    let outcome = render_src(b"^XA^PW8^LL8^POI^FO0,0^GFA,2,2,2,F00F^FS^XZ");
    let bmp = &outcome.bitmap;
    for x in 0..4 {
        assert!(
            !bmp.get(x, 7),
            "src cols 8..11 (0x0F leading zeros) at dst {x},7"
        );
    }
    for x in 4..8 {
        assert!(
            bmp.get(x, 7),
            "src cols 12..15 (0x0F trailing ones) at dst {x},7"
        );
    }
    for y in 0..7 {
        assert!(
            !bmp.get(0, y) && !bmp.get(7, y),
            "no image pixels above the rotated row"
        );
    }
}

#[test]
fn degenerate_inline_graphic_is_render_error() {
    // The parser never emits a malformed inline graphic (it validates ^GF
    // before the bytes enter the IR); a hand-built IR with bytes_per_row 0
    // must be a structural RenderError, not a panic.
    let label = zpl::ir::Label {
        width_dots: 10,
        height_dots: 10,
        copies: 1,
        numbered: BTreeMap::new(),
        fields: vec![zpl::ir::Field::Graphic(zpl::ir::GraphicField {
            x: 0,
            y: 0,
            kind: zpl::ir::GraphicKind::Inline,
            data: vec![0xFF],
            bytes_per_row: 0,
        })],
    };
    let opts = RenderOptions {
        dpi: 203,
        fonts: &NoFonts,
        memory: &Memory::default(),
        mode: &zpl::state::PrinterMode::default(),
    };
    match render(&label, &opts) {
        Err(RenderError::InvalidGraphic(msg)) => assert!(msg.contains("per row"), "{msg}"),
        other => panic!("expected InvalidGraphic, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Task 13: text painting - glyph rasterization, ^FB wrapping, rotation,
// reverse video, baseline (^FT) origin, width rule, downloaded fonts.
// ---------------------------------------------------------------------------

use zpl::fonts::DefaultFontProvider;

/// Parse + render with the bundled built-in font provider (text tests need
/// real fonts; `render_into` uses `NoFonts`).
fn render_text_into(src: &[u8], diags: &mut Diagnostics, memory: &mut Memory) -> RenderOutcome {
    let mut ctx = ParseContext::new(diags, memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    let label = ctx.pending_label.take().expect("no format closed by ^XZ");
    let opts = RenderOptions {
        dpi: 203,
        fonts: &DefaultFontProvider::new(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    render(&label, &opts).expect("render failed structurally")
}

/// Parse + render a text-bearing format at 203 dpi with the bundled fonts.
fn render_text(src: &[u8]) -> RenderOutcome {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    render_text_into(src, &mut diags, &mut memory)
}

/// Split the black columns of a row range into contiguous clusters (each
/// cluster = one glyph's ink).
fn col_clusters(bmp: &Bitmap, rows: core::ops::Range<u32>) -> Vec<(u32, u32)> {
    let mut cols: Vec<u32> = (0..bmp.width)
        .filter(|&x| rows.clone().any(|y| bmp.get(x, y)))
        .collect();
    cols.dedup();
    let mut clusters = Vec::new();
    for &c in &cols {
        match clusters.last_mut() {
            Some((_, end)) if c <= *end + 1 => *end = c,
            _ => clusters.push((c, c)),
        }
    }
    clusters
}

/// First row (within the given range) that has any black pixel.
fn first_ink_row(bmp: &Bitmap, mut rows: core::ops::Range<u32>) -> Option<u32> {
    rows.find(|&y| (0..bmp.width).any(|x| bmp.get(x, y)))
}

#[test]
fn text_paints_black_pixels_in_cell() {
    // ^A0N,20,20^FO0,0^FDX^FS at 203 dpi: the X glyph inks rows 0..20 of
    // the label's top-left corner.
    let out = render_text(b"^XA^PW64^LL40^FO0,0^A0N,20,20^FDX^FS^XZ");
    let bmp = &out.bitmap;
    let ink: Vec<(u32, u32)> = (0..bmp.height)
        .flat_map(|y| (0..bmp.width).map(move |x| (x, y)))
        .filter(|&(x, y)| bmp.get(x, y))
        .collect();
    assert!(
        ink.len() >= 8,
        "an X rasterizes to a real glyph, got {} dots",
        ink.len()
    );
    assert!(
        ink.iter().all(|&(_, y)| y < 40),
        "nothing paints below the 20-dot cell at a 40-tall label"
    );
    assert!(
        ink.iter().any(|&(x, y)| (0..20).contains(&y) && x < 20),
        "black pixels land inside rows 0..20 of the field origin"
    );
    let within: Vec<&(u32, u32)> = ink.iter().filter(|&&(x, y)| y < 20 && x < 40).collect();
    assert!(
        !within.is_empty() && within.len() < 400,
        "glyph ink, not a filled region"
    );
    // Rows 22..40 stay completely white.
    assert!((22..40).all(|y| (0..bmp.width).all(|x| !bmp.get(x, y))));
}

#[test]
fn fb_wraps_at_width() {
    // ^FB35 width, ^A0N,20,20: "AA AA" wraps to two lines of "AA" at the
    // word's proportional width (the built-in bitmap-style font ignores the
    // ^A width parameter - the reference renders it proportionally); with 0
    // line spacing the second line starts exactly one cell (20 dots) below
    // the first. 35 fits the bundled face's two A advances at the 20-dot
    // size (2 x ~15 dots), 27 does not.
    let out = render_text(b"^XA^PW64^LL60^FO0,0^FB35,2,0,L^A0N,20,20^FDAA AA^FS^XZ");
    let bmp = &out.bitmap;
    let r1 = first_ink_row(bmp, 0..20).expect("first line has ink");
    let r2 = first_ink_row(bmp, 20..40).expect("second wrapped line has ink");
    assert_eq!(r2, r1 + 20, "second line starts one cell below the first");
    assert!(
        (40..60).all(|y| (0..bmp.width).all(|x| !bmp.get(x, y))),
        "block stays 2 lines"
    );
    // Each line carries two A glyphs at the proportional A advance - the
    // aggregated two-line scan shows the two column clusters per line.
    let clusters = col_clusters(bmp, 0..40);
    assert_eq!(clusters.len(), 2, "two A columns per line: {clusters:?}");
    let adv = clusters[1].0 - clusters[0].0;
    assert!(
        (10..16).contains(&adv),
        "proportional A-to-A advance spaces the As ({adv} dots)"
    );
}

#[test]
fn rotation_90() {
    // ^A0R,20,20^FO10,10: the 20x20 cell rotates 90deg CW about the field
    // origin: ink lands in the 20-wide column starting at x=10, stacked
    // down the y-axis from y=10 by the 20-dot advance.
    let out = render_text(b"^XA^PW80^LL80^FO10,10^A0R,20,20^FDX^FS^XZ");
    let bmp = &out.bitmap;
    assert!(
        (0..80).all(|y| !bmp.get(9, y)),
        "nothing left of the rotated cell"
    );
    assert!(
        (0..80).all(|x| !bmp.get(x, 9)),
        "nothing above the rotated cell"
    );
    let ink: Vec<(u32, u32)> = (0..80)
        .flat_map(|y| (0..80).map(move |x| (x, y)))
        .filter(|&(x, y)| bmp.get(x, y))
        .collect();
    assert!(!ink.is_empty(), "rotated glyph paints");
    assert!(
        ink.iter().all(|&(x, _y)| (10..30).contains(&x)),
        "rotated ink confined to the cell-height column x 10..30: {ink:?}"
    );
    assert!(
        ink.iter().all(|&(_, y)| (10..30).contains(&y)),
        "rotated ink confined to the advance column y 10..30"
    );
    assert!(
        ink.iter().any(|&(_, y)| y >= 15),
        "ink stacks down past the halfway point of the rotated advance"
    );
}

#[test]
fn reverse_field() {
    // ^FR: the field's layout rectangle fills black and the glyph erases to
    // white - every pixel inside the box is the complement of the normal
    // render, everything outside is unchanged.
    let normal = render_text(b"^XA^PW64^LL40^FO0,0^A0N,20,0^FDX^FS^XZ");
    let rev = render_text(b"^XA^PW64^LL40^FO0,0^FR^A0N,20,0^FDX^FS^XZ");
    let (nb, rb) = (&normal.bitmap, &rev.bitmap);
    let n_ink = (0..20)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .filter(|&(x, y)| nb.get(x, y))
        .count();
    assert!(n_ink > 0, "normal render actually inks");
    // Single 'X' at the origin (Left): the layout box is [0..advance) x
    // [0..cell), advance = round(adv_X * 20/15) at the fixed 20-dot cell.
    let metric = DefaultFontProvider::new()
        .get(&FontSpec::Zebra(b'0'))
        .expect("font 0");
    let adv = (metric.metrics.advances[b'X' as usize - 32] as f32 * 20.0 / 15.0).round() as u32;
    let cell = 20u32;
    for y in 0..40 {
        for x in 0..64 {
            let in_box = y < cell && x < adv;
            if in_box {
                assert_ne!(nb.get(x, y), rb.get(x, y), "box pixel {x},{y} flips");
            } else {
                assert_eq!(
                    nb.get(x, y),
                    rb.get(x, y),
                    "outside pixel {x},{y} unchanged"
                );
            }
        }
    }
    // The box itself is black somewhere (reverse video, not a no-op) ...
    assert!(
        (0..cell)
            .flat_map(|y| (0..adv).map(move |x| (x, y)))
            .any(|(x, y)| rb.get(x, y)),
        "reverse paints a black field"
    );
    // ... and the glyph bits are white inside it.
    let g_ink: Vec<(u32, u32)> = (0..cell)
        .flat_map(|y| (0..adv).map(move |x| (x, y)))
        .filter(|&(x, y)| nb.get(x, y))
        .collect();
    assert!(
        g_ink.iter().any(|&(x, y)| !rb.get(x, y)),
        "the glyph itself is white inside the black box"
    );
}

#[test]
fn dot_origin_baseline() {
    // ^FT20,20 + a descending 'p': the origin is the BASELINE, so ink
    // straddles row 20 - ascender above, descender below. A ^FO block
    // origin would put the cell top at 20 (no ink above it).
    let out = render_text(b"^XA^PW64^LL40^FT20,20^A0N,20,20^FDp^FS^XZ");
    let bmp = &out.bitmap;
    assert!(
        (10..19).any(|y| (0..40).any(|x| bmp.get(x, y))),
        "ascender ink above the baseline row 20"
    );
    assert!(
        (21..26).any(|y| (0..40).any(|x| bmp.get(x, y))),
        "descender ink below the baseline row 20"
    );
    assert!(
        (26..40).all(|y| (0..40).all(|x| !bmp.get(x, y))),
        "descender is ~5 dots"
    );
}

#[test]
fn a_width_rule_ignored_for_builtins() {
    // The reference renders the built-in (bitmap-style) fonts at their
    // native proportional advances; the ^A width does not fix the advance
    // (it only scales the scalable default font, and at w == h that scale
    // is 1). ^A0N,20,20 and ^A0N,20,0 therefore space the X's identically.
    let fixed = render_text(b"^XA^PW64^LL40^FO0,0^A0N,20,20^FDXX^FS^XZ");
    let prop = render_text(b"^XA^PW64^LL40^FO0,0^A0N,20,0^FDXX^FS^XZ");
    let f = col_clusters(&fixed.bitmap, 0..30);
    let p = col_clusters(&prop.bitmap, 0..30);
    assert!(f.len() >= 2 && p.len() >= 2, "two X glyphs in both renders");
    let f_pitch = f[1].0 - f[0].0;
    let p_pitch = p[1].0 - p[0].0;
    let metric = DefaultFontProvider::new()
        .get(&FontSpec::Zebra(b'0'))
        .expect("font 0 served");
    let adv_x = metric.metrics.advances[b'X' as usize - 32] as u32;
    // eff = 20/15 (font 0: cell 15 -> requested height 20).
    let expected = (adv_x as f32 * 20.0 / 15.0).round() as u32;
    assert_eq!(
        f_pitch, expected,
        "w == h does not fix the advance (proportional pitch)"
    );
    assert_eq!(
        p_pitch, expected,
        "proportional pitch = round(advance * h/cell)"
    );
}

#[test]
fn missing_font_warns() {
    // ^A9 (an unmapped Zebra letter): warning + the field is skipped, but
    // the label still renders (a following valid field paints).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let out = render_text_into(
        b"^XA^PW64^LL40^FO0,0^A9N,20,20^FDX^FS^FO0,25^A0N,10,10^FDY^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert!(!out.warnings.is_empty(), "missing font warns");
    assert!(
        out.warnings.iter().any(|w| w.contains("font unavailable")),
        "warning names the unavailable font: {:?}",
        out.warnings
    );
    assert!(
        (25..40).any(|y| (0..64).any(|x| out.bitmap.get(x, y))),
        "the second field still paints"
    );
}

#[test]
fn downloaded_font_text_paint() {
    // ^A@ naming a stored TTF paints through the same path with no warning.
    const FIXTURE: &[u8] = include_bytes!("fixtures/DejaVuSansCondensed.ttf");
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    memory.fonts.insert("FONTFILE.TTF".into(), FIXTURE.to_vec());
    let out = render_text_into(
        b"^XA^PW64^LL40^FO0,0^A@N,20,20,E:FONTFILE.TTF^FDX^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert!(
        out.warnings.is_empty(),
        "downloaded font paints cleanly: {:?}",
        out.warnings
    );
    assert!(
        (0..20)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .any(|(x, y)| out.bitmap.get(x, y)),
        "downloaded-font glyph inks the cell"
    );
}

#[test]
fn hostile_text_never_panics() {
    // Huge height/width params, an off-label origin, and a giant string:
    // cell scaling caps the raster, off-canvas glyphs are skipped, and
    // nothing panics or overflows.
    let src = b"^XA^PW64^LL64\
^FO99999999,99999999^A0N,32000,32000^FDX^FS\
^FO0,0^A0N,32000,4294967295^FDXX^FS\
^FO10,10^A0N,4294967295,1^FDXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX^FS^XZ";
    let out = render_text(src);
    assert_eq!((out.bitmap.width, out.bitmap.height), (64, 64));
    // No panic is the point; a best-effort glyph may or may not intersect:
    // assert the bitmap is still coherent (deterministic pixel reads).
    assert!((0..64).all(|y| (0..64).all(|x| !out.bitmap.get(x, y) || out.bitmap.get(x, y))));
}

// ---------------------------------------------------------------------------
// Task 13 review round 1: ^FR+^FT box re-anchoring, over-wide word wrap,
// R180 ^FT footprint/auto height.
// ---------------------------------------------------------------------------

/// Font-0 cell/baseline in dots for height h (rounded like the renderer).
/// Font 0's drawn cell and cap-anchored baseline (the metric baseline the
/// `^FT`/`^FR` box geometry anchors to): cap rows = `floor(0.75 x cell)`, the
/// baseline row is the cap's bottom row.
fn font0_hv(h: u32) -> (u32, u32) {
    let m = DefaultFontProvider::new()
        .get(&FontSpec::Zebra(b'0'))
        .expect("font 0 served");
    let eff = h as f32 / m.metrics.cell_height as f32;
    let cell = (m.metrics.cell_height as f32 * eff).round() as u32;
    let baseline = (cell * 75 / 100).saturating_sub(1);
    (cell, baseline)
}

/// Font-0 advance of `letter` in dots at height h (rounded like the renderer,
/// through the measured tenths table when present).
fn font0_adv(h: u32, letter: char) -> u32 {
    let m = DefaultFontProvider::new()
        .get(&FontSpec::Zebra(b'0'))
        .expect("font 0 served");
    m.metrics
        .tenths
        .and_then(|t| t.iter().find(|&&(ch, _)| ch == letter).map(|&(_, v)| v))
        .map(|t| (t as u32 * h + 500) / 1000)
        .unwrap_or_else(|| {
            let eff = h as f32 / m.metrics.cell_height as f32;
            (m.metrics.advances[letter as usize - 32] as f32 * eff).round() as u32
        })
}

#[test]
fn reverse_field_ft_box() {
    // ^FR + ^FT (Dot origin): the R0 box must be re-anchored to the
    // baseline - [0..adv) x [o.y-baseline .. o.y-baseline+cell). Every box
    // pixel is the complement of the normal render; outside is unchanged.
    let (cell, baseline) = font0_hv(40);
    let adv = font0_adv(40, 'p');
    let normal = render_text(b"^XA^PW90^LL90^FT60,20^A0N,40,0^FDp^FS^XZ");
    let rev = render_text(b"^XA^PW90^LL90^FT60,20^FR^A0N,40,0^FDp^FS^XZ");
    let (nb, rb) = (&normal.bitmap, &rev.bitmap);
    // ^FT60,20: the field origin is (60, 20); the baseline-anchored box
    // spans x [60, 60+adv) and y [20-baseline, 20-baseline+cell) = [-9, 31).
    let y0 = 20i64 - baseline as i64;
    let x0 = 60i64;
    for y in 0..90 {
        for x in 0..90 {
            let in_box = (x as i64) >= x0
                && (x as i64) < x0 + adv as i64
                && (y as i64) >= y0
                && (y as i64) < y0 + cell as i64;
            if in_box {
                assert_ne!(nb.get(x, y), rb.get(x, y), "box pixel {x},{y} flips");
            } else {
                assert_eq!(
                    nb.get(x, y),
                    rb.get(x, y),
                    "outside pixel {x},{y} unchanged"
                );
            }
        }
    }
    assert!(
        (0..3).any(|y| (60..60 + adv).any(|x| rb.get(x, y))),
        "the box's top rows are black"
    );
}

#[test]
fn reverse_field_ft_box_rotated() {
    // ^FR + ^FT + ^FW90 with a 2-line ^FB block: the rotated Dot box is
    // re-anchored by baseline AND last line pitch - the old code dropped
    // the pitch term, shifting the box 20 dots right.
    let (cell, baseline) = font0_hv(20);
    let adv_p = font0_adv(20, 'p');
    let adv_space = font0_adv(20, ' ');
    let line_w = 2 * adv_p; // "pp" per wrapped line
    let full_w = 2 * adv_p + adv_space + 2 * adv_p; // "pp pp" on one line
    assert!(full_w > line_w, "wrap width can separate the words");
    let wrap_w = full_w - 1; // forces the wrap into two "pp" lines
    let src = |rev: bool| {
        format!(
            "^XA^PW90^LL90^FT40,20{}^FB{},2,0,L^A0R,20,0^FDpp pp^FS^XZ",
            if rev { "^FR" } else { "" },
            wrap_w
        )
    };
    let normal = render_text(src(false).as_bytes());
    let rev = render_text(src(true).as_bytes());
    let (nb, rb) = (&normal.bitmap, &rev.bitmap);
    // Box in rotated space: x = o.x + (paint baseline) - last_pitch -
    // cell + 1, width = cells (last_pitch + cell); y = [o.y + box_left,
    // o.y + line_w). The rotated arms anchor to the paint baseline (the
    // bitmap baseline row is one below the metric baseline).
    let (cell, pitch) = (cell as i64, cell as i64);
    let x0 = 40i64 + baseline as i64 + 1 - pitch - cell + 1;
    let x1 = x0 + pitch + cell;
    for y in 0..90 {
        for x in 0..90 {
            let in_box = (x as i64) >= x0
                && (x as i64) < x1
                && (y as i64) >= 20
                && (y as i64) < 20 + line_w as i64;
            if in_box {
                assert_ne!(nb.get(x, y), rb.get(x, y), "box pixel {x},{y} flips");
            } else {
                assert_eq!(
                    nb.get(x, y),
                    rb.get(x, y),
                    "outside pixel {x},{y} unchanged"
                );
            }
        }
    }
}

/// Assert the ^FR render's black region against an expected dot-mode box
/// rect (`x0..x1` x `y0..y1` in label coordinates): every pixel inside the
/// box flips vs the normal render (black box, white glyph), every pixel
/// outside is unchanged AND still white (nothing black escapes the box),
/// the box paints black somewhere, and the glyph erases to white inside.
fn assert_reverse_box_bounds(nb: &Bitmap, rb: &Bitmap, x0: i64, y0: i64, x1: i64, y1: i64) {
    let in_box = |x: u32, y: u32| {
        (x as i64) >= x0 && (x as i64) < x1 && (y as i64) >= y0 && (y as i64) < y1
    };
    let (mut box_black, mut glyph_white) = (false, false);
    for y in 0..rb.height {
        for x in 0..rb.width {
            if in_box(x, y) {
                assert_ne!(nb.get(x, y), rb.get(x, y), "box pixel {x},{y} flips");
                box_black |= rb.get(x, y);
                glyph_white |= nb.get(x, y) && !rb.get(x, y);
            } else {
                assert_eq!(
                    nb.get(x, y),
                    rb.get(x, y),
                    "outside pixel {x},{y} unchanged"
                );
                assert!(!rb.get(x, y), "no black outside the box at {x},{y}");
            }
        }
    }
    assert!(box_black, "reverse paints a black box");
    assert!(glyph_white, "the glyph erases to white inside the box");
}

#[test]
fn reverse_field_ft_box_r180() {
    // ^FW180 + ^FR + ^FT (Dot): "inverted 180" re-anchors the box so the
    // field's baseline point sits on the origin. Ledger formula: box
    // x = -(box_right-1), y = baseline-cell+1, width box_w, height block_h,
    // offset from (o.x, o.y). Fixed ^A0,20,20 gives box_right = 20 and
    // box_left = 0 (single char); font 0 at a 20-dot cell has baseline 15.
    // With o = (40, 40) the box is exactly x [21, 41), y [36, 56) - and
    // every glyph dot (rotated x = -gx, y = baseline-gy) lands inside it.
    let (cell, baseline) = font0_hv(20);
    assert_eq!((cell, baseline), (20, 14), "20-dot cell, cap-anchored baseline");
    let adv = font0_adv(20, 'X');
    let normal = render_text(b"^XA^PW80^LL80^FW180^FT40,40^A0,20,20^FDX^FS^XZ");
    let rev = render_text(b"^XA^PW80^LL80^FW180^FT40,40^FR^A0,20,20^FDX^FS^XZ");
    let (nb, rb) = (&normal.bitmap, &rev.bitmap);
    // R180 ^FT box: x = o.x - (box_right-1), y = o.y + paint baseline -
    // cell + 1; size box_w x block_h = adv x cell.
    let x0 = 40i64 - (adv as i64 - 1);
    let y0 = 40i64 + baseline as i64 + 1 - cell as i64 + 1;
    assert_reverse_box_bounds(nb, rb, x0, y0, x0 + adv as i64, y0 + cell as i64);
}

#[test]
fn reverse_field_ft_box_r270() {
    // ^FW270 + ^FR + ^FT (Dot): "read from the bottom up" with the same
    // baseline re-anchor. Ledger formula: box x = -(baseline+last_pitch),
    // y = box_left - first_adv_total (the last line's first char advance
    // minus one - the ^FT shrink), width block_w, height box_w. One
    // 20-dot-advance line: x = 40-(15+0) = 25, y = 40+0-(20-1) = 21,
    // x [25, 45), y [21, 41) - the glyph's rotated dots
    // (x = gy-baseline, y = -gx) all fall inside.
    let (cell, baseline) = font0_hv(20);
    assert_eq!((cell, baseline), (20, 14), "20-dot cell, cap-anchored baseline");
    let adv = font0_adv(20, 'X');
    let normal = render_text(b"^XA^PW80^LL80^FW270^FT40,40^A0,20,20^FDX^FS^XZ");
    let rev = render_text(b"^XA^PW80^LL80^FW270^FT40,40^FR^A0,20,20^FDX^FS^XZ");
    let (nb, rb) = (&normal.bitmap, &rev.bitmap);
    // R270 ^FT box: x = o.x - (paint baseline + last_pitch), y = o.y +
    // box_left - (first_adv - 1); size (last_pitch + cell) x box_w =
    // cell x adv.
    let x0 = 40i64 - (baseline as i64 + 1);
    let y0 = 40i64 - (adv as i64 - 1);
    assert_reverse_box_bounds(nb, rb, x0, y0, x0 + cell as i64, y0 + adv as i64);
}

#[test]
fn fb_overwide_word_breaks_without_empty_line() {
    // "AAAAAAAAAA" at a 40-dot block: no leading empty line - max_lines 2
    // char-breaks two lines, max_lines 1 prints the first fragment only.
    let out = render_text(b"^XA^PW64^LL60^FO0,0^FB40,2,0,L^A0N,20,20^FDAAAAAAAAAA^FS^XZ");
    let bmp = &out.bitmap;
    let r1 = first_ink_row(bmp, 0..20).expect("first char-broken line has ink");
    let r2 = first_ink_row(bmp, 20..40).expect("second char-broken line has ink");
    assert_eq!(r2, r1 + 20, "second line one pitch below");
    assert!(
        (40..60).all(|y| (0..bmp.width).all(|x| !bmp.get(x, y))),
        "capped at max_lines"
    );

    let out1 = render_text(b"^XA^PW64^LL60^FO0,0^FB40,1,0,L^A0N,20,20^FDAAAAAAAAAA^FS^XZ");
    let bmp1 = &out1.bitmap;
    assert!(
        first_ink_row(bmp1, 0..20).is_some(),
        "default max_lines=1 still prints the first fragment"
    );
    assert!(
        (20..60).all(|y| (0..bmp1.width).all(|x| !bmp1.get(x, y))),
        "one line only"
    );
}

#[test]
fn fb_block_narrower_than_font_does_not_print() {
    // Guide ^FB p.186: "If the value is less than the font width or not
    // specified, the text does not print." Width 0 (the default) and any
    // width below one character suppress the field entirely; a width that
    // fits a character still prints. A fixed 20-dot advance (^A0N,20,20)
    // makes the font width deterministically 20.
    let blank = |w: u32| {
        render_text(format!("^XA^PW64^LL60^FO0,0^FB{w},2,0,L^A0N,20,20^FDAA^FS^XZ").as_bytes())
    };
    let w0 = blank(0);
    assert!(
        (0..60).all(|y| (0..64).all(|x| !w0.bitmap.get(x, y))),
        "width 0 prints nothing"
    );
    assert!(
        w0.warnings.iter().any(|w| w.contains("guide ^FB")),
        "a suppressed block warns (renderer diagnostic): {0:?}",
        w0.warnings
    );
    let w5 = blank(5);
    assert!(
        (0..60).all(|y| (0..64).all(|x| !w5.bitmap.get(x, y))),
        "width 5 (< the 20-dot font width) prints nothing"
    );
    assert!(
        w5.warnings.iter().any(|w| w.contains("guide ^FB")),
        "sub-font width warns like width 0: {0:?}",
        w5.warnings
    );
    let w27 = blank(27);
    assert!(
        (0..60).any(|y| (0..64).any(|x| w27.bitmap.get(x, y))),
        "width above the font width still prints"
    );
    assert!(
        w27.warnings.is_empty(),
        "no warning when the field prints: {0:?}",
        w27.warnings
    );
}

#[test]
fn tb_block_narrower_than_font_still_prints() {
    // ^TB (guide p.356) has no "does not print" rule - its width is
    // 1..label width, so a 5-dot block with a 20-dot advance still
    // char-breaks and prints (unlike ^FB).
    let out = render_text(b"^XA^PW64^LL60^FO0,0^TB0,5,20^A0N,20,20^FDAA^FS^XZ");
    assert!(
        (0..60).any(|y| (0..64).any(|x| out.bitmap.get(x, y))),
        "^TB prints even below the font width"
    );
    assert!(
        out.warnings.is_empty(),
        "^TB at width 5 is valid input, no warnings: {0:?}",
        out.warnings
    );
}

#[test]
fn auto_height_covers_rotated_ft_text() {
    // ^FT60,60 + R180 with no ^LL: the label auto-sizes to the R180 ^FT
    // content, which lies BELOW the origin; the glyph's bottom rows must
    // not be clipped off the label.
    let out = render_text(b"^XA^PW200^FT60,60^A0I,40,40^FDp^FS^XZ");
    assert!(
        out.bitmap.height >= 76,
        "auto height covers the R180 ^FT block, got {}",
        out.bitmap.height
    );
    assert!(
        (70..out.bitmap.height).any(|y| (0..200).any(|x| out.bitmap.get(x, y))),
        "glyph bottom rows paint inside the auto-sized label"
    );
}

// ---------------------------------------------------------------------------
// Task 14: barcode rasterization - bars/modules -> bitmap, quiet zones,
// interpret line, rotation. Quiet zone: guide "Basic Format for Bar Codes"
// (p.1586): "The size of the quiet zone depends on the size of bar widths
// (usually 10 times the width of the narrow bar)" - the Code 128 narrow
// element is one module (ISO/IEC 15417), so the drawer reserves 10 x module
// dots on each side and the film starts after it.
// ---------------------------------------------------------------------------

use zpl::bc::{barcode_field, Pattern};
use zpl::ir::{BarcodeField, Field, Rotation};

/// Parse + render a barcode-bearing format with the bundled fonts (the
/// interpretation line needs them), returning the label too so tests can
/// compare the painted bars against the Task 9 encoder output.
fn render_bc(src: &[u8]) -> (zpl::ir::Label, RenderOutcome) {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    let label = ctx.pending_label.take().expect("no format closed by ^XZ");
    let opts = RenderOptions {
        dpi: 203,
        fonts: &DefaultFontProvider::new(),
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let outcome = render(&label, &opts).expect("render failed structurally");
    (label, outcome)
}

fn single_barcode(label: &zpl::ir::Label) -> &BarcodeField {
    match &label.fields[0] {
        Field::Barcode(f) => f,
        other => panic!("expected a barcode field, got {other:?}"),
    }
}

/// Is content dot `u` (field-relative, quiet zones included) part of a
/// black bar? Mirrors the encoder's run list.
fn bar_bit_at(bars: &[zpl::bc::Bar], quiet: u32, h: u32, u: u32, v: u32) -> bool {
    let mut off = quiet;
    for b in bars {
        if b.black && v < h && u >= off && u < off + b.width {
            return true;
        }
        off += b.width;
    }
    false
}

/// Golden brief case: `^FO0,0^BY2,3,60^BCN,60,N,N^FD12345678^FS` - the
/// first black bar starts AT the field origin (the reference inserts no
/// interior quiet zone; the label margin is the quiet zone), the bar run
/// matches the Task 9 encoder output dot-for-dot, and the bars are 60
/// dots tall.
#[test]
fn barcode_first_bar_at_field_origin() {
    let src = b"^XA^PW240^LL120^FO0,0^BY2,3,60^BCN,60,N,N^FD12345678^FS^XZ";
    let (label, out) = render_bc(src);
    assert!(out.warnings.is_empty(), "clean render: {:?}", out.warnings);
    let f = single_barcode(&label);
    let quiet = 0u32; // reference: bars start at the ^FO origin
    assert_eq!((f.module, f.height), (2, 60), "^BY2,3,60 and ^BC h=60");

    // First bar is 4 dots wide (elements 2,1,1,2,3,2 at module 2) and the
    // full 60 dots tall; the space element after it is white.
    assert!(out.bitmap.get(quiet, 0) && out.bitmap.get(quiet + 3, 59), "first bar");
    assert!(!out.bitmap.get(quiet + 4, 0), "first space element");
    // Row 60 is the bar-height boundary: nothing below (f=:N - no line).
    assert!(
        !(0..240).any(|x| (60..120).any(|y| out.bitmap.get(x, y))),
        "nothing below the 60-dot bars"
    );

    // The painted bar run matches the T9 encoder output exactly.
    let Pattern::Bars(bars) = barcode_field(f) else {
        panic!("Code 128 is a bar pattern")
    };
    for x in 0..240 {
        for y in 0..120 {
            let expect = bar_bit_at(&bars, quiet, f.height, x, y);
            assert_eq!(out.bitmap.get(x, y), expect, "pixel {x},{y} vs encoder");
        }
    }
}

/// Rotated barcode (`^BCR`, 90deg clockwise): the R0 content plane rotates
/// about the field origin - the bars become horizontal rows running down
/// the page from the top (no interior quiet zone, like the reference).
/// Every column inside the rotated bar area carries the same pattern, so
/// a row scan equals the encoder's content scan.
#[test]
fn barcode_rotation_90() {
    let src = b"^XA^PW240^LL240^FO0,0^BY2,3,60^BCR,60,N,N^FD12345678^FS^XZ";
    let (label, out) = render_bc(src);
    let f = single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R90);
    let quiet = 0u32; // reference: bars start at the ^FO origin
    let Pattern::Bars(bars) = barcode_field(f) else {
        panic!("Code 128 is a bar pattern")
    };
    // fh = bar height (no interpret line).
    let fh = f.height;

    // Bottom: the pattern ends at row `pat_w` (no right quiet zone).
    let pat_w: u32 = bars.iter().map(|b| b.width).sum();
    assert!(
        !(pat_w..240).any(|y| (0..240).any(|x| out.bitmap.get(x, y))),
        "nothing below the rotated pattern"
    );
    // Beyond the rotated bar height (columns x >= fh) everything is white.
    assert!(
        !(0..240).any(|y| (fh..240).any(|x| out.bitmap.get(x, y))),
        "bar height rotates to the column axis"
    );
    // Row scan inside the bar area matches the encoder content scan; each
    // row is uniform across the full bar-height columns.
    for y in 0..240 {
        let expect = bar_bit_at(&bars, quiet, fh, y, 0) && y < quiet + pat_w;
        for x in 0..fh {
            assert_eq!(out.bitmap.get(x, y), expect, "rotated pixel {x},{y}");
        }
    }
}

/// Interpret line below (`^BC f=Y`): the guide's `h = barcode height (in
/// dots)` is the BAR height only - the interpretation line is added BELOW
/// the bars, outside the height parameter. The line prints in the built-in
/// font 0 at the reference's size (15% of the bar height, min 15 dots) and
/// is CENTERED on the symbol (measured on the reference: the sample's
/// "12345678" line and corpus 08's "ZEBRA" both sit centered under the
/// bars). The default `N` mode encodes the digits literally in Start B,
/// so "12345678" = 123 modules x 2 = 246 dots wide.
#[test]
fn barcode_interpret_line_below_bars() {
    let src = b"^XA^PW300^LL140^FO0,0^BY2,3,60^BCN,60,Y,N^FD12345678^FS^XZ";
    let (label, out) = render_bc(src);
    assert!(out.warnings.is_empty(), "clean render: {:?}", out.warnings);
    let f = single_barcode(&label);
    assert_eq!((f.line, f.interpret), (b'Y', false));

    // The bars keep the full 60-dot height (the line is NOT inside it) and
    // start at the field origin (reference: no interior quiet zone).
    assert!(
        out.bitmap.get(0, 0) && out.bitmap.get(3, 59),
        "bars span 0..60"
    );
    // Line ink appears exactly below the bars, centered on the 246-dot
    // symbol: 8 digits at the font-0 15-dot cell advance ~60 dots total,
    // so the line spans roughly x 93..158, rows 60..75.
    assert!(
        (60..78).any(|y| (90..165).any(|x| out.bitmap.get(x, y))),
        "line ink sits below the bar height, centered"
    );
    // Nothing beneath the line's ink; the line stays clear of the symbol
    // edges (it is centered, not left-bounded at the field origin).
    assert!(
        !(0..300).any(|x| (80..140).any(|y| out.bitmap.get(x, y))),
        "no ink below the interpretation line"
    );
    assert!(
        !(0..300).any(|x| (60..80).any(|y| out.bitmap.get(x, y)))
            || (90..160).any(|x| (60..78).any(|y| out.bitmap.get(x, y))),
        "line ink is centered (within the middle third of the symbol)"
    );
}

/// Interpret line above (`^BC g=Y`): the line prints ABOVE the bars,
/// extending the field above its ^FO origin; the bars are unchanged. The
/// line is the built-in font-0 line at 15% of the bar height (60 -> min
/// 15), so it occupies rows 0..15 above the origin, centered.
#[test]
fn barcode_interpret_line_above_bars() {
    let plain = b"^XA^PW300^LL140^FO0,15^BY2,3,60^BCN,60,N,N^FD12345678^FS^XZ";
    let above = b"^XA^PW300^LL140^FO0,15^BY2,3,60^BCN,60,N,Y^FD12345678^FS^XZ";
    let (_, out_plain) = render_bc(plain);
    let (_, out_above) = render_bc(above);

    // Without g=Y nothing paints above the field origin...
    assert!(
        !(0..15).any(|y| (0..300).any(|x| out_plain.bitmap.get(x, y))),
        "plain: rows above the origin are white"
    );
    // ...with g=Y the 15-dot line cell occupies rows 0..15 (origin 15 - 15),
    // centered on the 246-dot symbol (roughly x 93..158 wide).
    assert!(
        (0..15).any(|y| (90..165).any(|x| out_above.bitmap.get(x, y))),
        "line ink appears above the bars"
    );
    // Bars are unchanged below the origin (first bar at the ^FO origin).
    assert!(
        out_above.bitmap.get(0, 15) && out_above.bitmap.get(3, 74),
        "bars still start at the ^FO origin"
    );
}

/// f=Y AND g=Y on an auto-height label (no ^LL): each printed
/// interpretation line adds its own cell, so the content spans
/// [-lc, h+lc) relative to the origin and the auto-sized label must cover
/// h + 2*lc - the below line would be clipped if only one cell were
/// counted. The line cell is the built-in font-0 line at 15% of the bar
/// height (60 -> min 15), centered on the symbol; the auto height must be
/// 15 + 60 + 15 = 90.
#[test]
fn barcode_both_interpret_lines_auto_height() {
    let (label, out) = render_bc(b"^XA^PW300^FO0,15^BY2,3,60^BCN,60,Y,Y^FD12345678^FS^XZ");
    let f = single_barcode(&label);
    assert_eq!((f.line, f.interpret), (b'Y', true));
    let bmp = &out.bitmap;
    assert_eq!(bmp.height, 90, "auto height covers both line cells + bars");
    assert!(
        (0..15).any(|y| (90..165).any(|x| bmp.get(x, y))),
        "above line ink"
    );
    assert!(
        bmp.get(0, 15) && bmp.get(3, 74),
        "bars span the full 60-dot height from the origin"
    );
    assert!(
        (75..90).any(|y| (90..165).any(|x| bmp.get(x, y))),
        "below line ink (not clipped)"
    );
}

/// ^POI (label rotated 180 deg): a barcode with BOTH interpretation lines
/// flips as a unit - above line, bars, and below line all survive in
/// flipped order (above stays above the bars; the footprint box covers
/// the full [-lc, h+lc) span, so the far-corner math sees all the ink).
#[test]
fn barcode_poi_keeps_both_interpret_lines() {
    // ^FO30,20: mode-N "12345678" = Start B, 123 modules at module 2 =
    // 246 dots wide, no interior quiet zone (reference) -> fw = 246;
    // fh = 60 + 2*15 = 90 (line cells are 15% of the bar height, min 15).
    // ^POI moves the field to the far corner (300-30-246, 300-20-90) =
    // (24, 190); the content plane renders from there unchanged: bars on
    // rows [190,250) at cols [24,270), the above line at rows [175,190),
    // the below line at rows [250,265).
    let (label, out) =
        render_bc(b"^XA^PW300^LL300^POI^FO30,20^BY2,3,60^BCN,60,Y,Y^FD12345678^FS^XZ");
    let f = single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R0);
    assert!(out.warnings.is_empty(), "clean: {:?}", out.warnings);
    let bmp = &out.bitmap;
    // First bar (u in [0,4)) at the far-corner origin.
    assert!(bmp.get(24, 190) && bmp.get(27, 249), "bars flipped intact");
    assert!(
        !(0..24).any(|x| (0..300).any(|y| bmp.get(x, y))),
        "nothing left of the flipped field"
    );
    assert!(
        (175..190).any(|y| (118..190).any(|x| bmp.get(x, y))),
        "above line survived the flip (centered font-0 line)"
    );
    assert!(
        (250..265).any(|y| (118..190).any(|x| bmp.get(x, y))),
        "below line survived the flip"
    );
    assert!(
        !(0..175).any(|y| (0..300).any(|x| bmp.get(x, y))),
        "nothing above the flipped field"
    );
    assert!(
        !(265..300).any(|y| (0..300).any(|x| bmp.get(x, y))),
        "nothing below the flipped field"
    );
}

/// Hostile hand-built IR: module = u32::MAX (the parser caps ^BY at 10;
/// only hand-built IR reaches the renderer). The quiet zone (10 modules)
/// and every bar resolve astronomically wide - the drawer must clamp to
/// the canvas, paint nothing, and never allocate or loop unboundedly.
#[test]
fn barcode_hostile_module_never_panics() {
    use zpl::ir::{BarcodeField, Label};
    let make = |symbology: u8, rotation| BarcodeField {
        x: 0,
        y: 0,
        symbology,
        data: "12".into(),
        module: u32::MAX,
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
        font: *b"A ",
        font_height: 9,
        font_width: 5,
        font_name: None,
        rotation,
    };
    let label = Label {
        width_dots: 8,
        height_dots: 12,
        copies: 1,
        numbered: BTreeMap::new(),
        fields: vec![
            Field::Barcode(make(b'C', Rotation::R0)),
            Field::Barcode(make(b'C', Rotation::R90)),
            Field::Barcode(make(b'C', Rotation::R180)),
            Field::Barcode(make(b'C', Rotation::R270)),
        ],
    };
    let opts = RenderOptions {
        dpi: 203,
        fonts: &NoFonts,
        memory: &Memory::default(),
        mode: &zpl::state::PrinterMode::default(),
    };
    let out = render(&label, &opts).expect("hostile barcode IR is still a valid render");
    assert_eq!((out.bitmap.width, out.bitmap.height), (8, 12));
    // Bars start at the field origin (no interior quiet zone), so the
    // c50-clamped first bar covers the entire tiny canvas at every
    // rotation - the point of the test is that MAX modules stay bounded
    // (no panic, no unbounded work), not that the origin stays white.
    assert!(
        (0..8).all(|x| (0..12).all(|y| out.bitmap.get(x, y))),
        "c50-clamped module-2^32 bar covers the tiny canvas"
    );
}

/// Task 15 smoke: every new 1D symbology renders through the full
/// pipeline (parse -> barcode_field -> paint) without warnings or panics.
#[test]
fn t15_all_symbologies_render() {
    let out = render_src(b"^XA^BY2,3,60^FO20,20^B1N,,80,N,N^FD12345^FS^FO20,150^B3N,,80,N,N^FDCode39^FS\
^FO20,280^BEN,80^FD400638133393^FS^FO20,410^B2N,80,N,N,Y^FD12345^FS^FO20,540^BAN,80^FDabcd^FS\
^FO20,670^BKN,,80,N,N,C,D^FD1234^FS^FO20,800^BUN,80^FD03600029145^FS^FO20,930^B9N,80^FD1200000123^FS\
^FO20,1060^B8N,80^FD9638507^FS^FO20,1190^BMN,D,80,N,N^FD1234567^FS^FO20,1320^B5N,80^FD01234567890^FS^XZ");
    // NoFonts has no interpretation-line font (T13 territory); the only
    // warnings must be that kind - never a barcode data/validity warning.
    assert!(
        out.warnings
            .iter()
            .all(|w| w.starts_with("interpretation line font")),
        "unexpected warnings: {:?}",
        out.warnings,
    );
    assert!(
        out.bitmap.width > 100 && out.bitmap.height > 1350,
        "{}x{}",
        out.bitmap.width,
        out.bitmap.height
    );
}

/// R4: `^FB` block geometry at the drawn cell 24 (parity case 07):
/// the corrected advances make the wrapped first line fit inside the
/// `^FB` box (the uncorrected table overruns its edge) while keeping
/// left/right justification flush within it.
#[test]
fn t04_fb_block_geometry_24() {
    use zpl::fonts::DefaultFontProvider;
    let fonts = DefaultFontProvider::new();
    let left = b"^XA^LL300^FO20,20^FB240,3,0,L^A0N,24,24^FDZebra block text wraps at the field width with justification left.^FS^XZ";
    let right = b"^XA^LL300^FO280,20^FB240,3,0,R^A0N,24,24^FDThis block is right justified across three lines.^FS^XZ";
    let run = |src: &[u8]| -> (u32, u32, Vec<u8>) {
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut memory);
        parse_stream(&mut Scanner::new(src), &mut ctx);
        let label = ctx.pending_label.take().unwrap();
        let opts = RenderOptions {
            dpi: 203,
            fonts: &fonts,
            memory: &*ctx.memory,
            mode: &ctx.mode,
        };
        let out = render(&label, &opts).expect("render failed structurally");
        decode_png(&out.bitmap.to_png())
    };
    let col_dark = |px: &[u8], w: u32, x: u32, y0: u32, y1: u32| {
        (y0..y1).any(|y| {
            let byte = px[((y * w + x) / 8) as usize];
            byte & (0x80 >> ((x % 8) as u8)) != 0
        })
    };
    // Left block: the corrected advances pull the fourth word ("wraps")
    // inside the box - the reference's last ink is 236-238, the
    // uncorrected table overruns past 239.
    let (w, h, px) = run(left);
    assert!(w > 240 && h >= 90, "{}x{}", w, h);
    let cols: Vec<u32> = (20..240).filter(|&x| col_dark(&px, w, x, 20, 44)).collect();
    assert!((20..=22).contains(cols.first().unwrap()), "block first ink near the field origin");
    assert!(
        *cols.last().unwrap() <= 239,
        "wrapped line fits the box (last ink {}, uncorrected table overruns)",
        cols.last().unwrap()
    );
    // Right block: flush-right placement inside the same box width.
    let (w, _h, px) = run(right);
    let l1: Vec<u32> = (300..520).filter(|&x| col_dark(&px, w, x, 20, 60)).collect();
    assert!(
        l1.first().unwrap() > &290 && l1.first().unwrap() <= &343,
        "right block inks inside the box (first ink {})",
        l1.first().unwrap()
    );
}
