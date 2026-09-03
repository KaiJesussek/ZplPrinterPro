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
/// replacements" (guide p.156). The replacements touch only ASCII-range
/// key positions (guide p.159 chart); 0x94 stays o in every national set.
#[test]
fn national_sets_share_cp850_high_bytes() {
    for cp in 1u8..=12 {
        for (byte, ch) in [(0x84u8, "\u{00E4}"), (0x94, "\u{00F6}"), (0xE1, "\u{00DF}")] {
            let mut src = format!("^XA^CI{cp}^FD").into_bytes();
            src.push(byte);
            src.extend_from_slice(b"^FS^XZ");
            let (label, diags) = parse_one(&src);
            assert_eq!(
                diags.items.len(),
                0,
                "^CI{cp} byte 0x{byte:02X}: unexpected diagnostics: {diags:?}",
            );
            assert_eq!(text_field(&label).data, ch, "^CI{cp} byte 0x{byte:02X}");
        }
    }
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

/// Spot checks of the per-country remaps transcribed from the guide's
/// International Character Sets chart (p.159): each national set replaces
/// only its own keys, and ^CI13 (plain 850) still prints the base chars.
#[test]
fn country_sets_remap_their_keys() {
    // 0x7E is avoided in ^FD payloads: `~` splits scanner data tokens and
    // the parser drops the slice after it (pre-existing ^FD behavior) -
    // the full-row checks for those countries live at decode level below.
    let cases: &[(u8, &[u8], &str)] = &[
        // ^CI2 U.K.: '#' (0x23) prints pound.
        (2, b"\x23", "\u{00A3}"),
        // ^CI4 Denmark/Norway.
        (4, b"\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00C6}\u{00D8}\u{00C5}\u{00E6}\u{00F8}\u{00E5}"),
        // ^CI5 Sweden/Finland.
        (5, b"\x40\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00C4}\u{00C4}\u{00D6}\u{00C5}\u{00E4}\u{00F6}\u{00E5}"),
        // ^CI6 Germany.
        (6, b"\x40\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00C4}\u{00C4}\u{00D6}\u{00DC}\u{00E4}\u{00F6}\u{00FC}"),
        // ^CI7 France 1.
        (7, b"\x40\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00E0}\u{00B0}\u{00E7}\u{00A7}\u{00E9}\u{00F9}\u{00E8}"),
        // ^CI8 France 2 (diaeresis replaced by c-cedilla at 0x7E).
        (8, b"\x40\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00E0}\u{00B0}\u{00E7}\u{00A7}\u{00E9}\u{00F9}\u{00E8}"),
        // ^CI9 Italy.
        (9, b"\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00B0}\u{00E7}\u{00E9}\u{00E0}\u{00F2}\u{00E8}"),
        // ^CI10 Spain.
        (10, b"\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00A1}\u{00D1}\u{00BF}\u{00E1}\u{00ED}\u{00F3}"),
        // ^CI11 Miscellaneous: U.K.'s pound plus the German accented row.
        (11, b"\x23\x40\x5B\x5C\x5D\x7B\x7C\x7D", "\u{00A3}\u{00C4}\u{00C4}\u{00D6}\u{00DC}\u{00E4}\u{00F6}\u{00FC}"),
        // ^CI12 Japan: only the backslash key changes (yen).
        (12, b"\x5C", "\u{00A5}"),
        // ^CI13 plain 850 keeps the base characters.
        (13, b"\x23\x5B\x5C\x5D", "#[\\]"),
        // ^CI1 U.S.A. 2 and ^CI3 Holland replace nothing.
        (1, b"\x23\x5B\x5C\x5D", "#[\\]"),
        (3, b"\x40\x5B\x5C\x5D", "@[\\]"),
    ];
    for &(cp, bytes, want) in cases {
        let mut src = format!("^XA^CI{cp}^FD").into_bytes();
        src.extend_from_slice(bytes);
        src.extend_from_slice(b"^FS^XZ");
        let (label, diags) = parse_one(&src);
        assert_eq!(diags.items.len(), 0, "^CI{cp}: {diags:?}");
        assert_eq!(text_field(&label).data, want, "^CI{cp}");
    }
}

/// The full remap rows against `decode_text` directly (all 12 positions,
/// including 0x5E which the scanner treats as a command prefix).
#[test]
fn country_sets_full_rows() {
    use zpl::codepage::decode_text;
    let full = [0x23u8, 0x24, 0x40, 0x5B, 0x5C, 0x5D, 0x7B, 0x7C, 0x7D, 0x7E];
    assert_eq!(decode_text(&full, 1), "#$@[\\]{|}~");
    assert_eq!(decode_text(&full, 2), "\u{00A3}$@[\\]{|}~");
    assert_eq!(decode_text(&full, 4), "#$@\u{00C6}\u{00D8}\u{00C5}\u{00E6}\u{00F8}\u{00E5}~");
    assert_eq!(decode_text(&full, 5), "#$\u{00C4}\u{00C4}\u{00D6}\u{00C5}\u{00E4}\u{00F6}\u{00E5}~");
    assert_eq!(decode_text(&full, 6), "#$\u{00C4}\u{00C4}\u{00D6}\u{00DC}\u{00E4}\u{00F6}\u{00FC}\u{00DF}");
    assert_eq!(decode_text(&full, 7), "#$\u{00E0}\u{00B0}\u{00E7}\u{00A7}\u{00E9}\u{00F9}\u{00E8}\u{00A8}");
    assert_eq!(decode_text(&full, 8), "#$\u{00E0}\u{00B0}\u{00E7}\u{00A7}\u{00E9}\u{00F9}\u{00E8}\u{00E7}");
    assert_eq!(decode_text(&full, 9), "#$@\u{00B0}\u{00E7}\u{00E9}\u{00E0}\u{00F2}\u{00E8}\u{00EC}");
    assert_eq!(decode_text(&full, 10), "#$@\u{00A1}\u{00D1}\u{00BF}\u{00E1}\u{00ED}\u{00F3}\u{00FA}");
    assert_eq!(decode_text(&full, 11), "\u{00A3}$\u{00C4}\u{00C4}\u{00D6}\u{00DC}\u{00E4}\u{00F6}\u{00FC}\u{00DF}");
    assert_eq!(decode_text(&full, 12), "#$@[\u{00A5}]{|}~");
    assert_eq!(decode_text(&full, 13), "#$@[\\]{|}~");
}

/// ^CI28 with malformed UTF-8 warns exactly once and decodes lossily
/// (U+FFFD), never panics - the UTF-8 mirror of the UTF-16 contract.
#[test]
fn malformed_utf8_warns_lossily() {
    let (label, diags) = parse_one(b"^XA^CI28^FD\xFFabc^FS^XZ");
    let warnings: Vec<_> = diags
        .items
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    assert_eq!(warnings.len(), 1, "exactly one warning, got {diags:?}");
    assert_eq!(text_field(&label).data, "\u{FFFD}abc");

    // Valid UTF-8 stays clean.
    let (label, diags) = parse_one("^XA^CI28^FDGr\u{00FC}\u{00DF}e^FS^XZ".as_bytes());
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(text_field(&label).data, "Gr\u{00FC}\u{00DF}e");
}
