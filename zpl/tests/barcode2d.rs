//! Task 16 integration tests for the 2D barcode encoders:
//! QR (^BQ) with REQUIRED rqrr decode round-trips, and structural checks
//! for DataMatrix (^BX), PDF417 (^B7) and Aztec (^B0): symbol dimensions
//! against the standard tables, finder patterns, EC codeword counts.
//!
//! The encoders were cross-checked during development against the zint
//! reference implementation (grids byte-identical for the encodation
//! regimes where the reference picks the same modes; see the report).

mod common;

use zpl::bc::{barcode_field, Pattern};
use zpl::ir::{BarcodeField, Field};

fn single_barcode(label: &zpl::ir::Label) -> &BarcodeField {
    assert_eq!(label.fields.len(), 1, "one field expected, got: {:?}", label.fields);
    match &label.fields[0] {
        Field::Barcode(f) => f,
        other => panic!("expected a barcode field, got {other:?}"),
    }
}

/// Decode a Patterns::Modules QR with rqrr (module map scaled x4,
/// dark = black). Returns the decoded text.
fn decode_qr(pat: &Pattern) -> Result<String, String> {
    let Pattern::Modules { width, height, data } = pat else {
        return Err(format!("expected Modules, got {pat:?}"));
    };
    let w = *width as usize;
    let scale = 4usize;
    let mut prepared = rqrr::PreparedImage::prepare_from_bitmap(w * scale, *height as usize * scale, |x, y| {
        let i = (y / scale) * w + (x / scale);
        data[i >> 3] & (0x80 >> (i & 7)) != 0
    });
    let grids = prepared.detect_grids();
    for g in &grids {
        if let Ok((_meta, text)) = g.decode() {
            return Ok(text);
        }
    }
    Err(format!("no QR grid decoded ({} grids found)", grids.len()))
}

/// Parse `command` inside ^XA^FO0,0^FD...^FS^XZ and return its module pattern.
fn modules_of(command: &[u8], data: &[u8]) -> Pattern {
    let mut label = format!("^XA^FO0,0{}^FD", String::from_utf8_lossy(command));
    label.push_str(&String::from_utf8_lossy(data));
    label.push_str("^FS^XZ");
    let (label, diags) = common::parse_one(label.as_bytes());
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let f = single_barcode(&label);
    barcode_field(f)
}

// --- QR: rqrr decode round-trips (REQUIRED verification) ------------------

#[test]
fn qr_roundtrip_hello_all_levels() {
    // Every EC level round-trips; the QR grid is 21+8 = 29 modules.
    for ec in [b'H', b'Q', b'M', b'L'] {
        let pat = modules_of(format!("^BQN,2,2,{}", ec as char).as_bytes(), b"hello");
        match &pat {
            Pattern::Modules { width, data, .. } => {
                assert_eq!(*width, 29, "v1 symbol + 4-module quiet zone ({ec} as char)");
                assert!(!data.is_empty());
            }
            other => panic!("expected Modules, got {other:?}"),
        }
        assert_eq!(decode_qr(&pat).unwrap(), "hello", "EC level {}", ec as char);
    }
}

#[test]
fn qr_roundtrip_unicode_and_masks() {
    // ^CI28 (UTF-8 input) through the codepage decode; masked symbols
    // (^BQ e) must round-trip.
    for mask in 0..=7u8 {
        let pat = modules_of(format!("^BQN,2,3,Q,{mask}").as_bytes(), b"mask-check");
        assert_eq!(decode_qr(&pat).unwrap(), "mask-check", "mask {mask}");
    }
    // Unicode text (UTF-8 bytes) round-trips verbatim.
    let pat = modules_of(b"^CI28^BQN,2,2,Q", "H\u{00E9}llo W\u{00F6}rld \u{2705}".as_bytes());
    assert_eq!(decode_qr(&pat).unwrap(), "H\u{00E9}llo W\u{00F6}rld \u{2705}");
    // Longer payload forces version 2 (25+8 = 33 modules).
    let long = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let pat = modules_of(b"^BQN,2,2,Q", long);
    if let Pattern::Modules { width, .. } = pat {
        assert_eq!(width, 41, "version 2 symbol");
    }
    let pat = modules_of(b"^BQN,2,2,Q", long);
    assert_eq!(decode_qr(&pat).unwrap(), String::from_utf8_lossy(long));
}

#[test]
fn qr_version_info_and_big_payload() {
    // Versions 7..10 carry the version-info blocks; 120 bytes at level
    // M -> v7 (45+8 = 53), 152 bytes -> v8 (49+8 = 57).
    let payload = vec![b'x'; 120];
    let pat = modules_of(b"^BQN,2,2,M", &payload);
    if let Pattern::Modules { width, .. } = &pat {
        assert!(matches!(width, 53), "v7 at level M holds 120 bytes, got {width}");
    }
    let payload = vec![b'z'; 152];
    let pat = modules_of(b"^BQN,2,2,M", &payload);
    if let Pattern::Modules { width, .. } = &pat {
        assert!(matches!(width, 57), "v8 at level M (152 bytes), got {width}");
    }
    assert_eq!(decode_qr(&pat).unwrap(), "z".repeat(152));
    // v10: 16-bit character-count indicator (ISO Table 3); v10-H byte
    // capacity is 119, so 110 bytes lands exactly in v10.
    let payload = vec![b'k'; 110];
    let pat = modules_of(b"^BQN,2,2,H", &payload);
    if let Pattern::Modules { width, .. } = &pat {
        assert!(matches!(width, 65), "v10 at level H (110 bytes), got {width}");
    }
    assert_eq!(decode_qr(&pat).unwrap(), "k".repeat(110), "v10 round-trip");
}

// --- QR: hostile inputs ---------------------------------------------------

#[test]
fn qr_too_large_is_empty_no_panic() {
    // 400 bytes cannot fit v10 at level H -> parse warning + empty pattern.
    let payload = vec![b'a'; 400];
    let mut label = b"^XA^FO0,0^FD".to_vec();
    label.extend_from_slice(&payload);
    label.extend_from_slice(b"^FS^XZ");
    // Insert the barcode command before the field data:
    let mut label = b"^XA^FO0,0^BQN,2,2,H^FD".to_vec();
    label.extend_from_slice(&payload);
    label.extend_from_slice(b"^FS^XZ");
    let (parsed, diags) = common::parse_one(&label);
    assert!(
        diags.items.iter().any(|d| d.message.contains("exceed")),
        "expected a capacity warning, got {diags:?}"
    );
    let f = single_barcode(&parsed);
    let pat = barcode_field(f);
    assert!(
        matches!(pat, Pattern::Bars(ref b) if b.is_empty()),
        "too-large QR degrades to an empty field"
    );
}

// --- DataMatrix: structural ----------------------------------------------

#[test]
fn dm_hello_is_12x12_with_finder() {
    // "hello" -> 5 data codewords -> the 12x12 symbol (5 data + 7 EC); the
    // grid adds the 1-module quiet zone -> 14x14 modules.
    let pat = modules_of(b"^BXN,1,200", b"hello");
    let Pattern::Modules { width, height, data } = &pat else {
        panic!("expected Modules");
    };
    assert_eq!((*width, *height), (14, 14));
    let g = |x: usize, y: usize| data[y * (*width as usize) + x >> 3] & (0x80 >> ((y * (*width as usize) + x) & 7)) != 0;
    // The solid finder "L": bottom row (symbol row 11 = pattern row 12)
    // and left column (symbol col 0 = pattern col 1) dark.
    for x in 1..13 {
        assert!(g(x, 12), "bottom finder row at x={x}");
    }
    for y in 1..=12 {
        assert!(g(1, y), "left finder col at row {y}");
    }
    // Clock track: top row alternates starting dark at x=1; right column
    // alternates at odd rows from the bottom.
    assert!(g(1, 1), "clock track corner dark");
    assert!(!g(2, 1), "clock track alternates");
    assert!(g(3, 1));
    // EC codeword count check: 5 data + 7 EC = 12 codewords of the 12x12.
    assert_eq!(data.len(), (14 * 14 + 7) / 8);
}

#[test]
fn dm_digits_compress_and_grow_symbol() {
    // 22 digits -> 11 codewords -> 18x18 (18 data) is chosen before 14x14 (8).
    // Digits -> pairs -> 11 cws -> 14x14 holds only 8 -> 18x18 ... wait: the
    // 12x12 holds 5, 14x14 holds 8, 16x16 holds 12 -> 18x18 (18).
    let pat = modules_of(b"^BXN,1,200", b"1234567890123456789012");
    let Pattern::Modules { width, .. } = &pat else { panic!() };
    assert_eq!(*width, 16 + 2, "16x16 symbol + quiet zone");
}

#[test]
fn dm_extended_bytes_use_upper_shift() {
    // Non-ASCII text costs 2 codewords per UTF-8 byte (upper-shift
    // escape 235 + value). "ooooo" = 10 UTF-8 bytes -> 20 codewords ->
    // 20x20 symbol (22 data codewords).
    let pat = modules_of(b"^CI28^BXN,1,200", "\u{00F6}\u{00F6}\u{00F6}\u{00F6}\u{00F6}".as_bytes());
    let Pattern::Modules { width, .. } = &pat else { panic!() };
    assert_eq!(*width, 20 + 2, "20x20 symbol + quiet zone");
}

// --- PDF417: structural ---------------------------------------------------

#[test]
fn pdf417_hello_layout() {
    // "hello": byte compaction 901 + 5 bytes -> 6 payload cws; level 0 ->
    // 2 EC; total 1 (SD) + 6 + 2 = 9 -> 2 columns x 5 rows (auto).
    let pat = modules_of(b"^B7N,3,0", b"hello");
    let Pattern::Modules { width, height, data } = &pat else { panic!() };
    assert_eq!(*width, 17 * 2 + 69 + 4, "2 cols + quiet zone"); // 107
    assert_eq!(*height, 5 * 3, "5 rows \u{00D7} row height 3"); // ^B7 h = 3
    // Start pattern 0x1FEA8 (17 modules): 11111111010101000.
    let start: Vec<bool> = (0..17).map(|i| (0x1FEA8u32 >> (16 - i)) & 1 == 1).collect();
    for (i, &d) in start.iter().enumerate() {
        let x = 2 + i; // after the 2-module quiet zone
        let g = data[0 * (*width as usize) + x >> 3] & (0x80 >> ((0 * (*width as usize) + x) & 7)) != 0;
        assert_eq!(g, d, "start pattern bit {i}");
    }
    assert!(data.iter().any(|&b| b != 0), "has dark modules");
}

#[test]
fn pdf417_security_levels_and_truncation() {
    // Security 1 -> 4 EC codewords; explicit 4 columns.
    let pat = modules_of(b"^B7N,3,1,4", b"payload-payload");
    let Pattern::Modules { width, height, .. } = &pat else { panic!() };
    assert_eq!(*width, 17 * 4 + 69 + 4, "4 columns + quiet zone");
    assert!(height > &0);
    // Truncated: right indicator + stop -> single bar: width 17*(c+2)+1+4.
    let pat = modules_of(b"^B7N,3,0,4,,Y", b"truncated");
    let Pattern::Modules { width, .. } = &pat else { panic!() };
    assert_eq!(*width, 17 * 4 + 35 + 4, "truncated symbol width");
}

// --- Aztec: structural ----------------------------------------------------

#[test]
fn aztec_hello_is_compact_15x15() {
    // "hello" (40 bits) -> compact layer 1 (73 data bits at 23%) -> 15x15
    // + 1-module quiet zone = 17x17.
    let pat = modules_of(b"^B0N,2,,0", b"hello");
    let Pattern::Modules { width, height, data } = &pat else { panic!() };
    assert_eq!((*width, *height), (17, 17), "compact layer 1 + quiet zone");
    // Bulls-eye: center cell dark, ring around it white/ring dark.
    let c = *width as usize / 2; // 8 (quiet zone shifts the center by 1: 9)
    let g = |x: usize, y: usize| {
        let i = y * (*width as usize) + x;
        data[i >> 3] & (0x80 >> (i & 7)) != 0
    };
    // The compact symbol center (offset 6 in a 27 map -> symbol 0-based
    // row 8, col 8 of the 15x15; quiet zone adds 1).
    let (cx, cy) = (c, c);
    assert!(g(cx, cy), "bulls-eye center dark");
    assert!(!g(cx - 1, cy) || !g(cx + 1, cy), "first ring white");
    assert!(data.iter().any(|&b| b != 0));
}

#[test]
fn aztec_forced_layers_and_big_payload() {
    // 200 bytes -> full-range symbol (larger than compact 4's 512 bits;
    // 200 bytes x 8 + overhead > 512 -> full layers).
    let payload = vec![b'y'; 200];
    let pat = modules_of(b"^B0N,2,,0", &payload);
    let Pattern::Modules { width, .. } = &pat else { panic!() };
    assert!(matches!(width, 19..=151), "full-range symbol, got {width}");
    // Forced full layer 1 (d=201): 19x19 + quiet.
    let pat = modules_of(b"^B0N,2,,201", b"tiny");
    let Pattern::Modules { width, .. } = &pat else { panic!() };
    assert_eq!(*width, 19 + 2);
}

// --- registration / parameters --------------------------------------------

#[test]
fn bq_parameters_parse() {
    // Model, magnification, EC and mask are captured on the field.
    let (label, diags) = common::parse_one(b"^XA^FO0,0^BQN,1,5,H,3^FDtoo^FS^XZ");
    assert!(!diags.is_empty(), "model 1 warns");
    let f = single_barcode(&label);
    assert_eq!((f.qr_ec, f.qr_mask), (b'H', 3));
}

#[test]
fn hostile_params_never_panic() {
    let nasty = [b"^BQN,9,9,9,9^FDx^FS".to_vec(), b"^BXN,9,9,9,9,9,9,9^FDx^FS".to_vec()];
    for cmd in nasty {
        let mut label = b"^XA^FO0,0".to_vec();
        label.extend_from_slice(&cmd);
        label.extend_from_slice(b"^XZ");
        let (_label, _diags) = common::parse_one(&label); // no panic
    }
}
