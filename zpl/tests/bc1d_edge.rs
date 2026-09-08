//! Edge-path integration tests for the barcode framework: the ^BY/^BC
//! option branches (invalid mode letters, out-of-range ratios, UCC
//! non-digit and over-long data), the 2D command option warnings (BQ/BX/
//! B7/B0), Code 128's unrecognized invocation literals, SHIFT and Subset
//! A paths, the 1D prep edges (Code 93 shift pairs, MSI check modes,
//! UPC-E zero-suppression rules 2-4, EAN padding, Code 11 without check
//! digits) and the Reed-Solomon error branches.
//!
//! Conventions match zpl/tests/barcode.rs: ZPL input via `common::parse_one`,
//! `assert_bars`/`assert_bits`/`assert_single_barcode` helpers, reference
//! sequences hand-computed from the published tables (ISO/IEC 15417 for
//! Code 128, GS1 for EAN/UPC, ANSI/AIM BC5 for Code 93, the MSI
//! conventions). Bar widths are module counts; `assert_bars` scales by
//! the module.

mod common;

use zpl::bc::{barcode_field, Pattern};
use zpl::diag::Severity;
use zpl::ir::Rotation;
use zpl::ir::{BarcodeField, Field};

/// Assert that `pattern` is exactly the `expected` module sequence (each
/// element rendered at `module` dots), alternating black/white from black.
fn assert_bars(pattern: &Pattern, module: u32, expected: &[u32]) {
    let Pattern::Bars(bars) = pattern else {
        panic!("expected Bars, got {pattern:?}");
    };
    let els: Vec<u32> = bars.iter().map(|b| b.width).collect();
    assert_eq!(
        els,
        expected.iter().map(|e| e * module).collect::<Vec<u32>>(),
        "bar widths in dots",
    );
    for (i, b) in bars.iter().enumerate() {
        assert_eq!(b.black, i % 2 == 0, "bar {i} alternates from black");
    }
    assert!(bars.first().is_some_and(|b| b.black));
}

/// Assert `pattern` is exactly the module sequence `bits` (1 = bar, 0 =
/// space, runs merged), each module `module` dots wide.
fn assert_bits(pattern: &Pattern, module: u32, bits: &str) {
    assert!(bits.starts_with('1'), "symbol must start with a bar");
    let bytes = bits.as_bytes();
    let mut runs: Vec<u32> = Vec::new();
    let mut prev = bytes[0];
    let mut len = 1u32;
    for &b in &bytes[1..] {
        if b == prev {
            len += 1;
        } else {
            runs.push(len);
            prev = b;
            len = 1;
        }
    }
    runs.push(len);
    assert_bars(pattern, module, &runs);
}

fn assert_single_barcode(label: &zpl::ir::Label) -> &BarcodeField {
    assert_eq!(
        label.fields.len(),
        1,
        "expected one field, got {:?}",
        label.fields
    );
    match &label.fields[0] {
        Field::Barcode(f) => f,
        other => panic!("expected a barcode field, got {other:?}"),
    }
}

fn has_warning(diags: &zpl::diag::Diagnostics, needle: &str) -> bool {
    diags
        .items
        .iter()
        .any(|d| d.severity == Severity::Warning && d.message.contains(needle))
}

/// The GS1 EAN/UPC element tables (7 modules per digit): L (odd, left),
/// G (even, left) and R (right = bitwise complement of L).
const L: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
    "0110111", "0001011",
];
const G: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001",
    "0001001", "0010111",
];
const R: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100",
    "1001000", "1110100",
];

// --- ^BC Code 128: literal `>x`, SHIFT, Subset A --------------------------

/// An unrecognized `>x` invocation (x not in the guide's Table 2) stays a
/// literal `>`: `>q` encodes as '>' (value 30) then 'q' (value 81).
/// Check: (104 + 30*1 + 81*2) mod 103 = 296 mod 103 = 90.
#[test]
fn bc128_unrecognized_invocation_is_literal() {
    let (label, diags) = common::parse_one(b"^XA^BC^FD>q^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.data, ">q");
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 1, 4, // 104 start B
            2, 1, 2, 1, 2, 3, // 30 '>'
            1, 2, 1, 1, 4, 2, // 81 'q'
            2, 1, 4, 1, 2, 1, // 90 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

/// Subset A (explicit `>9`): a control byte 0x01 maps to value 65 (c+64),
/// a byte >= 0x80 maps to '?' (value 31, the `_` arm). Check:
/// (103 + 65*1 + 31*2) mod 103 = 230 mod 103 = 24. Then a backtick
/// (value 96 in A terminology) lives in Subset B: CODE B followed by
/// value 64; the check is (103 + 100*1 + 64*2) mod 103 = 331 mod 103 = 22.
#[test]
fn bc128_subset_a_controls_and_high_bytes() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>9\x01\xFF^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 4, 1, 2, // 103 start A
            1, 2, 1, 1, 2, 4, // 65 control 0x01
            2, 1, 2, 3, 2, 1, // 31 '?'
            3, 1, 1, 2, 2, 2, // 24 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );

    let (label, _) = common::parse_one(b"^XA^BC^FD>9`^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 4, 1, 2, // 103 start A
            1, 1, 4, 1, 3, 1, // 100 CODE B
            1, 1, 1, 4, 2, 2, // 64 '`'
            2, 2, 3, 1, 1, 2, // 22 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

/// `>4` SHIFT encodes the next data character with the other of A/B's
/// mapping. A digit after SHIFT in Subset B ('1' = 49, in A value 17):
/// (104 + 98*1 + 17*2) mod 103 = 236 mod 103 = 30. A letter after SHIFT
/// ('a' = 97, in A "lives in B": CODE B then value 65): check
/// (104 + 98*1 + 100*2 + 65*3) mod 103 = 597 mod 103 = 82.
#[test]
fn bc128_shift_applies_to_next_char() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>41^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 1, 4, // 104 start B
            4, 1, 1, 3, 1, 1, // 98 SHIFT
            1, 2, 3, 2, 2, 1, // 17 '1' in A
            2, 1, 2, 1, 2, 3, // 30 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );

    let (label, _) = common::parse_one(b"^XA^BC^FD>4a^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 1, 4, // 104 start B
            4, 1, 1, 3, 1, 1, // 98 SHIFT
            1, 1, 4, 1, 3, 1, // 100 CODE B
            1, 2, 1, 1, 2, 4, // 65 'a'
            1, 2, 1, 2, 4, 1, // 82 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

/// SHIFT in Subset C is undefined by the standard; the flag is dropped.
/// `>;12>4x`: pairs 12 in C, SHIFT, then 'x' encodes normally (CODE B +
/// 88). Check: (105 + 12*1 + 98*2 + 100*3 + 88*4) mod 103 = 965 mod 103
/// = 38.
#[test]
fn bc128_shift_in_subset_c_is_dropped() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>;12>4x^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 3, 2, // 105 start C
            1, 1, 2, 2, 3, 2, // 12
            4, 1, 1, 3, 1, 1, // 98 SHIFT (dropped)
            1, 1, 4, 1, 3, 1, // 100 CODE B
            4, 2, 1, 2, 1, 1, // 88 'x'
            1, 3, 2, 3, 1, 1, // 38 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

/// Automatic mode (`m=A`): a leading digit run whose end breaks on a
/// letter still triggers Subset C for the run (`digit_run`'s `_ => break`).
/// `^FD1234ABC^FS` -> start C, pairs 12 34, CODE B, A B C. Check:
/// (105 + 12*1 + 34*2 + 100*3 + 33*4 + 34*5 + 35*6) mod 103 =
/// 997 mod 103 = 70.
#[test]
fn bc128_mode_a_digit_run_breaks_on_letter() {
    let (label, diags) = common::parse_one(b"^XA^BCN,60,N,N,N,A^FD1234ABC^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.model, b'A');
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 3, 2, // 105 start C
            1, 1, 2, 2, 3, 2, // 12
            1, 3, 1, 1, 2, 3, // 34
            1, 1, 4, 1, 3, 1, // 100 CODE B
            1, 1, 1, 3, 2, 3, // 33 'A'
            1, 3, 1, 1, 2, 3, // 34 'B'
            1, 3, 1, 3, 2, 1, // 35 'C'
            1, 1, 2, 4, 1, 2, // 70 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

// --- ^BY / ^BC option edges -----------------------------------------------

/// Barcode commands outside a ^XA..^XZ format are no-ops: no draft opens,
/// nothing is collected, nothing panics (the `draft_mut` None path).
#[test]
fn barcode_outside_format_is_noop() {
    use zpl::cmd::parse_stream;
    use zpl::diag::Diagnostics;
    use zpl::scanner::Scanner;
    use zpl::state::{FormatState, Memory, ParseContext};

    let mut diags = Diagnostics::default();
    let mut mem = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut mem);
    parse_stream(&mut Scanner::new(b"^BCN,60,N,N^FD12^FS"), &mut ctx);
    assert!(
        matches!(ctx.state, FormatState::Outside),
        "no format was opened"
    );
    assert!(ctx.pending_label.is_none());
    assert!(
        !diags.items.iter().any(|d| d.severity != Severity::Warning),
        "only (if any) warnings expected: {diags:?}",
    );
}

/// `^B` with no symbology digit warns and skips - both when the parameter
/// list is empty and when the first segment is empty (`^B,30`).
#[test]
fn barcode_missing_symbology_digit_warns() {
    for src in [&b"^XA^B,30^FD12^FS^XZ"[..], &b"^XA^B^FD12^FS^XZ"[..]] {
        let (label, diags) = common::parse_one(src);
        assert!(
            has_warning(&diags, "missing the symbology digit"),
            "expected warning, got {diags:?}",
        );
        // The failed ^B merges into a plain text field (data still flows),
        // so no *barcode* may open - each of these expects a text field.
        assert!(
            label.fields.iter().all(|f| matches!(f, Field::Text(_))),
            "no barcode field may open for {src:?}: {label:?}",
        );
    }
}

/// ^B4 (Code 49, a 2D/stacked symbology outside this build) warns and
/// skips rather than silently dropping the field.
#[test]
fn barcode_unsupported_digit_warns() {
    let (label, diags) = common::parse_one(b"^XA^B4,30^FD12^FS^XZ");
    assert!(
        has_warning(&diags, "not implemented by this build"),
        "expected warning, got {diags:?}",
    );
    // The skipped symbology leaves the data as a plain text field.
    assert!(
        label.fields.iter().all(|f| matches!(f, Field::Text(_))),
        "no barcode field may open: {label:?}",
    );
}

/// A ^BY ratio value inside the parseable range but outside 2.0..3.0
/// (e.g. 9.0) warns and keeps the previous ratio, never clamping.
#[test]
fn by_ratio_out_of_range_warns_keeps_previous() {
    let (label, diags) = common::parse_one(b"^XA^BY2,9,10^BC^FD12^FS^XZ");
    assert!(
        has_warning(&diags, "ratio"),
        "expected a ratio warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.ratio, 30, "the out-of-range ratio keeps the previous");
    assert_eq!(f.module, 2);
}

/// ^BC mode parameter garbage warns and falls back to N.
#[test]
fn bc_invalid_mode_warns_default_n() {
    let (label, diags) = common::parse_one(b"^XA^BCN,60,N,N,N,X^FD12^FS^XZ");
    assert!(
        has_warning(&diags, "mode"),
        "expected a mode warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.model, b'N');
}

/// ^BC orientation R rotates the field (the `d.rotation` write), and an
/// explicit orientation is honored for the Task 15 commands too.
#[test]
fn bc_and_barcode_1d_explicit_rotation() {
    let (label, diags) = common::parse_one(b"^XA^BCR,60,N,N^FD12^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R90);

    let (label, diags) = common::parse_one(b"^XA^BY1,3,10^B3R,,20,N,N^FDB^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R90);

    let (label, diags) = common::parse_one(b"^XA^BY1,3,10^B3Q,,20,N,N^FDB^FS^XZ");
    assert!(
        has_warning(&diags, "orientation"),
        "expected an orientation warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R0, "invalid orientation falls back");
}

// --- UCC/EAN-128 data-preparation edges ------------------------------------

/// e=Y with non-digit data: the mod-10 check is only defined for digits,
/// so nothing is appended (the `ucc_mod10` None path); the mod-103 check
/// still applies. `ABC` -> check (104 + 33*1 + 34*2 + 35*3) mod 103 =
/// 310 mod 103 = 1.
#[test]
fn bc_ucc_e_nondigit_skips_mod10() {
    let (label, diags) = common::parse_one(b"^XA^BCN,60,N,N,Y^FDABC^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert!(f.ucc);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2, 1, 1, 2, 1, 4, // 104 start B
            1, 1, 1, 3, 2, 3, // 33 'A'
            1, 3, 1, 1, 2, 3, // 34 'B'
            1, 3, 1, 3, 2, 1, // 35 'C'
            2, 2, 2, 1, 2, 2, // 1 check
            2, 3, 3, 1, 1, 1, 2, // stop
        ],
    );
}

/// m=U with more than 19 digits truncates to 19 (guide p.94) - 28 digits
/// become 9 digit pairs plus an odd tail in B: 14 values = 13 x 11 + 13
/// stop modules = 156 modules = 312 dots at module 2.
#[test]
fn bc_ucc_mode_u_truncates_over_19_digits() {
    let (label, diags) =
        common::parse_one(b"^XA^BCN,60,N,N,N,U^FD1234567890123456789012345678^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.model, b'U');
    assert_eq!(
        f.data, "1234567890123456789012345678",
        "the IR keeps the raw field data"
    );
    let Pattern::Bars(bars) = barcode_field(f) else {
        panic!("expected bars")
    };
    assert_eq!(
        bars.iter().map(|b| b.width).sum::<u32>(),
        156 * 2,
        "19 digits = 9 pairs + tail in B: 13 values x 11 + 13 stop modules",
    );
}

/// m=U with non-digit data leaves the data alone (the UCC case mode is
/// for numeric application identifiers): `AB` survives unchanged, so the
/// symbol is start B + A + B + check (205 mod 103 = 102) + stop = 57
/// modules = 114 dots at module 2.
#[test]
fn bc_ucc_mode_u_leaves_nondigits() {
    let (label, diags) = common::parse_one(b"^XA^BCN,60,N,N,N,U^FDAB^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.data, "AB");
    let Pattern::Bars(bars) = barcode_field(f) else {
        panic!("expected bars")
    };
    assert_eq!(bars.iter().map(|b| b.width).sum::<u32>(), 57 * 2);
}

// --- 2D command option branches --------------------------------------------

/// ^B0 Aztec option warnings: ECI Y, Rune (d=300), menu Y and structured
/// append > 1 each warn while the field still parses; an explicit
/// orientation R rotates it.
#[test]
fn aztec_option_warnings_parse() {
    let (label, diags) = common::parse_one(b"^XA^B0R,2,Y,300,Y,2^FDx^FS^XZ");
    for needle in ["ECI", "Rune", "menu symbol", "structured append"] {
        assert!(has_warning(&diags, needle), "missing {needle}: {diags:?}");
    }
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'0');
    assert_eq!(f.rotation, Rotation::R90);
    assert_eq!(f.az_level, 300);
    assert!(f.az_menu);
    assert_eq!(f.az_append, 2);
}

/// ^BX DataMatrix: a legacy convolution quality (100) warns and still
/// encodes ECC 200; explicit columns/rows warn that forcing is
/// unsupported; orientation R rotates.
#[test]
fn datamatrix_legacy_quality_and_forcing_warn() {
    let (label, diags) = common::parse_one(b"^XA^BXR,1,100,9,9^FDx^FS^XZ");
    assert!(
        has_warning(&diags, "legacy convolution"),
        "expected legacy-quality warning, got {diags:?}",
    );
    assert!(
        has_warning(&diags, "columns forcing"),
        "expected columns-forcing warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'X');
    assert_eq!(f.rotation, Rotation::R90);
    assert_eq!(f.dm_quality, 100);

    // A garbage orientation warns and falls back to the current ^FW.
    let (label, diags) = common::parse_one(b"^XA^BXX,1,200^FDx^FS^XZ");
    assert!(
        has_warning(&diags, "orientation"),
        "expected orientation warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.rotation, Rotation::R0);
}

/// ^B7 PDF417: explicit rotation, security level, columns/rows and
/// truncation flow into the draft.
#[test]
fn pdf417_explicit_options_flow() {
    let (label, diags) = common::parse_one(b"^XA^B7B,3,8,10,9,Y^FDx^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'7');
    assert_eq!(f.rotation, Rotation::R270);
    assert_eq!(f.height, 3);
    assert_eq!(f.pdf_security, 8);
    assert_eq!(f.pdf_cols, 10);
    assert_eq!(f.pdf_rows, 9);
    assert!(f.pdf_truncate);
}

/// ^BQ QR with an explicit rotation, model 2, magnification, EC level and
/// mask flows into the draft.
#[test]
fn qr_explicit_rotation_and_mask_flow() {
    let (label, diags) = common::parse_one(b"^XA^BQR,2,5,H,3^FDx^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'Q');
    assert_eq!(f.rotation, Rotation::R90);
    assert_eq!(f.module, 5);
    assert_eq!(f.qr_ec, b'H');
    assert_eq!(f.qr_mask, 3);
}

/// An empty 2D barcode field degrades to an empty pattern (the 2D
/// branch of `barcode_field`), warned at ^FS.
#[test]
fn empty_2d_barcode_data_is_empty_pattern() {
    let (label, diags) = common::parse_one(b"^XA^BQN,2,2,Q^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected a warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert!(matches!(barcode_field(f), Pattern::Bars(b) if b.is_empty()));
}

// --- Task 15 prep edges -----------------------------------------------------

/// ^BM e=A appends no check digit (the `_` arm of the MSI mode match);
/// e=C appends two Mod-10 digits; e=D appends Mod-11 then Mod-10, and a
/// Mod-11 result of 10 prints as the literal two-digit "10".
#[test]
fn bm_msi_check_mode_edges() {
    // e=A: bare 123.
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BMN,A,20,N,N^FD123^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.check, b'A');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            2, 1, // start
            1, 2, 1, 2, 1, 2, 2, 1, // 1
            1, 2, 1, 2, 2, 1, 1, 2, // 2
            1, 2, 1, 2, 2, 1, 2, 1, // 3
            1, 2, 1, // stop
        ],
    );

    // e=C: 1234567 -> two Mod-10 digits: 4 then 1 -> 123456741.
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BMN,C,20,N,N^FD1234567^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.check, b'C');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            2, 1, // start
            1, 2, 1, 2, 1, 2, 2, 1, // 1
            1, 2, 1, 2, 2, 1, 1, 2, // 2
            1, 2, 1, 2, 2, 1, 2, 1, // 3
            1, 2, 2, 1, 1, 2, 1, 2, // 4
            1, 2, 2, 1, 1, 2, 2, 1, // 5
            1, 2, 2, 1, 2, 1, 1, 2, // 6
            1, 2, 2, 1, 2, 1, 2, 1, // 7
            1, 2, 2, 1, 1, 2, 1, 2, // 4 (mod 10)
            1, 2, 1, 2, 1, 2, 2, 1, // 1 (mod 10)
            1, 2, 1, // stop
        ],
    );

    // e=D on "6": Mod-11 = 10 -> the literal pair "10", then Mod-10 of
    // "610" = 6 -> 6106.
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BMN,D,20,N,N^FD6^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.check, b'D');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            2, 1, // start
            1, 2, 2, 1, 2, 1, 1, 2, // 6
            1, 2, 1, 2, 1, 2, 2, 1, // 1
            1, 2, 1, 2, 1, 2, 1, 2, // 0
            1, 2, 2, 1, 2, 1, 1, 2, // 6 (mod 10)
            1, 2, 1, // stop
        ],
    );
}

/// A garbage ^BM check-digit mode warns and falls back to B.
#[test]
fn bm_invalid_mode_warns() {
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BMN,Z,20,N,N^FD123^FS^XZ");
    assert!(
        has_warning(&diags, "check digit mode"),
        "expected a mode warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!(f.check, b'B');
}

/// ^BA Code 93 full-ASCII substitutions with uppercase letters emit the
/// ($) (%) (/) (+) shift pairs; a shift character NOT followed by an
/// uppercase letter expands through the plain table instead. Payload
/// `&A'B(C)D&1` expands to 11 values (+ C + K = 13 symbols): 4 pairs of 2
/// (the &/&/() shifts) + 3 plain (the trailing & falls back to EXT93, then
/// '1'), so the symbol is 13 chars plus start/stop and termination bar,
/// giving 13*9 + 2*9 + 1 = 136 modules, 91 bars. A doubled or mis-expanded
/// pair would change the count.
#[test]
fn ba_code93_shift_pairs() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^BAN,20^FD&A'B(C)D&1^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    let Pattern::Bars(bars) = barcode_field(f) else {
        panic!("expected bars")
    };
    assert_eq!(
        bars.len(),
        91,
        "13 chars x 6 elements + star x2 + termination bar"
    );
    assert_eq!(
        bars.iter().map(|b| b.width).sum::<u32>(),
        136,
        "13 chars at 9 modules + 2 stars + termination module",
    );
}

/// ^BK garbage in the (diagnostics-only) e parameter and the k start
/// character warns and falls back; the narrow/wide bar encoding is
/// untouched.
#[test]
fn bk_invalid_params_warn() {
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BKN,Z,20,N,N,Z,A^FD1234^FS^XZ");
    assert!(
        has_warning(&diags, "check digit"),
        "expected an e warning, got {diags:?}",
    );
    assert!(
        has_warning(&diags, "start character"),
        "expected a k warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_eq!((f.check, f.start, f.stop), (b'N', b'A', b'A'));
}

/// ^B9's e parameter is diagnostic-only (the bars always carry the
/// standard's check digit); garbage warns and keeps the default Y.
#[test]
fn b9_invalid_print_check_warns() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^B9N,20,N,N,Z^FD1200000123^FS^XZ");
    assert!(
        has_warning(&diags, "print check digit"),
        "expected a warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert!(
        matches!(barcode_field(f), Pattern::Bars(b) if !b.is_empty()),
        "valid UPC-E data still encodes",
    );
}

/// UPC-E zero-suppression rules 2, 3 and 4 (guide p.86) plus the three
/// matching `upce_expand` decode arms:
///   rule 2: mfr 12300 product 00045 -> digits 123453, check 1, parity
///           row 1 = GGLGLL;
///   rule 3: mfr 12340 product 00005 -> digits 123454, check 3, parity
///           row 3 = GGLLLG;
///   rule 4: mfr 12345 product 00007 -> digits 123457, check 2, parity
///           row 2 = GGLLGL.
#[test]
fn b9_upce_suppression_rules_2_3_4() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^B9N,20^FD1230000045^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    let codes = [G[1], G[2], L[3], G[4], L[5], L[3]];
    let mut bits = String::from("101");
    for c in codes {
        bits.push_str(c);
    }
    bits.push_str("010101");
    assert_bits(&barcode_field(f), 1, &bits);

    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^B9N,20^FD1234000005^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    let codes = [G[1], G[2], L[3], L[4], L[5], G[4]];
    let mut bits = String::from("101");
    for c in codes {
        bits.push_str(c);
    }
    bits.push_str("010101");
    assert_bits(&barcode_field(f), 1, &bits);

    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^B9N,20^FD1234500007^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    let codes = [G[1], G[2], L[3], L[4], G[5], L[7]];
    let mut bits = String::from("101");
    for c in codes {
        bits.push_str(c);
    }
    bits.push_str("010101");
    assert_bits(&barcode_field(f), 1, &bits);
}

/// EAN-13 with fewer than 12 data digits left-pads with zeros (guide
/// ^BE): `123` -> 000000000123, first digit 0 (parity LLLLLL), left half
/// 000000, right half 00123, check (weights 3,1 from the right) 6.
#[test]
fn be_ean13_short_data_pads_left() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^BEN,20^FD123^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    let mut bits = String::from("101");
    for d in b"000000" {
        bits.push_str(L[(d - b'0') as usize]);
    }
    bits.push_str("01010");
    for d in b"00123" {
        bits.push_str(R[(d - b'0') as usize]);
    }
    bits.push_str(R[6]);
    bits.push_str("101");
    assert_bits(&barcode_field(f), 1, &bits);
}

/// A hand-built Code 11 field whose check option is neither 1 nor 2 (the
/// parser always produces those; only hand-built IR can carry other
/// values) encodes without check digits - the `prepare_code11` guard.
#[test]
fn b1_handbuilt_ir_without_check_digits() {
    let f = BarcodeField {
        x: 0,
        y: 0,
        origin: zpl::ir::OriginMode::Block,
        symbology: b'1',
        data: "12345".into(),
        module: 1,
        ratio: 20,
        height: 60,
        line: b'N',
        interpret: false,
        ucc: false,
        model: b'N',
        check: b'N',
        start: b'A',
        stop: b'A',
        qr_ec: b'Q',
        qr_mask: 0,
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
        md_mode: 4,
        md_mag: 1,
        font: *b"A ",
        font_height: 9,
        font_width: 5,
        font_name: None,
        rotation: Rotation::R0,
    };
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(&f),
        1,
        &[
            // * start = 112211
            1, 1, 2, 2, 1, 1,
            // 1 2 3 4 5 (wide 2)
            2, 1, 1, 1, 2, 1,
            1, 2, 1, 1, 2, 1,
            2, 2, 1, 1, 1, 1,
            1, 1, 2, 1, 2, 1,
            2, 1, 2, 1, 1, 1,
            // * stop (first five elements)
            1, 1, 2, 2, 1,
        ],
    );
}

/// Codabar rejects a character outside its set at encode time: `!` is
/// not in 0-9-$:/.+ABCD. The parse already warned; the field renders
/// empty.
#[test]
fn bk_invalid_data_warns_and_skips() {
    let (label, diags) = common::parse_one(b"^XA^BY1,2,20^BKN,,20,N,N^FD12!4^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected a warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert!(
        matches!(barcode_field(f), Pattern::Bars(b) if b.is_empty()),
        "invalid Codabar data must degrade to an empty pattern",
    );
}

// --- direct encode() entry points (paths the parser cannot reach) ---------

/// `bc::encode` for ^B5 Planet with empty data reaches the encoder's
/// required-data error (the parser intercepts empty fields before this).
#[test]
fn encode_planet_empty_data_errs() {
    assert!(zpl::bc::encode(b'5', b"", 2, 30, 60, b'N', 0).is_err());
}

/// Code 11 with a character outside 0-9/- errs from the direct entry.
#[test]
fn encode_code11_invalid_data_errs() {
    assert!(zpl::bc::encode(b'1', b"12A", 2, 30, 60, b'N', 0).is_err());
}

// --- Reed-Solomon codec error branches -------------------------------------

use zpl::reed_solomon::{Field as RsField, Rs};

/// The Debug impl prints field and nsym.
#[test]
fn rs_debug_format() {
    let rs = Rs::new(RsField::Binary(0x12d), 7, 1).unwrap();
    assert_eq!(format!("{rs:?}"), "Rs { field: Binary(301), nsym: 7 }",);
}

/// `new` rejects nsym = 0 and nsym beyond the field order (255 for
/// GF(256), 928 for GF(929)).
#[test]
fn rs_new_rejects_bad_nsym() {
    assert!(Rs::new(RsField::Binary(0x11d), 0, 0).is_err());
    assert!(Rs::new(RsField::Binary(0x11d), 256, 0).is_err());
    assert!(Rs::new(RsField::Prime(929), 929, 1).is_err());
}

/// `encode` rejects empty data.
#[test]
fn rs_encode_empty_data_errs() {
    let rs = Rs::new(RsField::Binary(0x11d), 10, 0).unwrap();
    assert!(rs.encode(&[]).is_err());
}

/// All-zero data drives the LFSR's canceling branch (m == 0: the
/// remainder shifts instead of multiplying): the EC codewords are all
/// zero, in both GF(2^8) and GF(929).
#[test]
fn rs_encode_zero_data_shift_branch() {
    let rs = Rs::new(RsField::Binary(0x11d), 7, 0).unwrap();
    assert_eq!(rs.encode(&[0, 0, 0]).unwrap(), vec![0; 7]);

    let rs = Rs::new(RsField::Prime(929), 2, 1).unwrap();
    assert_eq!(rs.encode(&[0, 0]).unwrap(), vec![0, 0]);
}

/// At the full multiplicative order (nsym = 928 = p - 1) the PDF417
/// generator polynomial has vanishing coefficients (the mul_alog
/// zero-operand guard): `new` still succeeds and encodes.
#[test]
fn rs_pdf417_full_order_zero_gen_coefficients() {
    let rs = Rs::new(RsField::Prime(929), 928, 1).unwrap();
    assert_eq!(rs.encode(&[1, 2, 3]).unwrap().len(), 928);
}
