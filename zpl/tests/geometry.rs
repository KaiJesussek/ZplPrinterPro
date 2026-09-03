//! Integration tests for the Task 8 drawing-field command set:
//! `^GB ^GC ^GD ^GE ^GF`.

mod common;

use common::parse_one;
use zpl::cmd::{parse_stream, parse_stream_collect};
use zpl::diag::{Diagnostics, Severity};
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};
use zpl::ir::{
    BoxField, CircleField, DiagonalField, EllipseField, Field, GraphicField, GraphicKind, Label,
};

fn box_field(label: &Label, i: usize) -> &BoxField {
    match &label.fields[i] {
        Field::Box(b) => b,
        other => panic!("expected box field, got {other:?}"),
    }
}

fn circle_field(label: &Label, i: usize) -> &CircleField {
    match &label.fields[i] {
        Field::Circle(c) => c,
        other => panic!("expected circle field, got {other:?}"),
    }
}

fn diagonal_field(label: &Label, i: usize) -> &DiagonalField {
    match &label.fields[i] {
        Field::Diagonal(d) => d,
        other => panic!("expected diagonal field, got {other:?}"),
    }
}

fn ellipse_field(label: &Label, i: usize) -> &EllipseField {
    match &label.fields[i] {
        Field::Ellipse(e) => e,
        other => panic!("expected ellipse field, got {other:?}"),
    }
}

fn graphic_field(label: &Label, i: usize) -> &GraphicField {
    match &label.fields[i] {
        Field::Graphic(g) => g,
        other => panic!("expected graphic field, got {other:?}"),
    }
}

#[test]
fn gb_draws_box_field() {
    // ^GB w,h,t,c: a 100x50 box with a 5-dot black border. No ^FO: origin
    // defaults to 0,0 (the draft opens at printer defaults).
    let (label, diags) = parse_one(b"^XA^GB100,50,5,B^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    let b = box_field(&label, 0);
    assert_eq!((b.x, b.y), (0, 0));
    assert_eq!((b.width, b.height), (100, 50));
    assert_eq!(b.thickness, 5);
    assert_eq!(b.color, b'B');
}

#[test]
fn gb_defaults_track_thickness() {
    // Missing w/h default to "the value used for thickness (t) or 1"
    // (guide ^GB p.210); missing t defaults to 1 and c to B.
    let (label, _) = parse_one(b"^XA^GB^FS^XZ");
    let b = box_field(&label, 0);
    assert_eq!((b.width, b.height), (1, 1));
    assert_eq!((b.thickness, b.color), (1, b'B'));

    // Bare thickness with no w/h: the guide's "solid box" - w=h=t
    // (guide ^GB: "If the width and height are not specified, you get a
    // solid box with its width and height as specified by the value t").
    let (label, diags) = parse_one(b"^XA^GB,,5^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let b = box_field(&label, 0);
    assert_eq!((b.width, b.height, b.thickness), (5, 5, 5));
}

#[test]
fn gb_thickness_zero_is_recorded_for_renderer_fill() {
    // The renderer (Task 10) treats ^GB t=0 as a filled box per the plan;
    // the parser records the value verbatim (the 11EN guide's table says
    // t in 1..32000; recording 0 satisfies both - the renderer decides).
    let (label, diags) = parse_one(b"^XA^GB40,30,0^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(box_field(&label, 0).thickness, 0);
}

#[test]
fn gb_accepts_white_color() {
    let (label, _) = parse_one(b"^XA^GB60,20,2,W^FS^XZ");
    assert_eq!(box_field(&label, 0).color, b'W');
    let (label, _) = parse_one(b"^XA^GB60,20,2,w^FS^XZ");
    assert_eq!(box_field(&label, 0).color, b'W');
}

#[test]
fn gb_corner_rounding_is_parsed_and_dropped() {
    // ^GB w,h,t,c,r - the 5th parameter r (0..8, default 0) has no IR slot;
    // it is accepted silently like ^FO's z justification.
    let (label, diags) = parse_one(b"^XA^GB100,50,5,B,4^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(
        (box_field(&label, 0).width, box_field(&label, 0).height),
        (100, 50)
    );
}

#[test]
fn gb_malformed_width_warns_and_falls_back() {
    // Malformed width -> warning + the thickness default, never a panic.
    let (label, diags) = parse_one(b"^XA^GBabc,50,5,B^FS^XZ");
    assert!(diags.items.iter().any(|d| d.severity == Severity::Warning));
    assert_eq!(label.fields.len(), 1);
    let b = box_field(&label, 0);
    assert_eq!(b.width, 5); // w defaults to t (=5) when missing/invalid
    assert_eq!(b.height, 50);
}

#[test]
fn gc_circle_parses_and_defaults() {
    let (label, diags) = parse_one(b"^XA^GC50,5,W^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let c = circle_field(&label, 0);
    assert_eq!((c.diameter, c.thickness, c.color), (50, 5, b'W'));

    // Guide defaults: d=3, t=1, c=B.
    let (label, _) = parse_one(b"^XA^GC^FS^XZ");
    let c = circle_field(&label, 0);
    assert_eq!((c.diameter, c.thickness, c.color), (3, 1, b'B'));
}

#[test]
fn gd_diagonal_parses_and_direction() {
    let (label, diags) = parse_one(b"^XA^GD100,200,10,B,L^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let d = diagonal_field(&label, 0);
    assert_eq!((d.width, d.height), (100, 200));
    assert_eq!((d.direction, d.thickness, d.color), (b'L', 10, b'B'));

    // Direction defaults to R (right-leaning); '/' is the R alias per the
    // guide's value table.
    let (label, _) = parse_one(b"^XA^GD300,100^FS^XZ");
    assert_eq!(diagonal_field(&label, 0).direction, b'R');
    let (label, _) = parse_one(b"^XA^GD200,200,2,/^FS^XZ");
    assert_eq!(diagonal_field(&label, 0).direction, b'R');

    // w/h default to "the value of t or 3" (guide ^GD p.213).
    let (label, _) = parse_one(b"^XA^GD,,5^FS^XZ");
    let d = diagonal_field(&label, 0);
    assert_eq!((d.width, d.height, d.thickness), (5, 5, 5));
}

#[test]
fn ge_ellipse_parses() {
    let (label, diags) = parse_one(b"^XA^GE80,40,2,B^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let e = ellipse_field(&label, 0);
    assert_eq!((e.width, e.height, e.thickness, e.color), (80, 40, 2, b'B'));

    let (label, _) = parse_one(b"^XA^GE^FS^XZ");
    let e = ellipse_field(&label, 0);
    assert_eq!((e.width, e.height, e.thickness, e.color), (1, 1, 1, b'B'));
}

#[test]
fn gf_inline_ascii_hex_graphic() {
    // ^GF a,b,c,d,data (guide p.215 - the plan brief's a/c reading is
    // inverted; the guide is ground truth): a=A ASCII hex, b=transmitted
    // byte count, c=image total bytes (widthxheight), d=bytes per row.
    // 8 bytes at 2 per row = 16 dots wide, 4 rows tall.
    let (label, diags) = parse_one(b"^XA^FO10,20^GFA,8,8,2,FF000000FF000000^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let g = graphic_field(&label, 0);
    assert_eq!((g.x, g.y), (10, 20));
    assert!(matches!(g.kind, GraphicKind::Inline));
    assert_eq!(g.bytes_per_row, 2);
    assert_eq!(g.data, vec![0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00]);
}

#[test]
fn gf_data_uses_download_compression() {
    // ^GF A-data "follows the format for other download commands" - repeat
    // letters and row fills decode exactly as ~DG (shared decoder).
    let (label, diags) = parse_one(b"^XA^GFA,4,4,1,M666^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let g = graphic_field(&label, 0);
    assert_eq!(g.data, vec![0x66; 4]);
    assert_eq!(g.bytes_per_row, 1);

    // A comma pads the current row with 00 ("minimizing the data sent");
    // trailing data after the count is satisfied is ignored.
    let (label, diags) = parse_one(b"^XA^GFA,4,4,2,FFFF,^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let g = graphic_field(&label, 0);
    assert_eq!(g.data, vec![0xFF, 0xFF, 0x00, 0x00]);
    assert_eq!(g.bytes_per_row, 2);
}

#[test]
fn gf_defaults_compression_to_a_and_tolerates_crlf() {
    // Missing a -> A (ASCII hex); CR/LF inside the data are allowed.
    let (label, diags) = parse_one(b"^XA^GF,2,2,1,0F0F\n^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let g = graphic_field(&label, 0);
    assert_eq!(g.data, vec![0x0F, 0x0F]);
    assert_eq!(g.bytes_per_row, 1);
}

#[test]
fn gf_missing_size_params_ignores_command() {
    // Guide: b/c/d "command is ignored if a value is not specified".
    let (label, diags) = parse_one(b"^XA^GFA,,,^FS^XZ");
    assert!(diags.items.iter().any(|d| d.command == "GF"));
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(
        label.fields.len(),
        0,
        "no field should be emitted: {label:?}"
    );
}

#[test]
fn gf_unsupported_compression_warns_and_skips() {
    // C is compressed binary: the guide defines its scheme only as
    // "compressed on the host side using Zebra's compression algorithm"
    // (undocumented; ~DY adds "used only by Zebra's BAR-ONE"), so C's
    // payload is framed correctly but dropped with a warning. Unknown
    // letters warn and skip too. B is supported (its own tests decode
    // it), so it no longer belongs in the warning list.
    for src in [
        b"^XA^GFC,4,4,2,FFFF^FS^XZ".as_slice(),
        b"^XA^GFZ,4,4,2,FFFF^FS^XZ".as_slice(),
    ] {
        let (label, diags) = parse_one(src);
        assert!(
            diags.items.iter().any(|d| d.command == "GF"),
            "{:?}: expected a ^GF diagnostic, got {diags:?}",
            String::from_utf8_lossy(src)
        );
        assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
        assert_eq!(
            label.fields.len(),
            0,
            "no field should be emitted: {label:?}"
        );
    }
}

#[test]
fn gf_malformed_data_warns_and_skips() {
    // Odd nibble count decodes short of the declared 4 bytes -> warn + skip.
    let (label, diags) = parse_one(b"^XA^GFA,4,4,2,FF^FS^XZ");
    assert!(diags.items.iter().any(|d| d.command == "GF"));
    assert_eq!(
        label.fields.len(),
        0,
        "no field should be emitted: {label:?}"
    );
}

#[test]
fn gf_total_not_multiple_of_row_warns_and_skips() {
    let (label, diags) = parse_one(b"^XA^GFA,5,5,2,FFFF00^FS^XZ");
    assert!(diags.items.iter().any(|d| d.command == "GF"));
    assert_eq!(
        label.fields.len(),
        0,
        "no field should be emitted: {label:?}"
    );
}

#[test]
fn drawing_fields_position_from_fo() {
    // Geometry commands are field commands: ^FO positions them like any
    // other field.
    let (label, diags) = parse_one(b"^XA^FO30,40^GB100,50,5,B^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let b = box_field(&label, 0);
    assert_eq!((b.x, b.y), (30, 40));
}
#[test]
fn gf_binary_payload_accepts_caret_and_tilde_bytes() {
    // ^GF B declares its payload length (b = 6): `^`, `~`, and `^FS`
    // bytes inside the payload are raw data (guide p.215: "all control
    // prefixes are ignored until the total number of bytes needed ... is
    // sent"), never commands - so zero diagnostics can fire, the bitmap
    // bytes are exact, and a command after the payload parses normally.
    let payload = [0x41u8, 0x42, 0x5E, 0x7E, 0x01, 0xFE]; // "AB^~\x01\xFE"
    let mut src = b"^XA^FO5,5^GFB,6,6,2,".to_vec();
    src.extend_from_slice(&payload);
    src.extend_from_slice(b"^FS^FO20,20^FDafter^FS^XZ");
    let (label, diags) = parse_one(&src);
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 2, "binary field plus the trailing field");
    let g = graphic_field(&label, 0);
    assert_eq!((g.x, g.y), (5, 5));
    assert!(matches!(g.kind, GraphicKind::Inline));
    assert_eq!(g.data, payload.to_vec(), "bitmap bytes are exact");
    assert_eq!(g.bytes_per_row, 2, "16 dots wide, 3 rows tall");
    match &label.fields[1] {
        Field::Text(t) => assert_eq!(t.data, "after", "command after the payload parses"),
        other => panic!("expected text field, got {other:?}"),
    }
}

#[test]
fn gf_binary_truncated_warns_and_parses_on() {
    // Declared byte count beyond the stream: one warning, no field, no
    // panic - and the input after the truncation point still parses
    // (the dangling ^FS closes nothing, ^XZ closes the empty format, and
    // a fresh ^XA starts the next label cleanly).
    let (label, diags) = parse_one(b"^XA^GFB,99,8,2,AB^FS^XZ^XA^FO10,10^FDhi^FS^XZ");
    let gf_warns = diags.items.iter().filter(|d| d.command == "GF").count();
    assert_eq!(gf_warns, 1, "exactly one truncation warning: {diags:?}");
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert!(diags.items[0].message.contains("99"), "{diags:?}");
    assert_eq!(label.fields.len(), 1, "the abandoned format left nothing");
    match &label.fields[0] {
        Field::Text(t) => assert_eq!(t.data, "hi"),
        other => panic!("expected the second format's text field, got {other:?}"),
    }
}

/// A `^GFB` format whose 55-byte payload is deliberately hostile: it
/// opens with plain bytes (which the scanner's params run absorbs),
/// then a `^` cuts the params off mid-payload, and it contains `^XA^XZ`
/// (phantom-format bait) and a fake `^FS` - bytes that must stay raw
/// under binary framing. Returns the raw format and a split point inside
/// the payload.
fn hostile_gf_src() -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0x41; 8]); // plain bytes the params absorb
    payload.extend_from_slice(&[0x5E, 0x7E, 0x01]); // "^~\x01" - params stop here
    payload.extend_from_slice(b"^XA^XZ"); // phantom-format bait
    payload.extend_from_slice(&[0x42; 18]); // incl. a fake ^FS inside
    payload.extend_from_slice(b"^FS");
    payload.extend_from_slice(&[0x43; 17]);
    assert_eq!(payload.len(), 55);
    // 55 = 11 rows x 5 bytes per row ^GF B framing: b = c = 55, d = 5.
    let mut src = b"^XA^FO5,5^GFB,55,55,5,".to_vec();
    src.extend_from_slice(&payload);
    src.extend_from_slice(b"^FS^XZ");
    // Split inside the payload, AFTER the ^ that cut the params run off
    // (16 header-relative payload bytes: 8 plain + "^~\x01" + 5 more of
    // the ^XA^XZ bait), so chunk 1 stashes a half-consumed download.
    (src, 24 + 16)
}

#[test]
fn gf_binary_payload_split_across_parse_calls_decodes_fully() {
    // The TCP session reads ~8 KiB per chunk, so large binary graphics
    // routinely straddle the boundary: chunk 1 stashes the interrupted
    // download into ctx.pending_binary, and the next parse call's loop
    // keeps raw-consuming the payload (the ^/~ bait must never tokenize
    // into phantom commands). The two-chunk decode must equal the
    // single-chunk one byte-for-byte, and the raw span must re-parse to
    // the same label.
    let (src, mid) = hostile_gf_src();

    // Single-chunk reference.
    let (mut diags_ref, mut mem_ref) = (Diagnostics::default(), Memory::default());
    let mut ref_ctx = ParseContext::new(&mut diags_ref, &mut mem_ref);
    let mut ref_labels = Vec::new();
    parse_stream_collect(&mut Scanner::new(&src), &mut ref_ctx, &mut ref_labels);
    assert_eq!(ref_labels.len(), 1, "single-chunk parse must close one label");
    let ref_data = match &ref_labels[0].label.fields[0] {
        Field::Graphic(g) => g.data.clone(),
        other => panic!("expected graphic field, got {other:?}"),
    };

    // Two-chunk parse on ONE context (the lib-level form of the session's
    // per-chunk parse): re-base the token offsets for chunk 2 exactly as
    // the TCP session does.
    let mut diags = Diagnostics::default();
    let mut mem = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut mem);
    let mut labels = Vec::new();
    parse_stream_collect(&mut Scanner::new(&src[..mid]), &mut ctx, &mut labels);
    assert!(labels.is_empty(), "chunk 1 must not close a label yet");
    ctx.stream_base = mid;
    ctx.token_start = mid;
    ctx.token_end = mid;
    parse_stream_collect(&mut Scanner::new(&src[mid..]), &mut ctx, &mut labels);

    assert_eq!(labels.len(), 1, "one label, no phantom format from ^/~ bait");
    assert_eq!(diags.items.len(), 0, "a payload spanning chunks is valid input: {diags:?}");
    assert_eq!(
        &src[labels[0].start..labels[0].end],
        &src[..],
        "the raw span covers the format incl. the whole binary payload"
    );
    match &labels[0].label.fields[0] {
        Field::Graphic(g) => {
            assert_eq!(g.data, ref_data, "two-chunk decode must byte-match the single-chunk one");
            assert_eq!(g.bytes_per_row, 5);
        }
        other => panic!("expected graphic field, got {other:?}"),
    }

    // Span round-trip: re-parse the archived raw slice with a fresh
    // context - the split must have consumed exactly the payload bytes,
    // no more, no less.
    let mut diags2 = Diagnostics::default();
    let mut mem2 = Memory::default();
    let mut ctx2 = ParseContext::new(&mut diags2, &mut mem2);
    parse_stream(&mut Scanner::new(&src[labels[0].start..labels[0].end]), &mut ctx2);
    assert!(
        ctx2.diags.items.is_empty(),
        "no diagnostics on re-parse: {:?}",
        ctx2.diags.items
    );
    let label2 = ctx2.pending_label.expect("re-parsed format closes");
    match &label2.fields[0] {
        Field::Graphic(g) => {
            assert_eq!(g.data, ref_data, "archived raw span must re-parse to the same decode");
            assert_eq!(g.bytes_per_row, 5);
        }
        other => panic!("expected graphic field, got {other:?}"),
    }
}

#[test]
fn gf_p_zb64_png_graphic() {
    // ^GF P carries a ZB64-encoded PNG (guide p.1602: base64 emits only
    // printable ASCII, so no raw framing is needed); width/height come
    // from the PNG itself, not the declared b/c/d.
    let png = tiny_gray_png();
    let b64 = base64(&png);
    let mut src = b"^XA^GFP,10,10,2,".to_vec();
    src.extend_from_slice(b64.as_bytes());
    src.extend_from_slice(b"^FS^XZ");
    let (label, diags) = parse_one(&src);
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let g = graphic_field(&label, 0);
    assert_eq!(g.bytes_per_row, 2, "10 dots wide -> 2 bytes per row");
    let mut expect = Vec::new();
    for _ in 0..8 {
        expect.extend_from_slice(&[0xFF, 0x00]); // 8 black + 2 white per row
    }
    assert_eq!(g.data, expect, "PNG decodes to the same 1bpp rows");
}

#[test]
fn gf_p_malformed_warns_and_skips() {
    let (label, diags) = parse_one(b"^XA^GFP,1,1,1,!!!not-base64!!!^FS^XZ");
    assert!(diags.items.iter().any(|d| d.command == "GF"));
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(label.fields.len(), 0, "no partial field on malformed P");
}

/// 10x8, 1-bit grayscale PNG: 8 black then 2 white pixels per row. PNG
/// packs 0 = black; decoding flips to ZPL's 1 = black convention.
fn tiny_gray_png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, 10, 8);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::One);
        let mut w = enc.write_header().unwrap();
        let row = [0x00u8, 0xC0];
        let data: Vec<u8> = (0..8).flat_map(|_| row).collect();
        w.write_image_data(&data).unwrap();
    }
    out
}

/// Minimal MIME base64 encoder for test fixtures (the crate's decoder is
/// crate-internal; integration tests build the payload themselves).
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let mut n = (chunk[0] as u32) << 16;
        if chunk.len() > 1 {
            n |= (chunk[1] as u32) << 8;
        }
        if chunk.len() > 2 {
            n |= chunk[2] as u32;
        }
        out.push(TABLE[(n >> 18) as usize & 0x3F] as char);
        out.push(TABLE[(n >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}
