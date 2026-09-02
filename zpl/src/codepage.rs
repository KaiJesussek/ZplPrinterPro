//! `^CI` character-set selection and `^FD` text decoding (guide p.155-157).
//!
//! The guide's `^CI` table maps each value to a character set. Single-byte
//! sets decode through `encoding_rs` where it covers them and through a
//! const Unicode table where it does not (Zebra Code Page 850, which
//! encoding_rs does not ship). `^CI28-30` are the UTF-8/UTF-16 forms.
//!
//! Values the guide defines but this crate cannot render (the Asian
//! double/multi-byte states 14-16/24/26, deprecated UCS-2 17) and values
//! the guide does not define fall back to the default page 1252; [`is_supported`]
//! tells the `^CI` handler which case fired so it can warn honestly.
//!
//! All decode paths are total: malformed input degrades to U+FFFD or a
//! dropped element, never a panic.

use encoding_rs::{Encoding, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255};

/// Zebra Code Page 850 (DOS Latin 1) -> Unicode. The guide's `^CI1-13`
/// national sets are all "Zebra Code Page 850 with specific character
/// replacements" (guide p.156), and `^CI13` *is* 850; this is the only DOS
/// page the ^CI table needs. Public-domain mapping (Unicode Consortium
/// vendor files, as shipped by Python's `cp850` codec); bytes undefined in
/// 850 decode as U+FFFD.
pub(crate) static CP850: [u16; 256] = [
    0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007,
    0x0008, 0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x000E, 0x000F,
    0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016, 0x0017,
    0x0018, 0x0019, 0x001A, 0x001B, 0x001C, 0x001D, 0x001E, 0x001F,
    0x0020, 0x0021, 0x0022, 0x0023, 0x0024, 0x0025, 0x0026, 0x0027,
    0x0028, 0x0029, 0x002A, 0x002B, 0x002C, 0x002D, 0x002E, 0x002F,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037,
    0x0038, 0x0039, 0x003A, 0x003B, 0x003C, 0x003D, 0x003E, 0x003F,
    0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    0x0048, 0x0049, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F,
    0x0050, 0x0051, 0x0052, 0x0053, 0x0054, 0x0055, 0x0056, 0x0057,
    0x0058, 0x0059, 0x005A, 0x005B, 0x005C, 0x005D, 0x005E, 0x005F,
    0x0060, 0x0061, 0x0062, 0x0063, 0x0064, 0x0065, 0x0066, 0x0067,
    0x0068, 0x0069, 0x006A, 0x006B, 0x006C, 0x006D, 0x006E, 0x006F,
    0x0070, 0x0071, 0x0072, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077,
    0x0078, 0x0079, 0x007A, 0x007B, 0x007C, 0x007D, 0x007E, 0x007F,
    0x00C7, 0x00FC, 0x00E9, 0x00E2, 0x00E4, 0x00E0, 0x00E5, 0x00E7,
    0x00EA, 0x00EB, 0x00E8, 0x00EF, 0x00EE, 0x00EC, 0x00C4, 0x00C5,
    0x00C9, 0x00E6, 0x00C6, 0x00F4, 0x00F6, 0x00F2, 0x00FB, 0x00F9,
    0x00FF, 0x00D6, 0x00DC, 0x00F8, 0x00A3, 0x00D8, 0x00D7, 0x0192,
    0x00E1, 0x00ED, 0x00F3, 0x00FA, 0x00F1, 0x00D1, 0x00AA, 0x00BA,
    0x00BF, 0x00AE, 0x00AC, 0x00BD, 0x00BC, 0x00A1, 0x00AB, 0x00BB,
    0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x00C1, 0x00C2, 0x00C0,
    0x00A9, 0x2563, 0x2551, 0x2557, 0x255D, 0x00A2, 0x00A5, 0x2510,
    0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x00E3, 0x00C3,
    0x255A, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256C, 0x00A4,
    0x00F0, 0x00D0, 0x00CA, 0x00CB, 0x00C8, 0x0131, 0x00CD, 0x00CE,
    0x00CF, 0x2518, 0x250C, 0x2588, 0x2584, 0x00A6, 0x00CC, 0x2580,
    0x00D3, 0x00DF, 0x00D4, 0x00D2, 0x00F5, 0x00D5, 0x00B5, 0x00FE,
    0x00DE, 0x00DA, 0x00DB, 0x00D9, 0x00FD, 0x00DD, 0x00AF, 0x00B4,
    0x00AD, 0x00B1, 0x2017, 0x00BE, 0x00B6, 0x00A7, 0x00F7, 0x00B8,
    0x00B0, 0x00A8, 0x00B7, 0x00B9, 0x00B3, 0x00B2, 0x25A0, 0x00A0,
];

/// What a `^CI` value means for the decoder, per the guide's table.
enum Page {
    /// Single-byte page through `encoding_rs`.
    Windows(&'static Encoding),
    /// Single-byte page through a const table.
    Table(&'static [u16; 256]),
    /// UTF-8 (^CI28).
    Utf8,
    /// UTF-16 with the given default endianness (^CI29 BE, ^CI30 LE);
    /// a BOM overrides.
    Utf16 { little: bool },
    /// Not renderable: Asian/DBCS states and deprecated UCS-2.
    Unsupported,
}

/// The `^CI` value -> page mapping (guide p.156-157; transcribed verbatim in
/// the Task 12 report). `None` = a value the guide does not define; it
/// decodes as the default page 1252.
fn page(cp: u8) -> Option<Page> {
    match cp {
        // 0 = U.S.A. 1: Zebra Code Page 1252, the power-up default.
        0 | 27 => Some(Page::Windows(WINDOWS_1252)),
        // 1-12 = national sets, all "Code Page 850 with specific character
        // replacements" (guide p.156); v1 decodes them as 850 silently
        // (the common umlaut bytes coincide; per-country replacements
        // deferred).
        1..=12 => Some(Page::Table(&CP850)),
        // 13 = Zebra Code Page 850 ("^CI13 must be selected to print a
        // backslash" - 0x5C is a backslash in 850).
        13 => Some(Page::Table(&CP850)),
        // 14 = Double Byte Asian, 15 = Shift-JIS, 16 = EUC-JP/EUC-CN,
        // 24 = Single Byte Asian, 26 = Multibyte Asian w/ ASCII
        // transparency: need .DAT conversion tables, v1 has none.
        14 | 15 | 16 | 24 | 26 => Some(Page::Unsupported),
        // 17 = deprecated UCS-2 BE.
        17 => Some(Page::Unsupported),
        28 => Some(Page::Utf8),
        29 => Some(Page::Utf16 { little: false }),
        30 => Some(Page::Utf16 { little: true }),
        // 31 = 1250, 33 = 1251, 34 = 1253, 35 = 1254, 36 = 1255
        // (32 is not defined by the guide).
        31 => Some(Page::Windows(WINDOWS_1250)),
        33 => Some(Page::Windows(WINDOWS_1251)),
        34 => Some(Page::Windows(WINDOWS_1253)),
        35 => Some(Page::Windows(WINDOWS_1254)),
        36 => Some(Page::Windows(WINDOWS_1255)),
        _ => None, // unknown to the guide: default-page fallback
    }
}

/// Whether `cp` is a value the guide defines (decodable or explicitly
/// unsupported). Tells the `^CI` handler whether a value is simply unknown.
pub(crate) fn is_known(cp: u8) -> bool {
    page(cp).is_some()
}

/// Whether `cp` is a value the guide defines but this crate cannot render
/// (Asian states, deprecated UCS-2): warn "not supported" specifically.
pub(crate) fn is_unsupported(cp: u8) -> bool {
    matches!(page(cp), Some(Page::Unsupported))
}

/// Decode `^FD` bytes with the `^CI` character set selected by `cp`.
/// Total: malformed/undefined input decodes lossily (U+FFFD, dropped
/// elements), never panics.
pub fn decode_text(data: &[u8], cp: u8) -> String {
    match page(cp) {
        Some(Page::Windows(enc)) => {
            let (s, _, _) = enc.decode(data);
            s.into_owned()
        }
        Some(Page::Table(table)) => decode_table(data, table),
        // ^CI28: UTF-8 passes through; invalid sequences degrade to U+FFFD.
        Some(Page::Utf8) => String::from_utf8_lossy(data).into_owned(),
        Some(Page::Utf16 { little }) => decode_utf16(data, little),
        // Unknown and defined-but-unsupported both fall back to the default
        // page; the ^CI handler already warned when the value was selected.
        Some(Page::Unsupported) | None => {
            let (s, _, _) = WINDOWS_1252.decode(data);
            s.into_owned()
        }
    }
}

fn decode_table(data: &[u8], table: &[u16; 256]) -> String {
    let mut s = String::with_capacity(data.len());
    for &b in data {
        s.push(char::from_u32(table[b as usize] as u32).unwrap_or('\u{FFFD}'));
    }
    s
}

/// Split a UTF-16 byte stream into (16-bit units, little-endian?) after
/// stripping a byte-order mark. Without a BOM the caller's default
/// endianness applies. Returns `(little, units)`.
fn utf16_units(data: &[u8], default_little: bool) -> (bool, &[u8], bool) {
    let (little, body) = match data {
        [0xFF, 0xFE, rest @ ..] => (true, rest),
        [0xFE, 0xFF, rest @ ..] => (false, rest),
        _ => (default_little, data),
    };
    (little, body, body.len() % 2 != 0)
}

/// Decode UTF-16 (^CI29 big-endian / ^CI30 little-endian, BOM overrides).
/// Unpaired surrogates and the trailing byte of an odd-length stream are
/// dropped from the output (the lossy end); the parser warns via
/// [`is_malformed_utf16`] before the field closes.
fn decode_utf16(data: &[u8], default_little: bool) -> String {
    let (little, body, _odd) = utf16_units(data, default_little);
    let mut s = String::with_capacity(body.len() / 2);
    let mut i = 0;
    while i + 1 < body.len() {
        let hi = if little {
            u16::from_le_bytes([body[i], body[i + 1]])
        } else {
            u16::from_be_bytes([body[i], body[i + 1]])
        };
        i += 2;
        let ch = if (0xD800..=0xDBFF).contains(&hi) {
            // Expect a low surrogate next; a lone high surrogate is lost.
            let lo = if i + 1 < body.len() {
                let l = if little {
                    u16::from_le_bytes([body[i], body[i + 1]])
                } else {
                    u16::from_be_bytes([body[i], body[i + 1]])
                };
                if (0xDC00..=0xDFFF).contains(&l) {
                    i += 2;
                    Some(l)
                } else {
                    None
                }
            } else {
                None
            };
            match lo {
                Some(l) => {
                    let c = 0x10000 + ((hi as u32 - 0xD800) << 10) | (l as u32 - 0xDC00);
                    char::from_u32(c).unwrap_or('\u{FFFD}')
                }
                None => '\u{FFFD}',
            }
        } else if (0xDC00..=0xDFFF).contains(&hi) {
            '\u{FFFD}' // lone low surrogate
        } else {
            char::from_u32(hi as u32).unwrap_or('\u{FFFD}')
        };
        s.push(ch);
    }
    s
}

/// Whether a UTF-16 `^FD` payload is malformed (odd byte count or an
/// unpaired surrogate). Wired into the ^FS/^XZ finalize path so the
/// "malformed -> warning + lossy" contract can warn from the parser's
/// diagnostics sink.
pub(crate) fn is_malformed_utf16(data: &[u8], default_little: bool) -> bool {
    let (little, body, odd) = utf16_units(data, default_little);
    if odd {
        return true;
    }
    let mut i = 0;
    while i < body.len() {
        let u = if little {
            u16::from_le_bytes([body[i], body[i + 1]])
        } else {
            u16::from_be_bytes([body[i], body[i + 1]])
        };
        i += 2;
        if (0xD800..=0xDBFF).contains(&u) {
            // Needs a following low surrogate; a raw pair boundary check:
            // the low surrogate must be the next unit.
            if i + 1 > body.len() {
                return true;
            }
            let l = if i + 1 < body.len() {
                if little {
                    u16::from_le_bytes([body[i], body[i + 1]])
                } else {
                    u16::from_be_bytes([body[i], body[i + 1]])
                }
            } else {
                return true;
            };
            if !(0xDC00..=0xDFFF).contains(&l) {
                return true;
            }
            i += 2;
        } else if (0xDC00..=0xDFFF).contains(&u) {
            return true; // lone low surrogate
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp850_german_umlaut_at_0x94() {
        assert_eq!(decode_text(&[0x94], 13), "\u{00F6}");
        assert_eq!(decode_text(&[0x5C], 13), "\\");
    }

    #[test]
    fn national_set_matches_cp850() {
        assert_eq!(decode_text(&[0x94, 0x84, 0x81], 5), "\u{00F6}\u{00E4}\u{00FC}");
    }

    #[test]
    fn default_page_is_1252() {
        // 0x80 is the euro sign in 1252 (not a C1 control).
        assert_eq!(decode_text(&[0x80], 0), "\u{20AC}");
        assert_eq!(decode_text(&[0xE9], 0), "\u{00E9}");
    }

    #[test]
    fn utf8_passthrough_and_lossy_replacement() {
        assert_eq!(decode_text("Gr\u{00FC}\u{00DF}e".as_bytes(), 28), "Gr\u{00FC}\u{00DF}e");
        // Invalid UTF-8 degrades, never panics.
        assert!(decode_text(&[0xFF, 0xFE], 28).contains('\u{FFFD}'));
    }

    #[test]
    fn utf16_endianness_and_bom() {
        assert_eq!(decode_text(b"\x00H\x00i", 29), "Hi"); // BE default
        assert_eq!(decode_text(b"H\x00i\x00", 30), "Hi"); // LE default
        assert_eq!(decode_text(b"\xFF\xFEH\x00", 29), "H"); // BOM overrides
        assert_eq!(decode_text(b"\xFE\xFF\x00H", 30), "H"); // BOM overrides
        // Surrogate pair, BE order (U+1F600 = D83D DE00).
        assert_eq!(decode_text(b"\xD8\x3D\xDE\x00", 29), "\u{1F600}");
    }

    #[test]
    fn unknown_and_unsupported_fall_back_to_1252() {
        assert_eq!(decode_text(b"abc", 99), "abc");
        assert_eq!(decode_text(&[0x80], 14), "\u{20AC}"); // Asian set -> 1252
    }

    #[test]
    fn malformed_utf16_detection() {
        assert!(is_malformed_utf16(b"H\x00\x41", false)); // odd length
        assert!(!is_malformed_utf16(b"\x00H\x00i", false));
        assert!(is_malformed_utf16(b"\xD8\x00\x41\x00", false)); // lone high
        assert!(is_malformed_utf16(b"\xDC\x00", false)); // lone low, BE
    }
}
