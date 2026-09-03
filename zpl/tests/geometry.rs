//! Integration tests for the Task 8 drawing-field command set:
//! `^GB ^GC ^GD ^GE ^GF`.

mod common;

use common::parse_one;
use zpl::diag::Severity;
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
    // Only ASCII hex (a=A) is supported. Binary (B) and compressed-binary
    // (C) payloads cannot ride a ZPL text stream (any ^/~ byte would split
    // the token) and C's scheme is host-side/undocumented - both warn and
    // skip, never panic. Unknown letters warn and skip too.
    for src in [
        b"^XA^GFB,4,4,2,FFFF^FS^XZ".as_slice(),
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
