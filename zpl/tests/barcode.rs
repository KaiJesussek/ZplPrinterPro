//! Integration tests for the Task 9 barcode framework: ^BY defaults, ^BC
//! parsing, the Code 128 encoder (ISO/IEC 15417) and the ^BC UCC/EAN-128
//! options.
//!
//! Bar-sequence references are hand-computed here from the published
//! ISO/IEC 15417 value table (the public table the encoder also implements,
//! reproduced in every Code 128 reference): each value 0-106 has a
//! six-element bar/space width pattern summing to 11 modules; the stop
//! pattern is the seven-element 2331112 (13 modules). Check digit:
//! (start_value + sum of value_i x position) mod 103, positions counted from 1.

mod common;

use zpl::bc::{barcode_field, Pattern};
use zpl::diag::Severity;
use zpl::ir::{BarcodeField, Field};
use zpl::ir::Rotation;

/// Assert that `pattern` is exactly the `expected` module sequence (each
/// element rendered at `module` dots), alternating black/white from black -
/// the Code 128 bar/space rhythm.
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
        assert_eq!(b.width % module, 0, "bar {i} width is a module multiple");
    }
    assert!(bars.first().is_some_and(|b| b.black));
}

fn assert_single_barcode(label: &zpl::ir::Label) -> &BarcodeField {
    assert_eq!(label.fields.len(), 1, "expected one field, got {:?}", label.fields);
    match &label.fields[0] {
        Field::Barcode(f) => f,
        other => panic!("expected a barcode field, got {other:?}"),
    }
}

// --- Code 128: automatic subset C and check digit -------------------------

/// `^FO0,0^BY2,3,60^BCN,60,N,N^FD12345678^FS`: eight digits -> automatic
/// Subset C (guide: a string of four or more digits causes an automatic
/// shift to Subset C), start C 105 and four digit pairs. Reference
/// (ISO/IEC 15417):
///
///   value 105 (start C) 211232
///   value 12             112232
///   value 34             131123
///   value 56             331121
///   value 78             241112
///   check = (105 + 12*1 + 34*2 + 56*3 + 78*4) mod 103 = 665 mod 103 = 47
///   value 47             133121
///   stop                 2331112
#[test]
fn bc128_auto_switches_and_checks() {
    let (label, diags) = common::parse_one(b"^XA^FO0,0^BY2,3,60^BCN,60,N,N^FD12345678^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(
        f,
        &BarcodeField {
            x: 0,
            y: 0,
            symbology: b'C',
            data: "12345678".into(),
            module: 2,
            ratio: 30,
            height: 60,
            line: b'N',
            interpret: false,
            ucc: false,
            model: b'N',
            // No ^A in the format: the draft's ^CF power-up font (A, 9x5).
            font: [b'A', b' '],
            font_height: 9,
            font_width: 5,
            font_name: None,
            rotation: Rotation::R0,
        },
    );
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,3,1,1,2,1, // 56
            2,4,1,1,1,2, // 78
            1,3,3,1,2,1, // 47
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// `^FD>8ABC^FS`: the guide's `>8` invocation code is FNC1 (value 102,
/// Table 2 p.96 - the plan sketch said `>1`; the guide wins). Data starts
/// with a letter, so the default start is B (104). Check digit:
/// (104 + 102*1 + 33*2 + 34*3 + 35*4) mod 103 = 514 mod 103 = 102.
#[test]
fn bc128_fnc_escapes() {
    let (label, _) = common::parse_one(b"^XA^BCN,60,N,N^FD>8ABC^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,1,4, // 104 start B
            4,1,1,1,3,1, // 102 FNC1
            1,1,1,3,2,3, // 33 'A'
            1,3,1,1,2,3, // 34 'B'
            1,3,1,3,2,1, // 35 'C'
            4,1,1,1,3,1, // 102 check
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// Literal `>`, `~`, `=` need the invocation escapes `>0`, `>=`, `><`
/// (guide Table 2): `>0` emits value 30, `>=` value 94, `><` value 62.
/// Check: (104 + 30*1 + 94*2 + 62*3) mod 103 = 508 mod 103 = 96.
#[test]
fn bc128_special_char_escapes() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>0>=><^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,1,4, // 104
            2,1,2,1,2,3, // 30
            1,3,1,1,4,1, // 94
            4,3,1,1,1,1, // 62
            1,1,4,3,1,1, // 96
            2,3,3,1,1,1,2, // stop
        ],
    );
}

// --- ^BY defaults and ^BC parameter flow -----------------------------------

/// Guide p.148: ^BY module width 1..10 power-up 2, ratio 2.0..3.0 in 0.1
/// steps power-up 3.0, height power-up 10. ^BC defaults (p.94): o = ^FW,
/// h = ^BY height, f = Y, g = N, e = N, m = N.
#[test]
fn by_defaults_apply_to_barcode_field() {
    let (label, diags) = common::parse_one(b"^XA^BC^FD12345678^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!((f.module, f.ratio, f.height, f.line), (2, 30, 10, b'Y'));
    assert!(!f.interpret && !f.ucc);
    assert_eq!(f.model, b'N');
}

#[test]
fn by_values_flow_and_bc_height_overrides() {
    // ^BY5,2.5,100 then ^BC height 200 overrides.
    let (label, diags) = common::parse_one(b"^XA^BY5,2.5,100^BCN,200,N,N^FD12^FS^XZ");
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!((f.module, f.ratio, f.height), (5, 25, 200));
    // ^BC height missing -> ^BY height.
    let (label, _) = common::parse_one(b"^XA^BY5,2.5,100^BC^FD12^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.height, 100);
    // A fresh parse (fresh PrinterMode) -> power-up defaults again.
    let (label, _) = common::parse_one(b"^XA^BC^FD12^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!((f.module, f.ratio, f.height), (2, 30, 10));
}

/// Malformed ^BY/^BC parameters warn and fall back, never panic.
#[test]
fn malformed_barcode_params_warn_not_panic() {
    let (label, diags) = common::parse_one(b"^XA^BY9,9x,0^BCQ,abc,N,N^FD12^FS^XZ");
    let f = assert_single_barcode(&label);
    // w=9 valid; r garbage -> default 30; h=0 out of range -> default 10;
    // ^BC o garbage -> ^FW; h garbage -> ^BY height.
    assert_eq!((f.module, f.ratio, f.height), (9, 30, 10));
    assert!(!diags.is_empty(), "malformed params must warn");
}

// --- subset invocation codes ----------------------------------------------

/// Mid-stream subset switching via the guide's invocation codes: `>5` =
/// CODE C (99), `>7` = CODE A (101). `^FD>:#>512`: start B, '#' (value 3
/// in B), CODE C, pair 12 - without the explicit >5 the 2-digit run stays
/// in B. Check: (104 + 3*1 + 99*2 + 12*3) mod 103 = 341 mod 103 = 32.
#[test]
fn bc128_subset_invocations() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>:#>512^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,1,4, // 104
            1,2,1,2,2,3, // 3 '#'
            1,1,3,1,4,1, // 99 CODE C
            1,1,2,2,3,2, // 12
            2,3,2,1,2,1, // 32
            2,3,3,1,1,1,2, // stop
        ],
    );

    // `>;1234>7ABC`: start C (also auto: run of 4 digits), pairs 12 34,
    // CODE A (101), then letters in Subset A - the guide's Table 3 (p.97,
    // matching ISO/IEC 15417) maps A->33, B->34, C->35, distinct from B's
    // 1,2,3. Check (hand-computed from the standard's formula):
    // (105 + 12*1 + 34*2 + 101*3 + 33*4 + 34*5 + 35*6) mod 103
    // = (105 + 12 + 68 + 303 + 132 + 170 + 210) = 1000 mod 103 = 73.
    let (label, _) = common::parse_one(b"^XA^BC^FD>;1234>7ABC^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,1,1,1,4,1, // 101 CODE A
            1,1,1,3,2,3, // 33 'A'
            1,3,1,1,2,3, // 34 'B'
            1,3,1,3,2,1, // 35 'C'
            1,4,2,1,1,2, // 73
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// Explicit start code `>;` right at the beginning selects Subset C even for
/// a short digit payload (start codes are only meaningful in front).
#[test]
fn bc128_explicit_start_code() {
    let (label, _) = common::parse_one(b"^XA^BC^FD>;12^FS^XZ");
    let f = assert_single_barcode(&label);
    // (105 + 12*1) mod 103 = 117 mod 103 = 14.
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,2,2,2,3,1, // 14: 10011001110 -> 1,2,2,2,3,1
            2,3,3,1,1,1,2, // stop
        ],
    );
}

// --- empty data ------------------------------------------------------------

/// An empty barcode field warns at ^FS and encodes to an empty pattern -
/// never a panic (global constraint).
#[test]
fn empty_barcode_data_warns() {
    let (label, diags) = common::parse_one(b"^XA^BCN,60,N,N^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected a warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert!(f.data.is_empty());
    assert!(matches!(barcode_field(f), Pattern::Bars(b) if b.is_empty()));
}

// --- UCC/EAN-128 options ---------------------------------------------------

/// ^BC e=Y: the guide pairs the mod-10 UCC check digit with the always-on
/// mod-103 check. "12345678" is all digits -> append the EAN-style mod-10
/// check digit (weights 3,1,3,... from the rightmost digit):
/// 8*3+7*1+6*3+5*1+4*3+3*1+2*3+1*1 = 76 -> (10 - 76 mod 10) mod 10 = 4.
/// The 9-digit payload starts Subset C with the odd tail digit in B:
/// [105, 12, 34, 56, 78, 100, 20, check, stop], check =
/// (105 + 12 + 68 + 168 + 312 + 500 + 120) mod 103 = 1285 mod 103 = 49.
#[test]
fn bc_ucc_check_digit_appends_mod10() {
    let (label, _) = common::parse_one(b"^XA^BCN,60,N,N,Y^FD12345678^FS^XZ");
    let f = assert_single_barcode(&label);
    assert!(f.ucc);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,3,1,1,2,1, // 56
            2,4,1,1,1,2, // 78 -> 10001101110 = 1,3,2,1,1,3
            1,1,4,1,3,1, // 100 CODE B
            2,2,1,2,3,1, // 20 '4'
            2,1,1,3,3,1, // 49
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// ^BC m=D (UCC/EAN mode): the guide strips parentheses and spaces and
/// starts the code with FNC1. "(01)123456789012345678" encodes as FNC1
/// followed by ten Subset-C pairs: [105, 102, 01 12 34 56 78 90 12 34 56 78,
/// check, stop], check = (105 + 102*1 + 1*2 + 12*3 + 34*4 + 56*5 + 78*6 +
/// 90*7 + 12*8 + 34*9 + 56*10 + 78*11) mod 103 = 3579 mod 103 = 77.
#[test]
fn bc_ucc_mode_d_strips_and_prepends_fnc1() {
    let (label, _) = common::parse_one(b"^XA^BC,,N,N,N,D^FD(01)123456789012345678^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.model, b'D');
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            4,1,1,1,3,1, // 102 FNC1
            2,2,2,1,2,2, // 01
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,3,1,1,2,1, // 56
            2,4,1,1,1,2, // 78
            2,1,4,1,2,1, // 90
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,3,1,1,2,1, // 56
            2,4,1,1,1,2, // 78
            4,1,3,1,1,1, // 77
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// ^BC m=U (UCC case mode): the guide pads fewer than 19 digits with
/// trailing zeros. "1234" -> "1234000000000000000": Subset C pairs then the
/// odd tail digit in B - [105, 12 34 00x7, 100, 16, check, stop], check =
/// (105 + 12 + 68 + 100*10 + 16*11) mod 103 = 1361 mod 103 = 22.
#[test]
fn bc_ucc_case_mode_pads_to_19_digits() {
    let (label, _) = common::parse_one(b"^XA^BCN,,N,N,N,U^FD1234^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.model, b'U');
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            2,1,2,2,2,2, // 00 x7
            2,1,2,2,2,2,
            2,1,2,2,2,2,
            2,1,2,2,2,2,
            2,1,2,2,2,2,
            2,1,2,2,2,2,
            2,1,2,2,2,2,
            1,1,4,1,3,1, // 100 CODE B
            1,2,3,1,2,2, // 16 '0'
            2,2,3,1,1,2, // 22
            2,3,3,1,1,1,2, // stop
        ],
    );
}

// --- encode() entry points -------------------------------------------------

/// Direct `encode` entry: unknown symbology -> Err; empty data -> Err;
/// non-`'C'` letters are Task 15's territory.
#[test]
fn encode_rejects_unknown_symbology_and_empty() {
    assert!(zpl::bc::encode(b'3', b"ABC", 2, 30, 60, b'N').is_err());
    assert!(zpl::bc::encode(b'C', b"", 2, 30, 60, b'N').is_err());
    assert!(zpl::bc::encode(b'C', b"ABC", 2, 30, 60, b'N').is_ok());
}

/// Non-ASCII bytes substitute '?' (Code 128 is 7-bit); high bytes never
/// panic the encoder.
#[test]
fn encode_substitutes_high_bytes() {
    let (label, _) = common::parse_one(b"^XA^BC^FDAB\xFF\xE9^FS^XZ");
    let f = assert_single_barcode(&label);
    assert!(matches!(barcode_field(f), Pattern::Bars(b) if !b.is_empty()));
}

/// Reviewer repro: a non-digit byte >= 0x80 while Subset C is active must
/// not index WIDTHS out of bounds (old code emitted c-32 = 107+ -> panic).
/// ^CI13 (CP850) keeps the byte single-byte (0x8B -> i, U+00EF) so the
/// encoder still sees a value >= 0x80, which becomes '?' (B value 31,
/// pattern 212321) after a CODE B switch:
/// [105, 12, 34, 56, 78, 100, 31, check, stop], check =
/// (105 + 12 + 68 + 168 + 312 + 500 + 186) mod 103 = 1351 mod 103 = 12.
/// The parser also warns about the unencodable byte.
#[test]
fn bc_high_byte_in_subset_c_never_panics() {
    let (label, diags) = common::parse_one(b"^XA^CI13^BC^FD12345678\x8B^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected an unencodable-byte warning, got {diags:?}",
    );
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,3,2, // 105
            1,1,2,2,3,2, // 12
            1,3,1,1,2,3, // 34
            3,3,1,1,2,1, // 56
            2,4,1,1,1,2, // 78
            1,1,4,1,3,1, // 100 CODE B
            2,1,2,3,2,1, // 31 '?'
            1,1,2,2,3,2, // 12 (check)
            2,3,3,1,1,1,2, // stop
        ],
    );
}

/// ^FH hex escapes apply to barcode data too (the shared `^FD` data path):
/// `_31_32` unescapes to `"12"`. A 2-digit run is below the auto-C
/// threshold (four or more digits), so the default start B applies.
/// Check: (104 + 17*1 + 18*2) mod 103 = 157 mod 103 = 54.
#[test]
fn bc_fh_hex_escapes_apply_to_barcode_data() {
    let (label, _) = common::parse_one(b"^XA^BC^FH^FD_31_32^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.data, "12");
    assert_bars(
        &barcode_field(f),
        2,
        &[
            2,1,1,2,1,4, // 104 start B
            1,2,3,2,2,1, // 17 '1'
            2,2,3,2,1,1, // 18 '2'
            3,1,1,1,2,3, // 54
            2,3,3,1,1,1,2, // stop
        ],
    );
}
