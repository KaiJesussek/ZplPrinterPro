//! Integration tests for the Task 5 text-field and Task 6 label-configuration
//! command sets.

mod common;

use common::parse_one;
use zpl::diag::{Diagnostics, Severity};
use zpl::ir::{Field, Justification, Label, OriginMode, Rotation, TextBlock, TextField};
use zpl::state::{Memory, ParseContext};

fn text_field(label: &Label, i: usize) -> &TextField {
    match &label.fields[i] {
        Field::Text(t) => t,
        other => panic!("expected text field, got {other:?}"),
    }
}

fn field_reverse(label: &Label, i: usize) -> bool {
    match &label.fields[i] {
        Field::Text(t) => t.reverse,
        Field::Barcode(_) => false, // barcodes have no reverse flag
        Field::Box(b) => b.reverse,
        Field::Graphic(_) => false, // ^GF has no reverse flag
        Field::Circle(c) => c.reverse,
        Field::Diagonal(d) => d.reverse,
        Field::Ellipse(e) => e.reverse,
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
            fb: true,
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
        assert_eq!(
            diags.items.len(),
            0,
            "{:?}: {diags:?}",
            String::from_utf8_lossy(src)
        );
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
            fb: true,
        })
    );
    assert_eq!(t.data, "Hello");
}

// --- Task 6: label configuration commands ---------------------------------

#[test]
fn config_commands_affect_label() {
    // ^XA^PW812^LL500^LH20,30^PQ3^XZ -> width 812, height 500, copies 3; the
    // ^LH offset is stored in the mode and applied to field origins at
    // render time (fields keep their own coordinates in the IR).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) =
        common::parse_ctx(b"^XA^PW812^LL500^LH20,30^PQ3^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.width_dots, 812);
    assert_eq!(label.height_dots, 500);
    assert_eq!(label.copies, 3);
    assert_eq!((ctx.mode.lh_x, ctx.mode.lh_y), (20, 30));

    // The offset is render-time, not baked into field origins.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(b"^XA^LH20,30^FO10,10^FDx^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!((ctx.mode.lh_x, ctx.mode.lh_y), (20, 30));
    assert_eq!((text_field(&label, 0).x, text_field(&label, 0).y), (10, 10));
}

#[test]
fn defaults_match_guide() {
    // Empty format: the guide's power-up defaults. ^PW defaults to the
    // printer width (832 dots for a 203-dpi 4-inch printer, the guide's
    // configuration-label example); ^LL omitted -> auto height, represented
    // as 0 in the IR (the renderer sizes the label to its content); ^PQ
    // defaults to 1 copy.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(b"^XA^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.width_dots, 832);
    assert_eq!(label.height_dots, 0); // auto
    assert_eq!(label.copies, 1);
    assert_eq!((ctx.mode.lh_x, ctx.mode.lh_y), (0, 0));
    assert_eq!(ctx.mode.orientation, 0); // ^PO N (normal)
    assert!(!ctx.mode.mirror);
    assert_eq!(ctx.mode.left_shift, 0);
    assert_eq!(ctx.mode.label_top, 0);
    assert_eq!(ctx.config.darkness, 0);
    assert_eq!(ctx.config.media_tracking, 0); // unset: guide requires a value
    assert_eq!(ctx.config.units, b'D'); // ^MU power-up D (dots)
    assert_eq!(ctx.config.zpl_mode, 2); // ^SZ power-up 2 = ZPL II
}

#[test]
fn omitted_ll_means_auto_label_height() {
    // Guide (continuous media): the label length comes from the content when
    // ^LL is not given; explicit ^LL y (1..32000) fixes the height.
    let (label, diags) = parse_one(b"^XA^FO0,0^FDx^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.height_dots, 0); // auto

    let (label, _) = parse_one(b"^XA^LL300^XZ");
    assert_eq!(label.height_dots, 300);
}

#[test]
fn pq_sets_copies_and_parses_replicates() {
    // Guide format ^PQq,p,r,o,e: q (1..99,999,999, default 1) is the
    // total quantity and sets copies; r (replicates of each serial number,
    // 0..99,999,999, default 0) is stored on the mode for the ^SN counter
    // trajectory. The pause/override/cut params (p/o/e) are accepted and
    // ignored without warning.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(b"^XA^PQ5,10,1,Y^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.copies, 5);
    assert_eq!(ctx.mode.copies, 5);
    assert_eq!(ctx.mode.replicates, 1);

    // Invalid q warns; the quantity stays at the previous value.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(b"^XA^PQabc^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        1,
        "expected one warning, got {0:?}",
        ctx.diags
    );
    assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
    assert_eq!(label.copies, 1);
    assert_eq!(ctx.mode.copies, 1);
}

#[test]
fn orientation_mirror_and_shifts_store_in_mode() {
    // ^PO N/I (guide: only these two), ^PM N/Y, ^LS -9999..9999, ^LT -120..120.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^POI^PMY^LS-50^LT-10^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.mode.orientation, 1); // I = inverted
    assert!(ctx.mode.mirror);
    assert_eq!(ctx.mode.left_shift, -50);
    assert_eq!(ctx.mode.label_top, -10);

    // Invalid or out-of-range values are ignored with a warning.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^POS^PMx^LS99999^LT999^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        4,
        "expected four warnings, got {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.mode.orientation, 0);
    assert!(!ctx.mode.mirror);
    assert_eq!(ctx.mode.left_shift, 0);
    assert_eq!(ctx.mode.label_top, 0);
}

#[test]
fn darkness_media_units_and_zpl_mode_stored_in_config() {
    // ^MD is a signed relative adjustment (-30..30, power-up 0); ~SD sets
    // the absolute 00..30. Both keep the 0.1-step resolution of
    // XiIIIPlus-class printers: stored in tenths (8.3 -> 83).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^MD8.3^MNV^MUI^SZ2^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.config.darkness, 83);
    assert_eq!(ctx.config.media_tracking, b'V');
    assert_eq!(ctx.config.units, b'I');
    assert_eq!(ctx.config.zpl_mode, 2);

    // ~SD writes absolute darkness (tilde printer operation, any time).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA~SD24^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.config.darkness, 240);

    // Malformed values warn and keep the current setting.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^MDabc^MN0^MUx^SZ3^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        4,
        "expected four warnings, got {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.config.darkness, 0);
    assert_eq!(ctx.config.media_tracking, 0);
    assert_eq!(ctx.config.units, b'D');
    assert_eq!(ctx.config.zpl_mode, 2);
}

#[test]
fn accept_only_config_commands_never_warn_on_valid_params() {
    // Every accept-only command fed guide-valid parameters: parsed, no
    // warnings, format completes.
    let (label, diags) = parse_one(b"^XA^PR12,5,D^MTT^MMC,Y^MFN,N^MIR,HEAD CLEANING^ML4000^MPD^MWN^PA1,0,1,0^PF150^SC9600,8,N,1,X,N^ZZ600,N^XSY,Y^XB^PH^PN10^PON^PMN^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 0);
}

#[test]
fn oversized_darkness_params_warn_not_panic() {
    // i32-overflow repros from review round 1: ^MD214748365 and
    // ~SD999999999 overflow `w * 10` in `to_tenths` under debug builds.
    // Checked arithmetic routes them into the warn + fallback path instead
    // of panicking; the current darkness is kept.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^MD214748365^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        1,
        "expected one warning, got {0:?}",
        ctx.diags
    );
    assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
    assert_eq!(ctx.config.darkness, 0);

    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA^MD-214748365^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        1,
        "expected one warning, got {0:?}",
        ctx.diags
    );
    assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
    assert_eq!(ctx.config.darkness, 0);

    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(b"^XA~SD999999999^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        1,
        "expected one warning, got {0:?}",
        ctx.diags
    );
    assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
    assert_eq!(ctx.config.darkness, 0);
}

#[test]
fn config_malformed_params_warn_not_panic() {
    // The brief's headline case: ^XA^PQabc^XZ -> warning, parse completes.
    let (label, diags) = parse_one(b"^XA^PQabc^XZ");
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(label.copies, 1);

    // A sweep of garbage through every config family warns and never panics;
    // settings fall back to their current values.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = common::parse_ctx(
        b"^XA^LHx,y^PW-1^LL99999^LSz^LTw^MDq^PO0^PMq^MN9^MT9^MM9^MU9^MF9^MI9,9^ML-1^MP9^MW9^PA9,9,9,9^PFX^SC9,9,9,9,9,9~SD99^SZ9^ZZx,y^XS9,9,9^PNx^XZ",
        &mut diags,
        &mut memory,
    );
    assert!(
        ctx.diags.items.len() >= 35,
        "expected >= 35 warnings, got {}",
        ctx.diags.items.len()
    );
    assert!(ctx
        .diags
        .items
        .iter()
        .all(|d| d.severity == Severity::Warning));
    assert_eq!(ctx.mode.lh_x, 0);
    assert_eq!(ctx.mode.pw_dots, 832); // kept (guide default)
    assert_eq!(ctx.config.darkness, 0);
    assert_eq!(ctx.config.zpl_mode, 2);
}

// --- Task 17: serial numbers, accept-only remainder, ^CC/^CT ---------------

#[test]
fn sn_serializes_and_increments_across_formats() {
    // Guide ^SNv,n,z (p.341): v is the data string whose rightmost integer
    // run is the starting value (default 1), n the signed increment (default
    // 1), z = Y/N leading zeros (default N). Expansion happens at parse time
    // (first copy's value); the counter lives on ParseContext, so the next
    // format parsed in the same session continues from where the last left
    // off (documented v1 simplification of the printer's reset-on-re-send).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) =
        common::parse_ctx(b"^XA^FO10,10^SN001,1,Y^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label, 0).data, "001");

    // The same format re-sent: the counter continues (001 -> 002). The ^SN
    // command opens its own field - no ^FD needed.
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN001,1,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed by ^XZ");
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label2, 0).data, "002");
}

#[test]
fn sn_defaults_decrement_and_embedded_text() {
    // Missing n -> 1 and missing z -> N (suppress leading zeros): ^SN0005
    // prints "5". v may embed text around the indexed run; a minus sign
    // decrements (guide: "To indicate a decrement value, precede the value
    // with a minus (-) sign").
    let (label, diags) = parse_one(b"^XA^FO10,10^SN0005^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label, 0).data, "5");

    // The counter starts at v: the first format prints the starting value,
    // the next one prints the decremented value (n=-1: 1000 -> 999).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^FO10,10^SNABCD1000EFGH,-1^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label, 0).data, "ABCD1000EFGH");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SNABCD1000EFGH,-1^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed by ^XZ");
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label2, 0).data, "ABCD999EFGH");

    // Leading zeros keep the run's width (z=Y): 0005 stays four digits and
    // increments to 0006 on the next format.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^FO10,10^SNABCD0005EFGH,1,Y^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label, 0).data, "ABCD0005EFGH");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SNABCD0005EFGH,1,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed by ^XZ");
    assert_eq!(text_field(&label2, 0).data, "ABCD0006EFGH");
}

#[test]
fn sn_counter_tracks_copies_trajectory() {
    // v1 per-copy ruling: every copy of the label renders the first copy's
    // serial value (Label.copies handles multiplicity); the counter advances
    // by n per copy, so the next format's value lands where a real printer's
    // would after printing all N labels.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) =
        common::parse_ctx(b"^XA^PQ3^FO10,10^SN001,1,Y^FS^XZ", &mut diags, &mut memory);
    assert_eq!(label.copies, 3);
    assert_eq!(text_field(&label, 0).data, "001");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN001,1,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed by ^XZ");
    assert_eq!(text_field(&label2, 0).data, "004"); // 001 + 3 copies x 1
}

#[test]
fn pq_replicates_gate_serial_trajectory() {
    // Guide ^PQ p.324: r = "replicates of each serial number" - each
    // distinct serial prints r+1 times, so ^SN counters advance once per
    // r+1 printed labels (p.341: incrementing happens "when all replicates
    // for each serial number have been printed"), while q stays the TOTAL
    // quantity (the guide's example ^PQ50,10,1,Y prints "a total of 50
    // labels with one replicate of each serial number").
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // r=1, q=4: 4 labels = 2 serials x 2 replicates -> 2 advances of step 1.
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^PQ4,0,1^FO10,10^SN001,1,Y^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        label.copies, 4,
        "q is the total quantity, not q \u{00D7} (r+1)"
    );
    assert_eq!(text_field(&label, 0).data, "001");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN001,1,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed");
    assert_eq!(
        text_field(&label2, 0).data,
        "003",
        "001 + 4/2 serials \u{00D7} 1"
    );

    // r=3, q=8 with step 2: 8 labels = 2 serials x 4 replicates -> 2
    // advances of step 2.
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^PQ8,0,3^FO10,10^SN001,2,Y^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(label.copies, 8);
    assert_eq!(ctx.mode.replicates, 3);
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN001,2,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed");
    assert_eq!(
        text_field(&label2, 0).data,
        "005",
        "001 + 8/4 serials \u{00D7} 2"
    );

    // r=3, q=2: a 2-label batch never completes a serial's 4 replicates ->
    // the counter does not advance (documented v1 reading of "when all
    // replicates ... have been printed").
    let (_, mut ctx) = common::parse_ctx(
        b"^XA^PQ2,0,3^FO10,10^SN001,1,Y^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.mode.replicates, 3);
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN001,1,Y^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed");
    assert_eq!(text_field(&label2, 0).data, "001", "no serial completed");
}

#[test]
fn accept_only_set_never_warns_on_valid_params() {
    // Every Task 17 accept-only command fed guide-valid parameters: parsed,
    // zero diagnostics, format completes. ^FX (comment) and the tilde ops
    // (~WC ~TA ~JR) have no rendering role.
    let (label, diags) = parse_one(
        b"^XA^FX pure comment text, no command chars^SQK,C,Y^SR0495^SS050,060,070,0200,128,128,050,055,128^ST06,15,2024,14,30,00,M^SXA,C,Y,N,admin@corp.com,0^TBN,120,60^FO10,10^FDTB text^FS^TO^HYR:LOGO.GRF^HZ^HU^JZY^XZ~WC~TA-12~JR",
    );
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    // The ^FX body never became fields; only the ^TB field has data.
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!(t.data, "TB text");
    assert_eq!(
        t.block,
        Some(TextBlock {
            width: 120,
            max_lines: 0, // ^TB truncates by height (no IR slot); no line cap
            line_spacing: 0,
            justification: Justification::Left,
            fb: false,
        })
    );
}

#[test]
fn accept_only_malformed_params_warn_only() {
    // Malformed accept-only params warn and are ignored; parsing continues
    // and never panics.
    let (label, diags) =
        parse_one(b"^XA^SQ1,2^SR9999^SSxxx^ST13,99,999,25,99,99,Z^SX1^JZx^TOx^XZ~TA999");
    assert!(diags.items.len() >= 9, "expected warnings, got {diags:?}");
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(label.fields.len(), 0);
}

#[test]
fn sn_malformed_params_warn_not_panic() {
    // Garbage increment/leading-zeros fall back to the guide defaults with a
    // warning; the format still completes, never panics.
    let (label, diags) = parse_one(b"^XA^FO10,10^SN001,x,Z^FS^XZ");
    assert!(diags.items.len() >= 2, "expected warnings, got {diags:?}");
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    // step falls back to 1, zeros to N: the run's value prints unpadded.
    assert_eq!(text_field(&label, 0).data, "1");

    // An increment longer than the guide's 12 digits warns and falls back to
    // 1. An overlength digit RUN is not an error: "a maximum of 12 of the
    // right-most integers are subject to indexing", so the first copy prints
    // its run byte-for-byte unchanged (never a saturated substitution).
    let (label, diags) = parse_one(b"^XA^FO10,10^SN12345678901234567890,99999999999999^FS^XZ");
    assert!(!diags.items.is_empty(), "expected warnings, got {diags:?}");
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert_eq!(text_field(&label, 0).data, "12345678901234567890");
}

#[test]
fn sn_indexes_only_the_rightmost_12_digits() {
    // Guide: "a maximum of 12 of the right-most integers are subject to
    // indexing". The rightmost 12 digits step; the leading digits stay
    // literal.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^FO10,10^SN12345678901234567890,1^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(text_field(&label, 0).data, "12345678901234567890");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SN12345678901234567890,1^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx
        .pending_label
        .take()
        .expect("second format never closed by ^XZ");
    // 901234567890 + 1 only: the leading 8 digits are literal.
    assert_eq!(text_field(&label2, 0).data, "12345678901234567891");
}

#[test]
fn cc_changes_command_char_applied_by_driver() {
    // Guide example (p.152): after ^CC/ the format continues in the new
    // prefix character (here /), and /XZ closes it. The scanner consumes
    // exactly one ^CC parameter byte, so the rest of the stream is not
    // swallowed.
    let (label, diags) = parse_one(b"^XA^CC//FO10,10/FDhi/FS/XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (10, 10));
    assert_eq!(t.data, "hi");

    // The guide's own layout - a newline between the changed prefix and the
    // closing tag: ^XA ^CC/ <newline> /XZ.
    let (label, diags) = parse_one(b"^XA^CC/\n/XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 0);
}

#[test]
fn ct_changes_tilde_char_applied_by_driver() {
    // Guide example (p.166): ^CT+ makes + the control-command prefix while
    // the caret still starts format commands. +SD19 after ^XZ dispatches as
    // the ~SD printer operation (darkness 19 -> 190 in tenths), and +HS
    // scans as a command - now registered (Task 20), so it records a host
    // status query instead of warning as unknown.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = common::parse_ctx(b"^XA^CT+^XZ+SD19+HS", &mut diags, &mut memory);
    assert_eq!(ctx.mode.tilde_char, b'+');
    assert_eq!(ctx.config.darkness, 190);
    assert_eq!(label.fields.len(), 0);
    assert_eq!(ctx.diags.items.len(), 0, "registered queries must not warn");
    assert_eq!(ctx.queries, vec![zpl::state::QueryRequest::HostStatus]);
}

#[test]
fn fx_comment_body_ignored() {
    // The ^FX body (params up to the next command char) is never dispatched:
    // it may contain arbitrary bytes, including things that would otherwise
    // scan as commands. Tildes are excluded here because a tilde ends ^FX
    // params outside a field (guide: "avoid using the caret or tilde
    // commands within the ^FX statement").
    let (label, diags) =
        parse_one(b"^XA^FX_body with (,<>@#%% garbage (NOT commands^FO10,10^FDreal^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(label.fields.len(), 1);
    assert_eq!(text_field(&label, 0).data, "real");
}

// --- ^SF serialized-field mask (guide p.335) ------------------------------

#[test]
fn sf_mask_serializes_right_aligned_odometer() {
    // Mask "%%DDDDD" aligned to the ^FD from the right: two literal '%' and
    // a five-digit decimal odometer. First print shows the starting value,
    // each later format in the same session advances by one.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) = common::parse_ctx(
        b"^XA^FO10,10^SF%%DDDDD,1^FDAB00123^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(text_field(&label, 0).data, "AB00123");

    // The mask covers the rightmost 7 chars: "00123" (+ "AB") -> the
    // odometer reads 123 and advances on the next print.
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SF%%DDDDD,1^FDAB00123^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx.pending_label.take().expect("second format");
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(text_field(&label2, 0).data, "AB00124");
}

#[test]
fn sf_alnum_and_alpha_classes_carry_like_an_odometer() {
    // "NNA": the rightmost mask char is the least-significant odometer
    // position. data "12A" -> d0('A')=0, d1('2')=2, d2('1')=1, weights
    // 1, 26, 26*36 -> value 988. +1 -> 989 -> 989%26 = 1 = 'B'; carries
    // leave the N positions at 2,1 -> the second print reads "12B".
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) =
        common::parse_ctx(b"^XA^FO10,10^SFNNA,1^FD12A^FS^XZ", &mut diags, &mut memory);
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(text_field(&label, 0).data, "12A");

    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SFNNA,1^FD12A^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx.pending_label.take().expect("second format");
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(
        text_field(&label2, 0).data,
        "12B",
        "rightmost alpha carries"
    );
}

#[test]
fn sf_alpha_wraps_to_class_zero() {
    // "A" alone with value "Z" + one -> wraps to "A" (class zero).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, mut ctx) =
        common::parse_ctx(b"^XA^FO10,10^SFA,1^FDZ^FS^XZ", &mut diags, &mut memory);
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(text_field(&label, 0).data, "Z");
    let mut scanner = zpl::scanner::Scanner::new(b"^XA^FO10,10^SFA,1^FDZ^FS^XZ");
    zpl::cmd::parse_stream(&mut scanner, &mut ctx);
    let label2 = ctx.pending_label.take().expect("second format");
    assert_eq!(text_field(&label2, 0).data, "A", "Z+1 wraps to A");
}

#[test]
fn sf_invalid_mask_warns_and_default_increment_is_one() {
    // 'X' is not a mask class -> the ^SF is ignored with a warning.
    let (label, diags) = parse_one(b"^XA^FO10,10^SFXXD,1^FD001^FS^XZ");
    assert_eq!(diags.items.len(), 1, "invalid mask must warn: {diags:?}");
    // Field data is still produced verbatim (the serialization is skipped).
    assert_eq!(text_field(&label, 0).data, "001");
}

// --- ^FE field-data insertion, ^FL, ^LR (guide p.194/195/295) ---------------

#[test]
fn fe_inserts_fn_fields_into_fd() {
    // The guide's own example (p.194): #n# inserts the whole ^FN field,
    // and only ^FN fields already closed in the format are visible.
    let (label, diags) = parse_one(
        b"^XA^FO10,10^FN2^FDField FN 2 Data^FS^FN3^FDField FN 3 Data^FS^FE#^FO10,20^FD#2# and then #3#^FS^XZ",
    );
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let inserted = text_field(&label, 2).data.clone();
    assert_eq!(inserted, "Field FN 2 Data and then Field FN 3 Data");
}

#[test]
fn fe_forward_and_backward_slices() {
    // #n,f,x,y# = x chars in from the start; #n,b,x,y# = x chars in from
    // the end (1 = the last char), y chars taken; a y overrun takes
    // everything from the start to the end (guide p.195).
    let (label, diags) = parse_one(
        b"^XA^FO10,10^FN1^FD1234567890^FS^FE#^FO10,20^FD#1,f,3,4#-#1,b,2,4#-#1,f,8,99#^FS^XZ",
    );
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let out = text_field(&label, 1).data.clone();
    // f,3,4 = "3456"; b,2,4: 2nd-from-last onward = "90" only (overrun);
    // f,8,99 = "890" (overrun to the end).
    assert_eq!(out, "3456-90-890");
}

#[test]
fn fe_unknown_number_stays_literal_and_bad_delimiter_warns() {
    // #9# is not defined: the token stays literal.
    let (label, diags) = parse_one(b"^XA^FO10,10^FN1^FDabc^FS^FE#^FO10,20^FD#9#^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label, 1).data, "#9#");

    // '~' is reserved as the control prefix: ^FE~ lexes as ^FE (no param
    // -> empty-delimiter warning) plus a dangling ~ token (unknown-command
    // warning). The delimiter stays '#', so the default substitution still
    // happens; nothing crashes on the reserved byte.
    let (label, diags) = parse_one(b"^XA^FO10,10^FN1^FDabc^FS^FE~^FO10,20^FD#1#^FS^XZ");
    assert_eq!(
        diags.items.len(),
        2,
        "rejected delimiter must warn: {diags:?}"
    );
    assert_eq!(text_field(&label, 1).data, "abc");
}

#[test]
fn lr_reverses_only_fields_after_it() {
    // ^LRN then ^LRY: the first field closes under N (normal), the second
    // under Y (the draft's reverse flag is set, like an implicit ^FR).
    let (label, diags) = parse_one(b"^XA^FO10,10^FDbefore^FS^LRN^FO10,30^FDon^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert!(!field_reverse(&label, 0), "before ^LRY prints normal");
    let (label, diags) = parse_one(b"^XA^FO10,10^FDbefore^FS^LRY^FO10,30^FDon^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert!(!field_reverse(&label, 0), "field before ^LRY stays normal");
    assert!(field_reverse(&label, 1), "field after ^LRY reverses");
    // Bad value -> warning + N.
    let (label, diags) = parse_one(b"^XA^LRQ^FO10,10^FDx^FS^XZ");
    assert_eq!(diags.items.len(), 1, "invalid ^LR must warn: {diags:?}");
    assert!(!field_reverse(&label, 0));
}

#[test]
fn fl_validates_and_records_font_links_in_config() {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_label, ctx) = common::parse_ctx(
        b"^XA^FLE:SWISS721.TTF,0.FNT,1^FS^FL*.TTF,Arial.TTE,0^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.diags.items.len(), 0, "unexpected: {:?}", ctx.diags);
    assert_eq!(ctx.config.font_links.len(), 2);
    assert_eq!(
        ctx.config.font_links[0],
        (b"E:SWISS721.TTF".to_vec(), b"0.FNT".to_vec(), 1)
    );
    // Bad link value -> warning, nothing stored.
    let mut diags = Diagnostics::default();
    let (_label, ctx) = common::parse_ctx(b"^XA^FLE:A.TTF,0.FNT,7^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        1,
        "invalid link must warn: {:?}",
        ctx.diags
    );
    assert!(ctx.config.font_links.is_empty());
}

#[test]
fn hardware_stores_validate_ranges() {
    // ^SO clock 2-3 and offsets -32000..32000; ^SP row 0..32000;
    // ^SI setting 1-2 value 0..196; ^SL mode S/T/1-999.
    let src = b"^XA^SO2,1,-2,3^FS^SP500^SI1,50^SLT,1^FS^XZ";
    let (_label, diags) = parse_one(src);
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    // Out of range -> warnings, no panic.
    let src = b"^XA^SO5^FS^SP99999^SI1,300^SL0^FS^XZ";
    let (_label, diags) = parse_one(src);
    assert_eq!(diags.items.len(), 4, "each bad range must warn: {diags:?}");
}

// --- Audit sweep: every documented mnemonic has a handler ----------------

/// The guide's full command set: parsing a bare instance of each documented
/// ^/~ mnemonic must never produce an UnknownCommand diagnostic (an
/// "unknown command" warning for a documented command is an audit finding).
#[test]
fn every_documented_mnemonic_dispatches() {
    let carets: &[&str] = &[
        "A@", "Af", "B0", "B1", "B2", "B3", "B4", "B5", "B7", "B8", "B9", "BA", "BB", "BC", "BD",
        "BE", "BF", "BI", "BJ", "BK", "BL", "BM", "BO", "BP", "BQ", "BR", "BS", "BT", "BU", "BX",
        "BY", "BZ", "CC", "CF", "CI", "CM", "CN", "CO", "CP", "CT", "CV", "CW", "DF", "FB", "FC",
        "FD", "FE", "FH", "FL", "FN", "FO", "FP", "FR", "FS", "FT", "FV", "FW", "FX", "GB", "GC",
        "GD", "GE", "GF", "GS", "HF", "HG", "HH", "HL", "HR", "HV", "HW", "HY", "HZ", "ID", "IL",
        "IM", "IS", "JB", "JH", "JI", "JJ", "JM", "JS", "JT", "JU", "JW", "JZ", "KC", "KD", "KL",
        "KN", "KP", "KV", "LH", "LL", "LR", "LS", "LT", "MA", "MC", "MD", "MF", "MI", "ML", "MM",
        "MN", "MP", "MT", "MU", "MW", "NB", "NC", "ND", "NI", "NN", "NP", "NS", "NT", "NW", "PA",
        "PF", "PH", "PL", "PM", "PN", "PO", "PP", "PQ", "RB", "RF", "RL", "RS", "RU", "RW", "SC",
        "SF", "SI", "SL", "SN", "SO", "SP", "SQ", "SR", "SS", "ST", "SX", "SZ", "TB", "TO", "WA",
        "WD", "WE", "WL", "WP", "WR", "WS", "WX", "XA", "XB", "XF", "XG", "XS", "XZ", "ZZ",
    ];
    let tildes: &[&str] = &[
        "DB", "DE", "DG", "DN", "DS", "DT", "DU", "DV", "HB", "HD", "HI", "HM", "HQ", "HU", "JA",
        "JB", "JC", "JD", "JE", "JF", "JG", "JI", "JL", "JN", "JO", "JP", "JQ", "JR", "JS", "JX",
        "KB", "NC", "NR", "NT", "PM", "PR", "RO", "SD", "TA", "WC", "WL", "WQ", "WR",
    ];
    let mut bad = Vec::new();
    for &m in carets {
        // A bare command may not close a label (^DF downloads, queries) -
        // only the UnknownCommand diagnostics matter here.
        let src = format!("^XA^{m}^XZ");
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut memory);
        zpl::cmd::parse_stream(&mut zpl::scanner::Scanner::new(src.as_bytes()), &mut ctx);
        for d in &ctx.diags.items {
            if d.severity == zpl::diag::Severity::UnknownCommand {
                bad.push(format!("^{m}: unknown"));
            }
        }
    }
    for &m in tildes {
        let src = format!("~{m}^XA^XZ");
        let mut diags = Diagnostics::default();
        let mut memory = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut memory);
        zpl::cmd::parse_stream(&mut zpl::scanner::Scanner::new(src.as_bytes()), &mut ctx);
        for d in &ctx.diags.items {
            if d.severity == zpl::diag::Severity::UnknownCommand {
                bad.push(format!("~{m}: unknown"));
            }
        }
    }
    assert!(bad.is_empty(), "undispatched documented commands: {bad:?}");
}
