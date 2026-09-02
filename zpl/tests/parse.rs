//! Integration tests for the Task 5 text-field command set.

mod common;

use common::parse_one;
use zpl::diag::Severity;
use zpl::ir::{Field, Justification, Label, OriginMode, Rotation, TextBlock, TextField};

fn text_field(label: &Label, i: usize) -> &TextField {
    match &label.fields[i] {
        Field::Text(t) => t,
        other => panic!("expected text field, got {other:?}"),
    }
}

#[test]
fn fo_ft_fd_fs_build_field() {
    let (label, diags) = parse_one(b"^XA^FO100,50^A0N,30,30^FDHello^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!(t.x, 100);
    assert_eq!(t.y, 50);
    assert_eq!(t.origin, OriginMode::Block);
    // Font 0 is the default font (canonical A); the raw spec is kept in byte 1.
    assert_eq!(t.font, [b'A', b'0']);
    // "N" in ^A0N is the orientation: normal rotation. h/w are 30,30.
    assert_eq!(t.rotation, Rotation::R0);
    assert_eq!((t.font_height, t.font_width), (30, 30));
    assert_eq!(t.data, "Hello");
    assert!(!t.re_eval);
}

#[test]
fn ft_sets_dot_origin_and_defaults_font() {
    let (label, diags) = parse_one(b"^XA^FT50,60^FDWorld^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let t = text_field(&label, 0);
    assert_eq!(t.origin, OriginMode::Dot);
    assert_eq!((t.x, t.y), (50, 60));
    assert_eq!(t.data, "World");
    // No ^A/^CF: the power-up default font A (raw byte unset) and its
    // power-up character size 9x5 dots.
    assert_eq!(t.font, [b'A', b' ']);
    assert_eq!((t.font_height, t.font_width), (9, 5));
    assert_eq!(t.rotation, Rotation::R0);
}

#[test]
fn fh_hex_escapes() {
    // _41 _42 __ -> 'A' 'B' '_' with the default underscore indicator.
    let (label, diags) = parse_one(b"^XA^FO10,10^FH^FD_41_42__^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label, 0).data, "AB_");

    // A custom indicator character.
    let (label, diags) = parse_one(b"^XA^FO10,10^FHX^FDX41X42X5F^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label, 0).data, "AB_");
}

#[test]
fn malformed_hex_escape_warns_and_stays_literal() {
    let (label, diags) = parse_one(b"^XA^FO10,10^FH^FD_4Z_text^FS^XZ");
    assert!(diags.items.iter().any(|d| d.severity == Severity::Warning));
    // "_4" is malformed (Z is not hex); kept literally, rest unchanged.
    assert_eq!(text_field(&label, 0).data, "_4Z_text");
}

#[test]
fn fb_block_parses() {
    let (label, diags) = parse_one(b"^XA^FO10,10^FB200,3,10,L^FDHello^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(
        text_field(&label, 0).block,
        Some(TextBlock {
            width: 200,
            max_lines: 3,
            line_spacing: 10,
            justification: Justification::Left,
        })
    );
}

#[test]
fn fw_rotations() {
    let cases: &[(&[u8], Rotation)] = &[
        (b"^XA^FW0^FO10,10^FDx^FS^XZ", Rotation::R0),
        (b"^XA^FWN^FO10,10^FDx^FS^XZ", Rotation::R0),
        (b"^XA^FW90^FO10,10^FDx^FS^XZ", Rotation::R90),
        (b"^XA^FWR^FO10,10^FDx^FS^XZ", Rotation::R90),
        (b"^XA^FWI^FO10,10^FDx^FS^XZ", Rotation::R180),
        (b"^XA^FWB^FO10,10^FDx^FS^XZ", Rotation::R270),
    ];
    for (src, want) in cases {
        let (label, diags) = parse_one(src);
        assert_eq!(diags.items.len(), 0, "{:?}: {diags:?}", String::from_utf8_lossy(src));
        assert_eq!(text_field(&label, 0).rotation, *want);
    }

    // ^A's orientation overrides the ^FW default (letter-font form: ^AAN =
    // font A with orientation N; the scanner keeps the extra letter in
    // params)...
    let (label, _) = parse_one(b"^XA^FW90^FO10,10^AAN,30,30^FDx^FS^XZ");
    let t = text_field(&label, 0);
    assert_eq!(t.rotation, Rotation::R0);
    assert_eq!(t.font, [b'A', b'A']);
    assert_eq!((t.font_height, t.font_width), (30, 30));
    // ...and persists across formats (printer state, guide: until power off).
    let (label, _) = parse_one(b"^XA^FW90^XZ^XA^FO10,10^FDx^FS^XZ");
    assert_eq!(text_field(&label, 0).rotation, Rotation::R90);
}

#[test]
fn fw_invalid_rotation_warns_and_uses_r0() {
    let (label, diags) = parse_one(b"^XA^FWq^FO10,10^FDx^FS^XZ");
    assert!(!diags.items.is_empty());
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(text_field(&label, 0).rotation, Rotation::R0);
}

#[test]
fn fr_reverses_field() {
    let (label, diags) = parse_one(b"^XA^FO10,10^FR^FDx^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert!(text_field(&label, 0).reverse);
    // ^FR applies to one field only: the next one is normal again.
    let (label, _) = parse_one(b"^XA^FO10,10^FR^FDx^FS^FO20,20^FDy^FS^XZ");
    assert!(text_field(&label, 0).reverse);
    assert!(!text_field(&label, 1).reverse);
}

#[test]
fn fv_marks_field_re_eval() {
    let (label, diags) = parse_one(b"^XA^FO10,10^FVHello^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let t = text_field(&label, 0);
    assert_eq!(t.data, "Hello");
    assert!(t.re_eval);

    let (label, _) = parse_one(b"^XA^FO10,10^FDHello^FS^XZ");
    assert!(!text_field(&label, 0).re_eval);
}

#[test]
fn cf_sets_default_font_for_following_fields() {
    let (label, diags) = parse_one(b"^XA^CFE^FO10,10^FDx^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label, 0).font, [b'E', b'E']);

    // ^CF0 -> canonical A, raw 0.
    let (label, _) = parse_one(b"^XA^CF0^FO10,10^FDx^FS^XZ");
    assert_eq!(text_field(&label, 0).font, [b'A', b'0']);
}

#[test]
fn multiple_fd_chunks_concatenate_into_one_field() {
    // ^FS-less continued ^FDs accumulate into the open field.
    let (label, diags) = parse_one(b"^XA^FO10,10^FDHel^FDlo^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    assert_eq!(text_field(&label, 0).data, "Hello");
}

#[test]
fn font_sizes_flow_into_field() {
    // Explicit 0 keeps the standard font matrix (guide ^A page).
    let (label, diags) = parse_one(b"^XA^FO10,10^A0,0,0^FDx^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let t = text_field(&label, 0);
    assert_eq!((t.font_height, t.font_width), (0, 0));

    // A bare ^A inherits the current ^CF sizes (power-up 9x5).
    let (label, _) = parse_one(b"^XA^FO10,10^A0^FDx^FS^XZ");
    let t = text_field(&label, 0);
    assert_eq!((t.font_height, t.font_width), (9, 5));

    // ^CF h,w become the defaults for following fields.
    let (label, _) = parse_one(b"^XA^CF0,24,20^FO10,10^FDx^FS^XZ");
    let t = text_field(&label, 0);
    assert_eq!((t.font_height, t.font_width), (24, 20));

    // And an ^A with sizes overrides them.
    let (label, _) = parse_one(b"^XA^CF0,24,20^FO10,10^A0,18,18^FDx^FS^XZ");
    let t = text_field(&label, 0);
    assert_eq!((t.font_height, t.font_width), (18, 18));
}

#[test]
fn multiline_format_trims_line_endings_from_params() {
    // Multi-line ZPL puts the line break inside the final parameter segment;
    // numeric handlers must trim it (no spurious warnings), while ^FD data
    // keeps its newline bytes, as Zebra does.
    let (label, diags) = parse_one(b"^XA\n^FW90\n^FO100,50\n^A0,30,30\n^FDHi\n^FS\n^XZ\n");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (100, 50));
    assert_eq!((t.font_height, t.font_width), (30, 30));
    assert_eq!(t.rotation, Rotation::R90); // ^FW90 survives its trailing \n
    assert_eq!(t.data, "Hi\n");

    // CRLF variant.
    let (label, diags) = parse_one(b"^XA\r\n^FO100,50\r\n^A0,30,30\r\n^FDHi\r\n^FS\r\n^XZ\r\n");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (100, 50));
    assert_eq!((t.font_height, t.font_width), (30, 30));
    assert_eq!(t.rotation, Rotation::R0);
    assert_eq!(t.data, "Hi\r\n");
}

#[test]
fn repeated_field_commands_keep_one_draft() {
    // ^FS-less continuation: data concatenates and the last origin wins, so
    // ^FD...^FD (and a second ^FO) all describe one field.
    let (label, diags) = parse_one(b"^XA^FO10,10^FDHi^FO20,30^FD there^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (20, 30)); // last ^FO wins
    assert_eq!(t.data, "Hi there");

    // ^FH hex is per field: a fresh field decodes nothing.
    let (label, _) = parse_one(b"^XA^FO10,10^FH^FD_41^FS^FO20,20^FD_42^FS^XZ");
    assert_eq!(label.fields.len(), 2);
    assert_eq!(text_field(&label, 0).data, "A");
    assert_eq!(text_field(&label, 1).data, "_42");
}

#[test]
fn malformed_params_warn_not_panic() {
    let (label, diags) = parse_one(b"^XA^FOabc^FBx,y^FDHello^FS^XZ");
    // ^FOabc -> warning + origin (0,0); ^FBx,y -> warning + guide defaults.
    assert!(diags.items.len() >= 2, "expected warnings, got {diags:?}");
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (0, 0));
    assert_eq!(
        t.block,
        Some(TextBlock {
            width: 0,
            max_lines: 1,
            line_spacing: 0,
            justification: Justification::Left,
        })
    );
    assert_eq!(t.data, "Hello");
}
