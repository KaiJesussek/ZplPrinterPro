//! Renderer integration tests (Task 10): framebuffer semantics, PNG output,
//! drawing primitives, graphic blits, auto height, mode transforms.

use std::io::Cursor;

use png::BitDepth;
use png::ColorType;

use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::fonts::{FontData, FontProvider, FontSpec};
use zpl::render::render;
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
    assert_eq!(info.color_type, ColorType::Grayscale, "PNG must be grayscale");
    assert_eq!(info.bit_depth, BitDepth::One, "PNG must be 1-bit");
    (info.width, info.height, buf)
}

fn sample(buf: &[u8], width: u32, x: u32, y: u32) -> u8 {
    let row = ((width as usize) + 7) / 8;
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
            assert_eq!(bmp.get(x, y), sample(&buf, w, x, y) == 0, "pixel {x},{y} pattern");
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
    assert!(!bmp.get(3, 4) && !bmp.get(3, 7), "no bleed outside the image");
    assert!(!bmp.get(0, 0) && !bmp.get(31, 19), "rest of the label is white");
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
    assert!(bmp.get(8, 8), "clipped blit still paints its on-canvas pixels");
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
    assert_eq!(outcome.warnings.len(), 1, "one missing-image warning, got {:?}", outcome.warnings);
    assert!(outcome.warnings[0].contains("LOGO.GRF"), "warning names the image: {}", outcome.warnings[0]);
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
    assert!(memory.images.contains_key("LOGO.GRF"), "download stored the image");
    let outcome = render_into(b"^XA^PW32^LL20^FO3,5^XGLOGO.GRF^FS^XZ", &mut diags, &mut memory);
    let bmp = &outcome.bitmap;
    assert!(outcome.warnings.is_empty(), "stored image must not warn: {:?}", outcome.warnings);
    assert!(bmp.get(3, 5) && bmp.get(3, 6), "stored image blits its pixels");
    assert!(!bmp.get(4, 5), "white pixels of the stored image stay white");
}

#[test]
fn lh_offset_applied_to_field_origins() {
    // ^LH5,5 + ^FO2,3 -> the box's top-left lands at (7,8), not (2,3).
    let outcome = render_src(b"^XA^PW30^LL30^LH5,5^FO2,3^GB4,4,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(7, 8), "field origin + ^LH offset");
    assert!(!bmp.get(2, 3), "un-offset origin is outside the box");
    assert!(!bmp.get(6, 8) && !bmp.get(7, 7), "top-left corner is exactly (7,8)");
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
    assert!(bmp.get(16, 16) && bmp.get(19, 19), "box painted at the rotated origin");
    assert!(!bmp.get(0, 0), "original corner is empty after the 180\u{00B0} rotation");
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
    assert!(!bmp.get(0, 0) && !bmp.get(9, 9) && !bmp.get(7, 4) && !bmp.get(6, 5), "fill stays inside the box");
}

#[test]
fn white_color_erases() {
    // Paint a black 20x20 field, then a white 10x10 box on top: the white
    // box erases (paints 0) - the guide's W color is not "nothing".
    let src = b"^XA^PW20^LL20^FO0,0^GB20,20,0,B^FS^FO5,5^GB10,10,0,W^FS^XZ";
    let outcome = render_src(src);
    let bmp = &outcome.bitmap;
    assert!(bmp.get(0, 0), "black stays where no white box overlaps");
    assert!(!bmp.get(5, 5) && !bmp.get(14, 14), "white fill erased the black");
    assert!(bmp.get(4, 4) && bmp.get(15, 15), "black remains just outside the white box");
}

#[test]
fn circle_thickness_grows_inward() {
    // ^GC6,1: a 6-dot ring, 1 dot thick, centered on a 6x6 grid whose corners
    // are empty and whose center 2x2 is the hole.
    let outcome = render_src(b"^XA^PW10^LL10^FO1,1^GC6,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    let at = |x: u32, y: u32| bmp.get(1 + x, 1 + y);
    assert!(!at(0, 0), "corner of the 6x6 circle box is empty");
    assert!(at(0, 3) && at(3, 0) && at(5, 3) && at(3, 5), "cardinal edge midpoints painted");
    assert!(!at(2, 2) && !at(3, 3) && !at(2, 3) && !at(3, 2), "1-dot ring leaves the center hole");

    // Thickness >= diameter/2 fills the whole disc (thickness grows inward).
    let outcome = render_src(b"^XA^PW10^LL10^FO1,1^GC6,3,B^FS^XZ");
    let bmp = &outcome.bitmap;
    assert!(bmp.get(4, 4), "solid disc center painted when thickness reaches the center");
}

#[test]
fn ellipse_paints_ring_with_hole() {
    // ^GE10,6,1: corners empty, top-edge midpoint painted, center hole.
    let outcome = render_src(b"^XA^PW12^LL10^FO1,1^GE10,6,1,B^FS^XZ");
    let bmp = &outcome.bitmap;
    let at = |x: u32, y: u32| bmp.get(1 + x, 1 + y);
    assert!(!at(0, 0) && !at(9, 0) && !at(0, 5) && !at(9, 5), "corners empty");
    assert!(at(4, 0) && at(5, 0), "top edge midpoint painted");
    assert!(!at(5, 2), "1-dot ellipse keeps an inner hole");
    assert!(at(0, 2) && at(0, 3), "leftmost column painted at mid height");
}

#[test]
fn diagonal_direction_pins_r_and_l() {
    // ^GD: R = right-leaning (top-left to bottom-right "/"), L = left-leaning.
    let outcome = render_src(b"^XA^PW20^LL20^FO0,0^GD5,5,1,B,R^FS^XZ");
    let bmp = &outcome.bitmap;
    for i in 0..5 {
        assert!(bmp.get(i, i), "R diagonal on the main diagonal at {i}");
    }
    assert!(!bmp.get(4, 0) && !bmp.get(0, 4), "R diagonal's anti-diagonal corners empty");

    let outcome = render_src(b"^XA^PW20^LL20^FO0,0^GD5,5,1,B,L^FS^XZ");
    let bmp = &outcome.bitmap;
    for i in 0..5 {
        assert!(bmp.get(4 - i, i), "L diagonal on the anti-diagonal at {i}");
    }
    assert!(!bmp.get(0, 0) && !bmp.get(4, 4), "L diagonal's main-diagonal corners empty");
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
    use zpl::ir::{BoxField, CircleField, DiagonalField, EllipseField, Field, GraphicField, GraphicKind, Label};
    let label = Label {
        width_dots: 4,
        height_dots: 0, // auto height over hostile fields must terminate
        copies: 1,
        fields: vec![
            Field::Box(BoxField { x: 0, y: 0, width: u32::MAX, height: u32::MAX, thickness: u32::MAX, color: b'B' }),
            Field::Circle(CircleField { x: u32::MAX, y: u32::MAX, diameter: u32::MAX, thickness: u32::MAX, color: b'B' }),
            Field::Ellipse(EllipseField { x: 2, y: 2, width: u32::MAX, height: u32::MAX, thickness: u32::MAX, color: b'W' }),
            Field::Diagonal(DiagonalField { x: 3, y: 0, width: u32::MAX, height: u32::MAX, direction: b'L', thickness: u32::MAX, color: b'B' }),
            Field::Graphic(GraphicField { x: u32::MAX - 1, y: 0, kind: GraphicKind::Inline, data: vec![0xFF], bytes_per_row: 1 }),
            // bytes_per_row * 8 overflows u32 for values >= 2^29 (the old
            // code panicked in debug); empty data passes the multiple-of
            // check (0 % N == 0) and must render as a no-op blit.
            Field::Graphic(GraphicField { x: 0, y: 0, kind: GraphicKind::Inline, data: Vec::new(), bytes_per_row: u32::MAX }),
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
        assert!(!bmp.get(x, 1) && !bmp.get(x, 2), "src row 0 went off the top; dst rows stay white");
    }

    // ^POI rotates the 16x1 image (data F00F: 0xF0 0x0F) to fx = 8-16 = -8
    // and fy = 8-0-1 = 7: destination row 7 shows source columns 8..15
    // (byte 0x0F = four white then four black); source columns 0..7
    // (0xF0) must never appear.
    let outcome = render_src(b"^XA^PW8^LL8^POI^FO0,0^GFA,2,2,2,F00F^FS^XZ");
    let bmp = &outcome.bitmap;
    for x in 0..4 {
        assert!(!bmp.get(x, 7), "src cols 8..11 (0x0F leading zeros) at dst {x},7");
    }
    for x in 4..8 {
        assert!(bmp.get(x, 7), "src cols 12..15 (0x0F trailing ones) at dst {x},7");
    }
    for y in 0..7 {
        assert!(!bmp.get(0, y) && !bmp.get(7, y), "no image pixels above the rotated row");
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
