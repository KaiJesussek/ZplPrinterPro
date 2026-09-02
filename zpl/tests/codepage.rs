//! Integration tests for the Task 12 codepage pipeline: `^CI` selects the
//! character set (guide p.156-157), `^FD` bytes decode through it after
//! `^FH` unescaping, and every path (text + barcode) shares the decode.

mod common;

use common::parse_one;
use zpl::diag::Severity;
use zpl::ir::{Field, Label, TextField};

fn text_field(label: &Label) -> &TextField {
    match &label.fields[0] {
        Field::Text(t) => t,
        other => panic!("expected text field, got {other:?}"),
    }
}

/// `^CI13` = Zebra Code Page 850 (guide p.156): byte 0x94 is u-umlaut.
#[test]
fn cp850_german_umlauts() {
    let (label, _) = parse_one(b"^XA^CI13^FD\x94^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{00F6}");
}

/// `^CI28` = UTF-8: valid UTF-8 bytes pass through unchanged.
#[test]
fn ci_utf8_passthrough() {
    let (label, diags) = parse_one("^XA^CI28^FDGr\u{00FC}\u{00DF}e^FS^XZ".as_bytes());
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label).data, "Gr\u{00FC}\u{00DF}e");
}

/// Unknown `^CI` values warn and fall back to the default page (1252):
/// ASCII data decodes identically either way.
#[test]
fn unknown_cp_warns() {
    let (label, diags) = parse_one(b"^XA^CI99^FDabc^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected a warning, got {diags:?}",
    );
    assert_eq!(text_field(&label).data, "abc");
}

/// `^CI29` = UTF-16 big-endian, `^CI30` = UTF-16 little-endian (no BOM).
#[test]
fn ci_utf16_be_and_le() {
    let (label, diags) = parse_one(b"^XA^CI29^FD\x00H\x00i^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label).data, "Hi");

    let (label, _) = parse_one(b"^XA^CI30^FDH\x00i\x00^FS^XZ");
    assert_eq!(text_field(&label).data, "Hi");
}

/// A byte-order mark overrides the `^CI` endianness choice (FF FE = LE,
/// FE FF = BE).
#[test]
fn ci_utf16_bom_overrides_endianness() {
    let (label, _) = parse_one(b"^XA^CI29^FD\xFF\xFEH\x00^FS^XZ");
    assert_eq!(text_field(&label).data, "H");

    let (label, _) = parse_one(b"^XA^CI30^FD\xFE\xFF\x00H^FS^XZ");
    assert_eq!(text_field(&label).data, "H");
}

/// Malformed UTF-16 (odd trailing byte, lone surrogate) warns and decodes
/// lossily - never panics.
#[test]
fn malformed_utf16_warns_lossily() {
    // "H" + a lone trailing byte (BE): unit 0x0048 decodes, the stray byte
    // drops and the field warns instead of panicking.
    let (label, diags) = parse_one(b"^XA^CI29^FD\x00H\x41^FS^XZ");
    assert!(
        diags.items.iter().any(|d| d.severity == Severity::Warning),
        "expected a malformed-UTF-16 warning, got {diags:?}",
    );
    assert_eq!(text_field(&label).data, "H");
}

/// A bare `^FH` on its own line must not consume the trailing newline as
/// the hex indicator (Task 5 deferred minor): the default `_` stays active.
#[test]
fn bare_fh_own_line_keeps_indicator() {
    let (label, diags) = parse_one(b"^XA^FO10,10\n^FH\n^FD_41_42^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label).data, "AB");
}

/// Regression: default `^CI` (power-up 0 -> 1252) decodes plain ASCII ^FD
/// data identically to the previous Latin-1 pipeline.
#[test]
fn default_ci_decodes_ascii_identically() {
    let (label, diags) = parse_one(b"^XA^FO10,10^FDHello World^FS^XZ");
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label).data, "Hello World");
}

/// National sets (1-12) are "Zebra Code Page 850 with specific character
/// replacements" (guide p.156); v1 approximates them as CP850 silently.
/// 0x94 is o in CP850 for every national set that has it.
#[test]
fn national_sets_decode_as_cp850_silently() {
    let (label, diags) = parse_one(b"^XA^CI5^FD\x94^FS^XZ");
    assert_eq!(diags.items.len(), 0, "expected no warning, got {diags:?}");
    assert_eq!(text_field(&label).data, "\u{00F6}");
}

/// ^CI13 must print a backslash for byte 0x5C (guide's ^CI note).
#[test]
fn cp850_backslash_round_trips() {
    let (label, _) = parse_one(b"^XA^CI13^FD\\^FS^XZ");
    assert_eq!(text_field(&label).data, "\\");
}

/// Double-byte states (14-16, 24, 26) and deprecated UCS-2 (17) need
/// .DAT conversion tables, which v1 does not carry: warn "not supported"
/// and fall back to 1252 (ASCII data still decodes).
#[test]
fn unsupported_character_sets_warn_and_fall_back() {
    for cp in [b"14", b"15", b"16", b"17", b"24", b"26"] {
        let mut src = b"^XA^CI".to_vec();
        src.extend_from_slice(cp);
        src.extend_from_slice(b"^FDabc^FS^XZ");
        let (label, diags) = parse_one(&src);
        assert!(
            diags.items.iter().any(|d| d.severity == Severity::Warning),
            "^CI{cp:?}: expected a warning, got {diags:?}",
        );
        assert_eq!(text_field(&label).data, "abc", "^CI{cp:?} fallback");
    }
}

/// Windows pages: ^CI27 = 1252, ^CI31 = CP1250, ^CI33 = CP1251,
/// ^CI34 = CP1253, ^CI35 = CP1254, ^CI36 = CP1255.
#[test]
fn windows_codepages_decode() {
    // ^CI27 = 1252: byte 0x80 is the euro sign (not a C1 control).
    let (label, _) = parse_one(b"^XA^CI27^FD\x80^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{20AC}");

    // ^CI31 = CP1250 (Central Europe): 0xE9 = e.
    let (label, _) = parse_one(b"^XA^CI31^FD\xE9^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{00E9}");

    // ^CI33 = CP1251 (Cyrillic): 0xC0 = Cyrillic-A.
    let (label, _) = parse_one(b"^XA^CI33^FD\xC0^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{0410}");

    // ^CI34 = CP1253 (Greek): 0xE1 = a.
    let (label, _) = parse_one(b"^XA^CI34^FD\xE1^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{03B1}");

    // ^CI35 = CP1254 (Turkish): 0xE9 = e.
    let (label, _) = parse_one(b"^XA^CI35^FD\xE9^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{00E9}");

    // ^CI36 = CP1255 (Hebrew): 0xE9 = Hebrew-yod.
    let (label, _) = parse_one(b"^XA^CI36^FD\xE9^FS^XZ");
    assert_eq!(text_field(&label).data, "\u{05D9}");
}

/// Barcode ^FD data goes through the same decode (shared pipeline): under
/// ^CI13, byte 0x8B is i (U+00EF), which stays single-byte through T9's
/// Latin-1 recovery and is substituted '?' by the 7-bit Code 128 encoder.
#[test]
fn barcode_data_decodes_via_codepage() {
    let (label, _) = parse_one(b"^XA^CI13^BC^FD\x8B^FS^XZ");
    let data = match &label.fields[0] {
        Field::Barcode(b) => b.data.clone(),
        other => panic!("expected barcode field, got {other:?}"),
    };
    assert_eq!(data, "\u{00EF}");
}
