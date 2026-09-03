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
/// Task 15's 1D symbologies (here ^B3 Code 39) encode.
#[test]
fn encode_rejects_unknown_symbology_and_empty() {
    assert!(zpl::bc::encode(b'4', b"ABC", 2, 30, 60, b'N').is_err()); // ^B4 is 2D Code 49 - native
    assert!(zpl::bc::encode(b'C', b"", 2, 30, 60, b'N').is_err());
    assert!(zpl::bc::encode(b'C', b"ABC", 2, 30, 60, b'N').is_ok());
    assert!(zpl::bc::encode(b'3', b"ABC123", 2, 30, 60, b'N').is_ok());
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

// --- Task 15: 1D symbologies ^B1 ^B2 ^B3 ^B8 ^B9 ^BA ^BK ^BE ^BU ^BM ----
//
// References are hand-composed here from the published standards (verified
// against ISO/IEC 16388 Code 39, ANSI/AIM BC5 Code 93, ISO/IEC 16390 I2of5,
// GS1 EAN/UPC, AIM BC3 Codabar and the Code 11/MSI conventions) - the same
// tables the encoder implements, written out independently in the test.
// Mnemonic mapping from the ZPL II guide (Task 15 report): ^B1=Code 11,
// ^B2=Interleaved 2 of 5, ^B3=Code 39, ^B8=EAN-8, ^B9=UPC-E, ^BA=Code 93,
// ^BK=Codabar, ^BE=EAN-13, ^BU=UPC-A, ^BM=MSI.

/// The GS1 EAN/UPC element tables (7 modules per digit): L (odd, left),
/// G (even, left) and R (right = bitwise complement of L).
const L: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011", "0110001",
    "0101111", "0111011", "0110111", "0001011",
];
const G: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101", "0111001",
    "0000101", "0010001", "0001001", "0010111",
];
const R: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100", "1001110",
    "1010000", "1000100", "1001000", "1110100",
];

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

// --- ^B3 Code 39 (ISO/IEC 16388) ------------------------------------------

/// `^FO0,0^BY1,3,10^B3N,Y^FDABC123^FS`: e=Y appends the Mod-43 check digit;
/// values A=10 B=11 C=12 1 2 3 sum to 39, 39 mod 43 = 39 = '$' (guide Mod
/// 43 p.1592). Each character is nine elements (three wide) plus an
/// inter-character narrow gap; '*' is generated around the data. Module 1,
/// ratio 3.0 -> wide = trunc(1*3) = 3 dots.
#[test]
fn b3_code39_mod43_and_ratio() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,10^B3N,Y,20,N,N^FDABC123^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'3');
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // * start (incl. trailing narrow gap)
            1, 3, 1, 1, 3, 1, 3, 1, 1, 1,
            // A
            3, 1, 1, 1, 1, 3, 1, 1, 3, 1,
            // B
            1, 1, 3, 1, 1, 3, 1, 1, 3, 1,
            // C
            3, 1, 3, 1, 1, 3, 1, 1, 1, 1,
            // 1
            3, 1, 1, 3, 1, 1, 1, 1, 3, 1,
            // 2
            1, 1, 3, 3, 1, 1, 1, 1, 3, 1,
            // 3
            3, 1, 3, 3, 1, 1, 1, 1, 1, 1,
            // $ (check)
            1, 3, 1, 3, 1, 3, 1, 1, 1, 1,
            // * stop (no trailing gap)
            1, 3, 1, 1, 3, 1, 3, 1, 1,
        ],
    );
}

/// The ^BY ratio table (guide p.148, Table 6) transcribed: the printed
/// wide/narrow ratio is trunc(w*r)/w - at module 3 the requested 2.5
/// yields wide = trunc(3*2.5) = 7 dots (2.3:1), not 8.
#[test]
fn b3_code39_by_ratio_table_wide_dots() {
    let (label, _) = common::parse_one(b"^XA^BY3,2.5,20^B3N,N^FDB^FS^XZ");
    let f = assert_single_barcode(&label);
    let Pattern::Bars(bars) = barcode_field(f) else { panic!("expected bars") };
    // 'B' = 001001001 -> N N W N N W N N W; * B * with 3-wide gaps.
    let widths: Vec<u32> = bars.iter().map(|b| b.width).collect();
    assert_eq!(
        widths,
        [
            3, 7, 3, 3, 7, 3, 7, 3, 3, 3, // * + gap
            3, 3, 7, 3, 3, 7, 3, 3, 7, 3, // B + gap
            3, 7, 3, 3, 7, 3, 7, 3, 3, // * stop
        ],
    );
}

// --- ^B2 Interleaved 2 of 5 (ISO/IEC 16390) -------------------------------

/// `1234` (no check): start NNNN, digit pairs interleaved (bars from the
/// first digit, spaces from the second), stop WNN. Wide/narrow 3:1.
#[test]
fn b2_interleaved_2of5_pairs_and_guards() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,10^B2N,20,N,N^FD1234^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'2');
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // start NNNN
            1, 1, 1, 1,
            // pair 1,2: bars(1)=31113, spaces(2)=13113
            3, 1, 1, 3, 1, 1, 1, 1, 3, 3,
            // pair 3,4: bars(3)=33111, spaces(4)=11313
            3, 1, 3, 1, 1, 3, 1, 1, 1, 3,
            // stop WNN
            3, 1, 1,
        ],
    );
}

/// The guide (p.69): "The printer automatically adds a leading 0 (zero) if
/// an odd number of digits is received." `123` -> `0123`.
#[test]
fn b2_interleaved_2of5_odd_count_pads_leading_zero() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,10^B2N,20,N,N^FD123^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        1,
        &[
            1, 1, 1, 1,
            // pair 0,1: bars(0)=11331, spaces(1)=31113
            1, 3, 1, 1, 3, 1, 3, 1, 1, 3,
            // pair 2,3
            1, 3, 3, 3, 1, 1, 1, 1, 3, 1,
            3, 1, 1,
        ],
    );
}

/// `^B2 e=Y` appends the Mod-10 check digit (guide p.68 -> Mod 10 p.1591,
/// the GS1 weighting 3,1,... from the right). `1234` (even) gets a leading
/// zero first: 01234 -> 4*3+3*1+2*3+1*1+0*3 = 22 -> check 8.
#[test]
fn b2_interleaved_2of5_mod10_check_digit() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,10^B2N,20,N,N,Y^FD1234^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_bars(
        &barcode_field(f),
        1,
        &[
            1, 1, 1, 1,
            // pair 0,1
            1, 3, 1, 1, 3, 1, 3, 1, 1, 3,
            // pair 2,3
            1, 3, 3, 3, 1, 1, 1, 1, 3, 1,
            // pair 4,8: bars(4)=11313, spaces(8)=31131
            1, 3, 1, 1, 3, 1, 1, 3, 3, 1,
            3, 1, 1,
        ],
    );
}

// --- ^BE EAN-13 ------------------------------------------------------------

/// `4006381333931` (the classic example; check digit 1). First digit 4
/// picks the LGLLGG parity row; right half uses R-codes.
#[test]
fn be_ean13_parity_and_check_digit() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,30^BEN,30^FD400638133393^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'E');
    let mut bits = String::from("101");
    // Parity row for first digit 4 (GS1 Table): LGLLGG.
    for (d, p) in b"006381".iter().zip("LGLLGG".bytes()) {
        bits.push_str(if p == b'L' { L[(d - b'0') as usize] } else { G[(d - b'0') as usize] });
    }
    bits.push_str("01010");
    for d in b"333931".iter() {
        bits.push_str(R[(d - b'0') as usize]);
    }
    bits.push_str("101");
    assert_bits(&barcode_field(f), 1, &bits);
}

// --- ^B8 EAN-8 -------------------------------------------------------------

/// `9638507` -> check digit 4: 7*3+0+5*3+8+3*3+6+9*3 = 86 -> 10-6 = 4.
#[test]
fn b8_ean8_check_and_structure() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,30^B8N,30^FD9638507^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'8');
    let mut bits = String::from("101");
    for d in b"9638".iter() {
        bits.push_str(L[(d - b'0') as usize]);
    }
    bits.push_str("01010");
    for d in b"5074".iter() {
        bits.push_str(R[(d - b'0') as usize]);
    }
    bits.push_str("101");
    assert_bits(&barcode_field(f), 1, &bits);
}

// --- ^BU UPC-A -------------------------------------------------------------

/// `03600029145` -> check digit 2 (5*3+4+1*3+9+2*3+0+0*3+0+6*3+3+0*3 = 58
/// -> 60-58). Left half L-codes, right half R-codes.
#[test]
fn bu_upc_a_structure() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,30^BUN,30^FD03600029145^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'U');
    let mut bits = String::from("101");
    for d in b"036000".iter() {
        bits.push_str(L[(d - b'0') as usize]);
    }
    bits.push_str("01010");
    for d in b"291452".iter() {
        bits.push_str(R[(d - b'0') as usize]);
    }
    bits.push_str("101");
    assert_bits(&barcode_field(f), 1, &bits);
}

// --- ^B9 UPC-E -------------------------------------------------------------

/// 10-digit input (mfr `12000`, product `00123`): zero-suppression case 1
/// (guide p.86) -> UPC-E digits `121230`; the check digit comes from the
/// expanded 11-digit UPC-A 012000000123 -> 21 -> 9; number-system-0 parity
/// row 9 = EOOEOE. Symbol: 101 + six digits + 010101.
#[test]
fn b9_upc_e_zero_suppression_and_parity() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,30^B9N,30^FD1200000123^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'9');
    // UPC-E digits 1,2,1,2,3,0 under parity E O O E O E.
    let codes = [G[1], L[2], L[1], G[2], L[3], G[0]];
    let mut bits = String::from("101");
    for c in codes {
        bits.push_str(c);
    }
    bits.push_str("010101");
    assert_bits(&barcode_field(f), 1, &bits);
}

// --- ^BA Code 93 -----------------------------------------------------------

/// `ABCD` values 10,11,12,13: C = 13*1+12*2+11*3+10*4 = 110 mod 47 = 16
/// ('G'); K = 16*1+13*2+12*3+11*4+10*5 = 172 mod 47 = 31 ('V'). The symbol
/// is * + data + C + K + * + a one-module termination bar.
#[test]
fn ba_code93_two_check_chars() {
    let (label, _) = common::parse_one(b"^XA^BY1,3,30^BAN,30^FDABCD^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'A');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // * (111141)
            1, 1, 1, 1, 4, 1,
            // A (211113) B (211212) C (211311) D (221112)
            2, 1, 1, 1, 1, 3,
            2, 1, 1, 2, 1, 2,
            2, 1, 1, 3, 1, 1,
            2, 2, 1, 1, 1, 2,
            // G (112113) check C, V (222111) check K
            1, 1, 2, 1, 1, 3,
            2, 2, 2, 1, 1, 1,
            // * + termination bar
            1, 1, 1, 1, 4, 1,
            1,
        ],
    );
}

// --- ^BK Codabar -----------------------------------------------------------

/// `1234` with default start/stop A: A 1 2 3 4 A, each character seven
/// elements plus a narrow inter-character gap; wide/narrow 2:1.
#[test]
fn bk_codabar_default_start_stop() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^BKN,,20,N,N^FD1234^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'K');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // A = 11221211
            1, 1, 2, 2, 1, 2, 1, 1,
            // 1 = 11112211
            1, 1, 1, 1, 2, 2, 1, 1,
            // 2 = 11121121
            1, 1, 1, 2, 1, 1, 2, 1,
            // 3 = 22111111
            2, 2, 1, 1, 1, 1, 1, 1,
            // 4 = 11211211
            1, 1, 2, 1, 1, 2, 1, 1,
            // A
            1, 1, 2, 2, 1, 2, 1, 1,
        ],
    );
}

/// Start/stop characters come from the k/l parameters (guide p.118): C..D.
#[test]
fn bk_codabar_custom_start_stop() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^BKN,,20,N,N,C,D^FD77^FS^XZ");
    let f = assert_single_barcode(&label);
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // C = 11121221
            1, 1, 1, 2, 1, 2, 2, 1,
            // 7 = 12112111
            1, 2, 1, 1, 2, 1, 1, 1,
            // 7
            1, 2, 1, 1, 2, 1, 1, 1,
            // D = 11122211
            1, 1, 1, 2, 2, 2, 1, 1,
        ],
    );
}

// --- ^B1 Code 11 -----------------------------------------------------------

/// `12345` with default e=N (two check digits): C = 5*1+4*2+3*3+2*4+1*5 =
/// 35 mod 11 = 2; K over [C, data]: 2*1+5*2+4*3+3*4+2*5+1*6 = 52 mod 11 =
/// 8 -> `1234528`. Start/stop *. Wide/narrow 2:1.
#[test]
fn b1_code11_two_check_digits() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^B1N,N,20,N,N^FD12345^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'1');
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            // * start = 112211
            1, 1, 2, 2, 1, 1,
            // 1 2 3 4 5
            2, 1, 1, 1, 2, 1,
            1, 2, 1, 1, 2, 1,
            2, 2, 1, 1, 1, 1,
            1, 1, 2, 1, 2, 1,
            2, 1, 2, 1, 1, 1,
            // C = 2, K = 8
            1, 2, 1, 1, 2, 1,
            2, 1, 1, 2, 1, 1,
            // * stop = 11221 (first five elements)
            1, 1, 2, 2, 1,
        ],
    );
}

/// e=Y -> a single (C) check digit.
#[test]
fn b1_code11_one_check_digit() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^B1N,Y,20,N,N^FD12345^FS^XZ");
    let f = assert_single_barcode(&label);
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            1, 1, 2, 2, 1, 1,
            2, 1, 1, 1, 2, 1,
            1, 2, 1, 1, 2, 1,
            2, 2, 1, 1, 1, 1,
            1, 1, 2, 1, 2, 1,
            2, 1, 2, 1, 1, 1,
            1, 2, 1, 1, 2, 1, // C = 2
            1, 1, 2, 2, 1,
        ],
    );
}

// --- ^BM MSI ---------------------------------------------------------------

/// MSI mod 10 with e=B (guide default): `1234567` -> Luhn check 4 (rightmost
/// digit doubled: 7*2=14->5, 6, 5*2=10->1, 4, 3*2=6, 2, 1*2=2 -> 26 -> check
/// 4). Start = wide bar + narrow space; stop = 1,2,1.
#[test]
fn bm_msi_mod10() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^BMN,B,20,N,N^FD1234567^FS^XZ");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'M');
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
            1, 2, 2, 1, 1, 2, 1, 2, // 4 (check)
            1, 2, 1, // stop
        ],
    );
}

/// e=D: one Mod-11 (weights 2..9 from the right) then one Mod-10.
/// `1234567`: 7*2+6*3+5*4+4*5+3*6+2*7+1*8 = 112 mod 11 = 2 -> 11-2 = 9;
/// then `12345679` -> 9*2=18->9, 7, 6*2=12->3, 5, 4*2=8, 3, 2*2=4, 1 = 40 ->
/// check 0 -> `123456790`.
#[test]
fn bm_msi_mod11_mod10() {
    let (label, _) = common::parse_one(b"^XA^BY1,2,20^BMN,D,20,N,N^FD1234567^FS^XZ");
    let f = assert_single_barcode(&label);
    #[rustfmt::skip]
    assert_bars(
        &barcode_field(f),
        1,
        &[
            2, 1,
            1, 2, 1, 2, 1, 2, 2, 1, // 1
            1, 2, 1, 2, 2, 1, 1, 2, // 2
            1, 2, 1, 2, 2, 1, 2, 1, // 3
            1, 2, 2, 1, 1, 2, 1, 2, // 4
            1, 2, 2, 1, 1, 2, 2, 1, // 5
            1, 2, 2, 1, 2, 1, 1, 2, // 6
            1, 2, 2, 1, 2, 1, 2, 1, // 7
            2, 1, 1, 2, 1, 2, 2, 1, // 9 (mod 11)
            1, 2, 1, 2, 1, 2, 1, 2, // 0 (mod 10)
            1, 2, 1,
        ],
    );
}

// --- round-1 review fixes ---------------------------------------------------

/// `^B8,30`: the comma glued to the symbology digit is a separator, not a
/// dropped byte - "30" must parse as the height with orientation falling
/// back to the current ^FW (N), with zero diagnostics.
#[test]
fn digit_command_leading_comma_keeps_height() {
    let (label, diags) = common::parse_one(b"^XA^B8,30^FD9638507^FS^XZ");
    assert!(diags.items.is_empty(), "expected no warnings: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!((f.rotation, f.height), (Rotation::R0, 30));
}

/// Same bug shape for a ratio symbology: `^B3,Y,20,N,N` - the "Y" (check
/// digit) is glued to the digit, the rest are real parameter segments.
#[test]
fn digit_command_leading_comma_variant_b3() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,10^B3,Y,20,N,N^FDABC123^FS^XZ");
    assert!(diags.items.is_empty(), "expected no warnings: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!((f.rotation, f.height), (Rotation::R0, 20));
    assert_eq!(f.line, b'N', "f=N after the glued check-digit segment");
}

/// ^B5 Planet (USPS): "0123" -> frame + four digits + frame, each digit
/// five bars (two tall, three short) at one-module pitch, tall bars fill
/// both rows of the two-row module grid, short bars sit in the bottom
/// row. Hand-computed from the USPS PLANET pattern table:
///
///   0=11000 1=00011 2=00101 3=00110
///
/// Columns (38 modules wide): frame(0); 0->[1,3]tall [5,7,9]short;
/// 1->[10,12,14]short [16,18]tall; 2->[19,21,23]short [25,27]tall;
/// 3->[28,30,36]short [32,34]tall; frame(37).
///
/// Row bits (MSB-first bytes, stride 5):
///   bottom row (all bars):   {1,3,5,7,9,10,12,14,16,18,19,21,23,25,27,28,30,32,34,36,37}
///   = D5 6A B5 5A AC
///   top row (tall only):     {0,1,3,16,18,23,27,32,34,37}
///   = D0 00 A1 10 A4
#[test]
fn b5_planet_module_grid() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,30^B5N,30^FD0123^FS^XZ");
    assert!(diags.items.is_empty(), "expected no warnings: {diags:?}");
    let f = assert_single_barcode(&label);
    assert_eq!(f.symbology, b'5');
    // ^B5's f defaults to N (the only 1D symbology that does).
    assert_eq!(f.line, b'N');
    assert_eq!(
        &barcode_field(f),
        &Pattern::Modules {
            width: 38,
            height: 2,
            data: vec![0xD5, 0x6A, 0xB5, 0x5A, 0xAC, 0xD0, 0x00, 0xA1, 0x10, 0xA4],
        },
    );
}

/// ^BY module and ratio are hostile u32s (parse caps w at 9999, so the
/// IR is hand-built): wide = trunc(w*r)/10 must use 64-bit math and
/// saturate - never panic on a debug build. narrow = module = u32::MAX
/// fits; wide = trunc(2^32-1 * 3.0) ~ 1.29e10 saturates to u32::MAX.
#[test]
fn b3_hostile_ratio_module_saturates() {
    let f = BarcodeField {
        x: 0,
        y: 0,
        symbology: b'3',
        data: "B".into(),
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
        font: [b'A', b' '],
        font_height: 9,
        font_width: 5,
        font_name: None,
        rotation: Rotation::R0,
    };
    let Pattern::Bars(bars) = barcode_field(&f) else { panic!("expected bars") };
    assert!(!bars.is_empty());
    assert!(
        bars.iter().all(|b| b.width == u32::MAX),
        "every run saturates: {:?}",
        bars.iter().map(|b| b.width).collect::<Vec<_>>(),
    );
}

/// Code 93 DEL (0x7F) expands to exactly ONE (%)T pair - the regression
/// doubled it to two pairs. A Code 93 character is NINE modules (three
/// bars + three spaces summing to 9, e.g. * = 111141); the DEL-only
/// symbol is * + (%,T) + C + K + * + termination bar = 6 characters at
/// 9 modules + 1 = 55 modules; a doubled DEL would be 73. C = 23, K = 25
/// (43-46-shift values; they don't affect the module count).
#[test]
fn ba_code93_del_is_one_pair() {
    let (label, diags) = common::parse_one(b"^XA^BY1,3,20^BAN,20^FD\x7f^FS^XZ");
    assert!(diags.items.is_empty(), "expected no warnings: {diags:?}");
    let f = assert_single_barcode(&label);
    let Pattern::Bars(bars) = barcode_field(f) else { panic!("expected bars") };
    assert_eq!(
        bars.iter().map(|b| b.width).sum::<u32>(),
        55,
        "2 borders + 4 chars = 6\u{00D7}9 + 1 termination bar = 55 (doubled DEL = 73)",
    );
}

// --- invalid data: parse warning + skipped field, never a panic ------------

/// Every symbology degrades invalid data to an empty pattern at render and
/// a Diagnostic at parse - the "no zero-length pattern without a
/// diagnostic" contract.
#[test]
fn invalid_1d_barcode_data_warns_and_skips() {
    // Non-digit in I2of5.
    let (label, diags) = common::parse_one(b"^XA^B2N,20,N,N^FD12A4^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));

    // ^B9 demands exactly 10 digits.
    let (label, diags) = common::parse_one(b"^XA^B9N,20^FD123^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));

    // ^B9 input with no valid zero-suppression (mfr not ending in 0 and a
    // product above 00009 breaks every case).
    let (label, diags) = common::parse_one(b"^XA^B9N,20^FD1200056789^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));

    // ^B5 rejects non-digits (Planet is numeric-only).
    let (label, diags) = common::parse_one(b"^XA^B5N,20^FD12A4^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));

    // ^BM e=A caps the field at 13 digits (guide p.122).
    let (label, diags) = common::parse_one(b"^XA^BMN,A,20,N,N^FD12345678901234^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));

    // Byte >= 0x80 is outside Code 39's full-ASCII set.
    let (label, diags) = common::parse_one(b"^XA^B3N,N,20,N,N^FDAB\x80^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected warning, got {diags:?}",
    );
    assert!(matches!(barcode_field(assert_single_barcode(&label)), Pattern::Bars(b) if b.is_empty()));
}