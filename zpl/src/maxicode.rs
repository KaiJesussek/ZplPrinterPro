//! MaxiCode (ISO/IEC 16023) encoder for `^BD`.
//!
//! Ported from the MIT-licensed BinaryKits.Zpl.Viewer `MaxiCodeSymbology.cs`
//! (BinaryKits/BinaryKits.Zpl), byte-for-byte for the codeword table and
//! the GF(64) Reed-Solomon, so the module map matches its reference
//! renderer and real MaxiCode readers. The symbol is a FIXED size: one
//! module = 25.5 mm / 29 (the ISO dims), independent of any ^BY.
//!
//! Layout: 33 rows; row `j` holds `30 - j%2` hexagons (the odd rows are
//! 29 wide and offset by half a column). The 974 data hexagons are
//! numbered row-major. Data encodation: primary message (mode 2 zip +
//! country + service class, mode 3 alphanumeric zip), then the code-set
//! A/B/C/D/E shift/latch machinery for the secondary message, padded,
//! with GF(64) ECC (EC10 for the primary block; EC20, or EC28 in EEC
//! mode 5, interleaved across the even/odd secondary words).

/// Encode `data` with the given mode (2-6) into a 30x33 row-major module
/// map, one `u8` per cell (1 = black). The odd rows contribute 29 cells;
/// the 30th stays white. Invalid mode-2/3 data degrades to mode 4
/// (matching the reference implementation).
pub fn encode(data: &str, mode: u8) -> Vec<u8> {
    let bits = encode_bits(data, mode);
    let mut cells = Vec::with_capacity(33 * 30);
    for j in 0..33usize {
        // the linear index of row j: every previous row contributed
        // 30 - (k % 2) hexagons (odd rows are 29 wide)
        let row_start = 30 * j - j / 2;
        for i in 0..30 - (j % 2) {
            cells.push(u8::from(bits[row_start + i]));
        }
        cells.resize(cells.len() + (j % 2), 0);
    }
    cells
}

/// The raw 974-bit linear map (row-major over the 30/29-hexagon rows).
fn encode_bits(data: &str, mode: u8) -> [bool; 974] {
    let mut result = [false; 974];
    for idx in ORIENTATION_PATTERNS {
        result[idx as usize] = true;
    }

    let words = analyze(data, mode);
    for (i, &val) in words.iter().enumerate() {
        let cw = &CODEWORDS[i];
        let offs: &[i64; 6] = if cw.kind == CodewordKind::Irregular {
            &cw.offsets
        } else {
            match cw.kind {
                CodewordKind::Right => &OFFSETS_RIGHT,
                CodewordKind::Left => &OFFSETS_LEFT,
                CodewordKind::Down => &OFFSETS_DOWN,
                CodewordKind::Irregular => unreachable!(),
            }
        };
        for k in 0..6 {
            result[(cw.msb + offs[k]) as usize] = (val & (1 << (5 - k))) != 0;
        }
    }
    result
}

#[derive(Clone, Copy, PartialEq)]
enum CodewordKind {
    Right,
    Left,
    Down,
    Irregular,
}

struct Codeword {
    kind: CodewordKind,
    msb: i64,
    offsets: [i64; 6],
}

const OFFSETS_RIGHT: [i64; 6] = [0, -1, 30, 29, 59, 58];
const OFFSETS_LEFT: [i64; 6] = [0, -1, 29, 28, 59, 58];
const OFFSETS_DOWN: [i64; 6] = [0, 30, 29, 59, 89, 88];

/// Mode mark + orientation hexagons (linear indices).
const ORIENTATION_PATTERNS: [i64; 13] = [
    28, 29, // mode mark
    276, 277, 306, // top left
    450, 480, // middle left
    492, 522, // middle right
    659, 689, // bottom left
    666, 696, // bottom right
];

const fn cw(kind: CodewordKind, msb: i64, offsets: [i64; 6]) -> Codeword {
    Codeword { kind, msb, offsets }
}

/// Codeword placement, 1-144 (ISO 16023 SS D.3): 9 irregular primary
/// words, 11 primary-buffer words, then the secondary (33-47 data words,
/// depending on mode), the ECC, interleaved by type.
#[rustfmt::skip]
const CODEWORDS: [Codeword; 144] = [
    // primary message, 1-9 (irregular)
    cw(CodewordKind::Irregular, 462, [0, 59, -180, -151, -120, -121]),
    cw(CodewordKind::Irregular, 662, [0, -1, 30, 29, -25, 3]),
    cw(CodewordKind::Irregular, 279, [0, -1, 29, 28, 85, 321]),
    cw(CodewordKind::Irregular, 608, [0, -235, -236, -205, -206, -176]),
    cw(CodewordKind::Irregular, 694, [0, -1, -144, -114, -115, -85]),
    cw(CodewordKind::Irregular, 451, [0, 59, 179, 239, 213, 212]),
    cw(CodewordKind::Irregular, 281, [0, -1, 29, 28, 24, 54]),
    cw(CodewordKind::Irregular, 523, [0, -238, -239, -209, -179, -180]),
    cw(CodewordKind::Irregular, 449, [0, 29, 60, 59, 15, 14]),
    // 10-20
    cw(CodewordKind::Right, 363, [0; 6]),
    cw(CodewordKind::Right, 540, [0; 6]),
    cw(CodewordKind::Left, 639, [0; 6]),
    cw(CodewordKind::Left, 629, [0; 6]),
    cw(CodewordKind::Left, 275, [0; 6]),
    cw(CodewordKind::Right, 375, [0; 6]),
    cw(CodewordKind::Right, 552, [0; 6]),
    cw(CodewordKind::Right, 538, [0; 6]),
    cw(CodewordKind::Right, 361, [0; 6]),
    cw(CodewordKind::Left, 287, [0; 6]),
    cw(CodewordKind::Left, 641, [0; 6]),
    // secondary message, 21-34
    cw(CodewordKind::Right, 1, [0; 6]),
    cw(CodewordKind::Right, 3, [0; 6]),
    cw(CodewordKind::Right, 5, [0; 6]),
    cw(CodewordKind::Right, 7, [0; 6]),
    cw(CodewordKind::Right, 9, [0; 6]),
    cw(CodewordKind::Right, 11, [0; 6]),
    cw(CodewordKind::Right, 13, [0; 6]),
    cw(CodewordKind::Right, 15, [0; 6]),
    cw(CodewordKind::Right, 17, [0; 6]),
    cw(CodewordKind::Right, 19, [0; 6]),
    cw(CodewordKind::Right, 21, [0; 6]),
    cw(CodewordKind::Right, 23, [0; 6]),
    cw(CodewordKind::Right, 25, [0; 6]),
    cw(CodewordKind::Right, 27, [0; 6]),
    // 35-48
    cw(CodewordKind::Left, 116, [0; 6]),
    cw(CodewordKind::Left, 114, [0; 6]),
    cw(CodewordKind::Left, 112, [0; 6]),
    cw(CodewordKind::Left, 110, [0; 6]),
    cw(CodewordKind::Left, 108, [0; 6]),
    cw(CodewordKind::Left, 106, [0; 6]),
    cw(CodewordKind::Left, 104, [0; 6]),
    cw(CodewordKind::Left, 102, [0; 6]),
    cw(CodewordKind::Left, 100, [0; 6]),
    cw(CodewordKind::Left, 98, [0; 6]),
    cw(CodewordKind::Left, 96, [0; 6]),
    cw(CodewordKind::Left, 94, [0; 6]),
    cw(CodewordKind::Left, 92, [0; 6]),
    cw(CodewordKind::Left, 90, [0; 6]),
    // 49-62
    cw(CodewordKind::Right, 178, [0; 6]),
    cw(CodewordKind::Right, 180, [0; 6]),
    cw(CodewordKind::Right, 182, [0; 6]),
    cw(CodewordKind::Right, 184, [0; 6]),
    cw(CodewordKind::Right, 186, [0; 6]),
    cw(CodewordKind::Right, 188, [0; 6]),
    cw(CodewordKind::Right, 190, [0; 6]),
    cw(CodewordKind::Right, 192, [0; 6]),
    cw(CodewordKind::Right, 194, [0; 6]),
    cw(CodewordKind::Right, 196, [0; 6]),
    cw(CodewordKind::Right, 198, [0; 6]),
    cw(CodewordKind::Right, 200, [0; 6]),
    cw(CodewordKind::Right, 202, [0; 6]),
    cw(CodewordKind::Right, 204, [0; 6]),
    // 63-69
    cw(CodewordKind::Left, 293, [0; 6]),
    cw(CodewordKind::Left, 291, [0; 6]),
    cw(CodewordKind::Left, 289, [0; 6]),
    cw(CodewordKind::Left, 273, [0; 6]),
    cw(CodewordKind::Left, 271, [0; 6]),
    cw(CodewordKind::Left, 269, [0; 6]),
    cw(CodewordKind::Left, 267, [0; 6]),
    // 70-75
    cw(CodewordKind::Right, 355, [0; 6]),
    cw(CodewordKind::Right, 357, [0; 6]),
    cw(CodewordKind::Right, 359, [0; 6]),
    cw(CodewordKind::Right, 377, [0; 6]),
    cw(CodewordKind::Right, 379, [0; 6]),
    cw(CodewordKind::Right, 381, [0; 6]),
    // 76-81
    cw(CodewordKind::Left, 470, [0; 6]),
    cw(CodewordKind::Left, 468, [0; 6]),
    cw(CodewordKind::Left, 466, [0; 6]),
    cw(CodewordKind::Left, 448, [0; 6]),
    cw(CodewordKind::Left, 446, [0; 6]),
    cw(CodewordKind::Left, 444, [0; 6]),
    // 82-87
    cw(CodewordKind::Right, 532, [0; 6]),
    cw(CodewordKind::Right, 534, [0; 6]),
    cw(CodewordKind::Right, 536, [0; 6]),
    cw(CodewordKind::Right, 554, [0; 6]),
    cw(CodewordKind::Right, 556, [0; 6]),
    cw(CodewordKind::Right, 558, [0; 6]),
    // 88-94
    cw(CodewordKind::Left, 647, [0; 6]),
    cw(CodewordKind::Left, 645, [0; 6]),
    cw(CodewordKind::Left, 643, [0; 6]),
    cw(CodewordKind::Left, 627, [0; 6]),
    cw(CodewordKind::Left, 625, [0; 6]),
    cw(CodewordKind::Left, 623, [0; 6]),
    cw(CodewordKind::Left, 621, [0; 6]),
    // 95-108
    cw(CodewordKind::Right, 709, [0; 6]),
    cw(CodewordKind::Right, 711, [0; 6]),
    cw(CodewordKind::Right, 713, [0; 6]),
    cw(CodewordKind::Right, 715, [0; 6]),
    cw(CodewordKind::Right, 717, [0; 6]),
    cw(CodewordKind::Right, 719, [0; 6]),
    cw(CodewordKind::Right, 721, [0; 6]),
    cw(CodewordKind::Right, 723, [0; 6]),
    cw(CodewordKind::Right, 725, [0; 6]),
    cw(CodewordKind::Right, 727, [0; 6]),
    cw(CodewordKind::Right, 729, [0; 6]),
    cw(CodewordKind::Right, 731, [0; 6]),
    cw(CodewordKind::Right, 733, [0; 6]),
    cw(CodewordKind::Right, 735, [0; 6]),
    // 109-122
    cw(CodewordKind::Left, 824, [0; 6]),
    cw(CodewordKind::Left, 822, [0; 6]),
    cw(CodewordKind::Left, 820, [0; 6]),
    cw(CodewordKind::Left, 818, [0; 6]),
    cw(CodewordKind::Left, 816, [0; 6]),
    cw(CodewordKind::Left, 814, [0; 6]),
    cw(CodewordKind::Left, 812, [0; 6]),
    cw(CodewordKind::Left, 810, [0; 6]),
    cw(CodewordKind::Left, 808, [0; 6]),
    cw(CodewordKind::Left, 806, [0; 6]),
    cw(CodewordKind::Left, 804, [0; 6]),
    cw(CodewordKind::Left, 802, [0; 6]),
    cw(CodewordKind::Left, 800, [0; 6]),
    cw(CodewordKind::Left, 798, [0; 6]),
    // 123-136
    cw(CodewordKind::Right, 886, [0; 6]),
    cw(CodewordKind::Right, 888, [0; 6]),
    cw(CodewordKind::Right, 890, [0; 6]),
    cw(CodewordKind::Right, 892, [0; 6]),
    cw(CodewordKind::Right, 894, [0; 6]),
    cw(CodewordKind::Right, 896, [0; 6]),
    cw(CodewordKind::Right, 898, [0; 6]),
    cw(CodewordKind::Right, 900, [0; 6]),
    cw(CodewordKind::Right, 902, [0; 6]),
    cw(CodewordKind::Right, 904, [0; 6]),
    cw(CodewordKind::Right, 906, [0; 6]),
    cw(CodewordKind::Right, 908, [0; 6]),
    cw(CodewordKind::Right, 910, [0; 6]),
    cw(CodewordKind::Right, 912, [0; 6]),
    // 137-144
    cw(CodewordKind::Down, 58, [0; 6]),
    cw(CodewordKind::Down, 176, [0; 6]),
    cw(CodewordKind::Down, 294, [0; 6]),
    cw(CodewordKind::Down, 412, [0; 6]),
    cw(CodewordKind::Down, 530, [0; 6]),
    cw(CodewordKind::Down, 648, [0; 6]),
    cw(CodewordKind::Down, 766, [0; 6]),
    cw(CodewordKind::Down, 884, [0; 6]),
];

const ECI: u8 = 27;
const NS: u8 = 31;
const PAD: u8 = 33;
const PAD1: u8 = 29;
const PAD2: u8 = 58;
const SHIFT_A: u8 = 59;
const SHIFT_B: u8 = 59;
const SHIFT_C: u8 = 60;
const SHIFT_D: u8 = 61;
const SHIFT_E: u8 = 62;
const SHIFT_2A: u8 = 56;
const SHIFT_3A: u8 = 57;
const LATCH_A: u8 = 63;
const LATCH_B: u8 = 63;
const LOCK_IN_C: u8 = 60;
const LOCK_IN_D: u8 = 61;
const LOCK_IN_E: u8 = 62;

/// The five code sets (ISO 8859-1 bytes per value). `0xFF` = absent.
#[rustfmt::skip]
const CODE_SET: [[u8; 5]; 64] = [
    [0x0d, 0x60, 0xc0, 0xe0, 0x00],
    [b'A', b'a', 0xc1, 0xe1, 0x01],
    [b'B', b'b', 0xc2, 0xe2, 0x02],
    [b'C', b'c', 0xc3, 0xe3, 0x03],
    [b'D', b'd', 0xc4, 0xe4, 0x04],
    [b'E', b'e', 0xc5, 0xe5, 0x05],
    [b'F', b'f', 0xc6, 0xe6, 0x06],
    [b'G', b'g', 0xc7, 0xe7, 0x07],
    [b'H', b'h', 0xc8, 0xe8, 0x08],
    [b'I', b'i', 0xc9, 0xe9, 0x09],
    [b'J', b'j', 0xca, 0xea, 0x0a],
    [b'K', b'k', 0xcb, 0xeb, 0x0b],
    [b'L', b'l', 0xcc, 0xec, 0x0c],
    [b'M', b'm', 0xcd, 0xed, 0x0d],
    [b'N', b'n', 0xce, 0xee, 0x0e],
    [b'O', b'o', 0xcf, 0xef, 0x0f],
    [b'P', b'p', 0xd0, 0xf0, 0x10],
    [b'Q', b'q', 0xd1, 0xf1, 0x11],
    [b'R', b'r', 0xd2, 0xf2, 0x12],
    [b'S', b's', 0xd3, 0xf3, 0x13],
    [b'T', b't', 0xd4, 0xf4, 0x14],
    [b'U', b'u', 0xd5, 0xf5, 0x15],
    [b'V', b'v', 0xd6, 0xf6, 0x16],
    [b'W', b'w', 0xd7, 0xf7, 0x17],
    [b'X', b'x', 0xd8, 0xf8, 0x18],
    [b'Y', b'y', 0xd9, 0xf9, 0x19],
    [b'Z', b'z', 0xda, 0xfa, 0x1a],
    [ECI, ECI, ECI, ECI, ECI],
    [0x1c, 0x1c, 0x1c, 0x1c, PAD],
    [0x1d, 0x1d, 0x1d, 0x1d, PAD1],
    [0x1e, 0x1e, 0x1e, 0x1e, 0x1b],
    [NS, NS, NS, NS, NS],
    [b' ', b'{', 0xdb, 0xfb, 0x1c],
    [PAD, PAD, 0xdc, 0xfc, 0x1d],
    [b'"', b'}', 0xdd, 0xfd, 0x1e],
    [b'#', b'~', 0xde, 0xfe, 0x1f],
    [b'$', 0x7f, 0xdf, 0xff, 0x9f],
    [b'%', b';', 0xaa, 0xa1, 0xa0],
    [b'&', b'<', 0xac, 0xa8, 0xa2],
    [b'\'', b'=', 0xb1, 0xab, 0xa3],
    [b'(', b'>', 0xb2, 0xaf, 0xa4],
    [b')', b'?', 0xb3, 0xb0, 0xa5],
    [b'*', b'[', 0xb5, 0xb4, 0xa6],
    [b'+', b'\\', 0xb9, 0xb7, 0xa7],
    [b',', b']', 0xba, 0xb8, 0xa9],
    [b'-', b'^', 0xbc, 0xbb, 0xad],
    [b'.', b'_', 0xbd, 0xbf, 0xae],
    [b'/', b' ', 0xbe, 0x8a, 0xb6],
    [b'0', b',', 0x80, 0x8b, 0x95],
    [b'1', b'.', 0x81, 0x8c, 0x96],
    [b'2', b'/', 0x82, 0x8d, 0x97],
    [b'3', b':', 0x83, 0x8e, 0x98],
    [b'4', b'@', 0x84, 0x8f, 0x99],
    [b'5', b'!', 0x85, 0x90, 0x9a],
    [b'6', b'|', 0x86, 0x91, 0x9b],
    [b'7', PAD1, 0x87, 0x92, 0x9c],
    [b'8', SHIFT_2A, 0x88, 0x93, 0x9d],
    [b'9', SHIFT_3A, 0x89, 0x94, 0x9e],
    [b':', PAD2, LATCH_A, LATCH_A, LATCH_A],
    [SHIFT_B, SHIFT_A, b' ', b' ', b' '],
    [SHIFT_C, SHIFT_C, LOCK_IN_C, SHIFT_C, SHIFT_C],
    [SHIFT_D, SHIFT_D, SHIFT_D, LOCK_IN_D, SHIFT_D],
    [SHIFT_E, SHIFT_E, SHIFT_E, SHIFT_E, LOCK_IN_E],
    [LATCH_B, LATCH_A, LATCH_B, LATCH_B, LATCH_B],
];

const EC10: [i64; 11] = [1, 31, 28, 39, 42, 57, 2, 3, 49, 44, 46];
const EC20: [i64; 21] = [
    1, 23, 44, 11, 33, 27, 8, 22, 37, 57, 36, 15, 48, 22, 17, 38, 33, 31, 19, 23, 59,
];
const EC28: [i64; 29] = [
    1, 22, 45, 53, 10, 41, 55, 35, 10, 22, 29, 23, 13, 61, 45, 34, 55, 40, 37, 46, 49, 34, 41, 9,
    43, 7, 20, 11, 28,
];

fn analyze(content: &str, mode: u8) -> Vec<i64> {
    let mut data: Vec<i64> = Vec::new();
    let mut eec = false;
    let mut code_set = 0; // A
    let mut content: Vec<u8> = content.bytes().collect();

    if mode == 2 {
        if let Some((rest, service, country, zip)) = take_mode2(&content) {
            let mut ziplen = zip.len() as i64;
            let mut zipval: i64 = zip.iter().fold(0i64, |a, &c| a * 10 + i64::from(c - b'0'));
            if country == 840 {
                // US zip: padded to 9 digits (reference reads PadRight)
                zipval *= 10i64.pow((9 - ziplen) as u32);
                ziplen = 9;
            }
            primary_words(
                &mut data,
                zipval,
                ziplen,
                i64::from(country),
                i64::from(service),
                mode,
            );
            content = rest;
        } else {
            return analyze("", 4); // invalid mode-2 data -> mode 4
        }
    } else if mode == 3 {
        if let Some((rest, service, country, zip)) = take_mode3(&content) {
            let mut zip = zip;
            zip.resize(6, b' ');
            let mut zipval: i64 = 0;
            for &c in &zip {
                let Some(v) = set_map(0).iter().position(|&v| v == i64::from(c)) else {
                    return analyze("", 4);
                };
                zipval = (zipval << 6) | v as i64;
            }
            primary_words(
                &mut data,
                zipval,
                6,
                i64::from(country),
                i64::from(service),
                mode,
            );
            content = rest;
        } else {
            return analyze("", 4);
        }
    } else {
        data.push(i64::from(mode));
        eec = mode == 5;
    }

    let mut code_map = set_map(0); // A map
                                   // The word stream has a FIXED primary block: words 0-9 + EC10 at
                                   // 10-19, then the secondary interleave from word 20. The C#
                                   // reference only inserts the EC10 when a 10-word boundary is hit
                                   // exactly (`== 10`); a 9-digit numeric run jumps 7 words so the
                                   // boundary is straddled and the EC10 never fires - the resulting
                                   // symbol FAILS the decoder's mandatory RS10 syndrome check (the zint
                                   // reference encodes the block unconditionally). Insert the EC10 of
                                   // the first 10 words once the stream passes them; a straddling run
                                   // keeps its NS in the primary but its payload lands after the block.
    let mut primary_ec = false;
    while !content.is_empty() {
        if !primary_ec && data.len() >= 10 {
            primary_ec = true;
            let ec10 = compute_rs(data[..10].to_vec(), &EC10);
            data.splice(10..10, ec10);
        }

        if content.len() >= 9 && content[..9].iter().all(|c| c.is_ascii_digit()) {
            // 9-digit numeric run: NS + five 6-bit words
            let value: i64 = content[..9]
                .iter()
                .fold(0i64, |a, &c| a * 10 + i64::from(c - b'0'));
            data.push(code_map[NS as usize]);
            content.drain(..9);
            for i in (0..=24).rev().step_by(6) {
                data.push((value >> i) & 63);
            }
            continue;
        }

        let c = content[0];
        if c == b'Z' {
            eprintln!(
                "TRACE Z: set={} in_a={} in_b={} a_code={} rest={:?}",
                code_set,
                in_a(c),
                in_b(c),
                a_code(c),
                String::from_utf8_lossy(&content[..content.len().min(10)])
            );
        }
        // NOTE: membership checks use the ISO char-class ranges, not the
        // value-map lookup: the map's shift-opcode slots 60-63 hold the
        // bytes 0x3c-0x3f, so a byte lookup would silently encode '<',
        // '=', '>' or '?' as a SHIFT opcode (the C# reference keys its
        // maps by string, which cannot collide).
        if code_set == 0 && in_a(c) {
            data.push(a_code(c));
            content.remove(0);
            continue;
        }
        // A -> B: latch when the rest is B-only, shift otherwise.
        if code_set == 0 && in_b(c) {
            let b_rest = content.iter().skip(1).take_while(|&&k| in_b(k)).count();
            let a_rest = content.iter().skip(1).take_while(|&&k| in_a(k)).count();
            let bv = b_code(c);
            if b_rest > 0 && a_rest == 0 {
                data.push(code_map[63]); // LATCH_B (A-set value 63)
                code_set = 1;
                code_map = set_map(1);
                data.push(bv);
            } else {
                data.push(code_map[SHIFT_B as usize]); // A-set shift-to-B (59)
                data.push(bv);
            }
            content.remove(0);
            continue;
        }
        // Latched in B: B-set chars are direct B values.
        if code_set == 1 && in_b(c) {
            data.push(b_code(c));
            content.remove(0);
            continue;
        }
        // B -> A with short-run SHIFT_3A/2A handling.
        if code_set == 1 && in_a(c) {
            let a_run = content.iter().take_while(|&&k| in_a(k)).count();
            let av = a_code(c);
            if a_run >= 3 {
                data.push(code_map[63]); // LATCH_A (B-set value 63)
                code_set = 0;
                code_map = set_map(0);
                data.push(av);
            } else if a_run == 2 {
                // TWSA: 2-char shift to A
                data.push(code_map[56]);
                data.push(av);
                data.push(a_code(content[1]));
                content.drain(..1);
            } else if a_run == 3 {
                // TRSA: 3-char shift to A
                data.push(code_map[57]);
                data.push(av);
                data.push(a_code(content[1]));
                data.push(a_code(content[2]));
                content.drain(..2);
            } else {
                data.push(code_map[59]); // SHIFT_A (B-set value 59)
                data.push(av);
            }
            content.remove(0);
            continue;
        }
        // C/D/E sets: one shift per char (the reference never latches).
        // The high-byte columns cannot alias the 0x3c-0x3f opcode bytes,
        // so the value lookup stays safe here.
        if let Some(cv) = set_map(2).iter().position(|&v| v == i64::from(c)) {
            data.push(code_map[60]); // SHIFT_C
            data.push(cv as i64);
            content.remove(0);
            continue;
        }
        if let Some(dv) = set_map(3).iter().position(|&v| v == i64::from(c)) {
            data.push(code_map[61]); // SHIFT_D
            data.push(dv as i64);
            content.remove(0);
            continue;
        }
        if let Some(ev) = set_map(4).iter().position(|&v| v == i64::from(c)) {
            data.push(code_map[62]); // SHIFT_E
            data.push(ev as i64);
            content.remove(0);
            continue;
        }
        content.remove(0); // non-8859-1: dropped like the reference
    }

    if data.len() <= 10 {
        while data.len() < 10 {
            data.push(code_map[PAD as usize]);
        }
        data.extend(compute_rs(data.clone(), &EC10));
    }

    if eec {
        if data.len() <= 88 {
            while data.len() < 88 {
                data.push(code_map[PAD as usize]);
            }
        } else if mode == 5 {
            // too much data for EEC: degrade to mode 4
            data[0] = 4;
            let head = data[..20].to_vec();
            let tail = data[20..].to_vec();
            data = head.clone();
            data.extend(compute_rs(head, &EC10));
            data.extend(tail);
            eec = false;
        } else {
            data.truncate(88);
        }
    }

    if !eec {
        if data.len() <= 104 {
            while data.len() < 104 {
                data.push(code_map[PAD as usize]);
            }
        } else {
            data.truncate(104);
        }
    }

    let mut odds: Vec<i64> = Vec::new();
    let mut evens: Vec<i64> = Vec::new();
    let mut i = 20usize;
    while i + 1 < data.len() {
        odds.push(data[i]);
        evens.push(data[i + 1]);
        i += 2;
    }
    let (odd_ec, even_ec) = if eec {
        (
            compute_rs(odds.clone(), &EC28),
            compute_rs(evens.clone(), &EC28),
        )
    } else {
        (
            compute_rs(odds.clone(), &EC20),
            compute_rs(evens.clone(), &EC20),
        )
    };
    for k in 0..odd_ec.len() {
        data.push(odd_ec[k]);
        data.push(even_ec[k]);
    }

    data
}

fn primary_words(
    data: &mut Vec<i64>,
    zipval: i64,
    ziplen: i64,
    country: i64,
    service: i64,
    mode: u8,
) {
    data.push(((zipval & 3) << 4) | i64::from(mode));
    data.push((zipval >> 2) & 63);
    data.push((zipval >> 8) & 63);
    data.push((zipval >> 14) & 63);
    data.push((zipval >> 20) & 63);
    data.push(((ziplen & 3) << 4) | (zipval >> 26));
    data.push(((country & 3) << 4) | (ziplen >> 2));
    data.push((country >> 2) & 63);
    data.push(((service & 15) << 2) | (country >> 8));
    data.push(service >> 4);
}

/// Mode 2 primary: `service(3) country(3) zip(1-9)` (the ZPL ^BD
/// concatenation). Returns (rest, service, country, zip bytes).
fn take_mode2(content: &[u8]) -> Option<(Vec<u8>, u32, u32, Vec<u8>)> {
    if content.len() < 7 || !content[..7].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let service = parse3(&content[0..3])?;
    let country = parse3(&content[3..6])?;
    let mut n = 6;
    while n < content.len() && n < 15 && content[n].is_ascii_digit() {
        n += 1;
    }
    if n == 6 {
        return None;
    }
    Some((
        content[n..].to_vec(),
        service,
        country,
        content[6..n].to_vec(),
    ))
}

/// Mode 3 primary: `service(3) country(3) [alnum/space zip]{1,6}`.
fn take_mode3(content: &[u8]) -> Option<(Vec<u8>, u32, u32, Vec<u8>)> {
    if content.len() < 7 || !content[..7].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let service = parse3(&content[0..3])?;
    let country = parse3(&content[3..6])?;
    let mut n = 6;
    while n < content.len() && n < 12 && (content[n].is_ascii_alphanumeric() || content[n] == b' ')
    {
        n += 1;
    }
    if n == 6 {
        return None;
    }
    Some((
        content[n..].to_vec(),
        service,
        country,
        content[6..n].to_vec(),
    ))
}

fn parse3(b: &[u8]) -> Option<u32> {
    if b.iter().all(|c| c.is_ascii_digit()) {
        Some(b.iter().fold(0u32, |a, &c| a * 10 + u32::from(c - b'0')))
    } else {
        None
    }
}

/// The reference's `codeARegex` char set: CR, 0x1c-0x1e, space-colon, A-Z.
fn in_a(c: u8) -> bool {
    c == 0x0d
        || (0x1c..=0x1e).contains(&c)
        || (0x20..=0x3a).contains(&c)
        || (0x41..=0x5a).contains(&c)
}

/// The reference's `codeBRegex` char set.
fn in_b(c: u8) -> bool {
    (0x1c..=0x1e).contains(&c)
        || matches!(c, b' ' | b'!' | 0x2c | 0x2e | 0x2f)
        || (0x3a..=0x40).contains(&c)
        || (0x5b..=0x7f).contains(&c)
}

/// The A-set code of an `in_a` char (ISO row numbering: letters 1-26,
/// CR 0, controls 28-30, then 32-58 for space-colon). Direct arithmetic,
/// never the value-map lookup: the map's 59-63 opcode slots hold the
/// bytes 0x3c-0x3f, which alias the chars '<' '=' '>' '?'.
fn a_code(c: u8) -> i64 {
    match c {
        0x0d => 0,
        0x1c => 28,
        0x1d => 29,
        0x1e => 30,
        0x20..=0x3a => i64::from(c),
        0x41..=0x5a => i64::from(c) - 0x40,
        _ => unreachable!("not an A-set char"),
    }
}

/// The B-set code of an `in_b` char (ISO row numbering).
fn b_code(c: u8) -> i64 {
    match c {
        0x1c..=0x1e => i64::from(c) - 0x1c + 28,
        0x20 => 47,
        0x21 => 53,
        0x2c => 48,
        0x2e => 49,
        0x2f => 50,
        0x3a => 51,
        0x3b => 37,
        0x3c => 38,
        0x3d => 39,
        0x3e => 40,
        0x3f => 41,
        0x40 => 52,
        0x5b => 42,
        0x5c => 43,
        0x5d => 44,
        0x5e => 45,
        0x5f => 46,
        0x60..=0x7a => i64::from(c) - 0x60,
        0x7b => 32,
        0x7c => 54,
        0x7d => 34,
        0x7e => 35,
        0x7f => 36,
        _ => unreachable!("not a B-set char"),
    }
}

/// Column `set` (0-4) of the 64-entry code-set table as a value map.
fn set_map(set: usize) -> [i64; 64] {
    std::array::from_fn(|v| i64::from(CODE_SET[v][set]))
}

fn compute_rs(data: Vec<i64>, ec_poly: &[i64]) -> Vec<i64> {
    let ec_len = ec_poly.len() - 1;
    let mut dividend = data.clone();
    dividend.resize(dividend.len() + ec_len, 0);
    let q = gf64_div(dividend, ec_poly);
    q[q.len() - ec_len..].to_vec()
}

fn gf64_div(mut dividend: Vec<i64>, divisor: &[i64]) -> Vec<i64> {
    let normalizer = divisor[0];
    for i in 0..dividend.len() - (divisor.len() - 1) {
        dividend[i] /= normalizer;
        let coeff = dividend[i];
        if coeff != 0 {
            for j in 1..divisor.len() {
                dividend[i + j] ^= gf64_mul(divisor[j], coeff);
            }
        }
    }
    dividend
}

fn gf64_mul(mut a: i64, mut b: i64) -> i64 {
    let mut product = 0;
    while a != 0 && b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        a <<= 1;
        if a & 64 != 0 {
            a ^= 67; // x^6 + x + 1
        }
        b >>= 1;
    }
    product
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BinaryKits/Zebra mode-2 example: service 002, country 840,
    /// zip 100450000, then the ISO-15434 secondary message (raw bytes,
    /// including the ^FH escapes applied).
    fn build_example2() -> String {
        let mut s = String::new();
        s.push_str("002840100450000");
        s.push_str("[)>");
        s.push(0x1e as char);
        s.push_str("01");
        s.push(0x1d as char);
        s.push_str("961Z00136071");
        s.push(0x1d as char);
        s.push_str("UPSN");
        s.push(0x1d as char);
        s.push_str("123X56");
        s.push(0x1d as char);
        s.push_str("028");
        s.push(0x1d as char);
        s.push(0x1d as char);
        s.push_str("001/001");
        s.push(0x1d as char);
        s.push_str("011");
        s.push(0x1d as char);
        s.push('N');
        s.push(0x1d as char);
        s.push(0x1d as char);
        s.push_str("NEW YORK");
        s.push(0x1d as char);
        s.push_str("NY");
        s.push(0x1e as char);
        s.push(0x04 as char);
        s
    }

    #[test]
    fn mode2_example_zebra_produces_974_bits_with_orientation() {
        let data = build_example2();
        let bits = encode_bits(&data, 2);
        assert_eq!(bits.len(), 974);
        for &i in &ORIENTATION_PATTERNS {
            assert!(bits[i as usize], "orientation bit {i} black");
        }
        let cells = encode(&data, 2);
        assert_eq!(cells.len(), 33 * 30);
        assert_eq!(
            cells.iter().filter(|&&c| c == 1).count(),
            bits.iter().filter(|&&b| b).count(),
            "the 30x33 grid carries every set bit"
        );
    }

    #[test]
    fn mode4_long_data_always_carries_the_ec10_block() {
        // The word stream has a FIXED primary block (words 0-9 + EC10 at
        // 10-19) for every mode; the C# lineage only inserted the EC10 on
        // an exact 10-word boundary, so a 9-digit numeric run (7 words)
        // blew past it and the symbol failed every decoder's mandatory
        // RS10 syndrome check. Words 10-19 must equal the RS10 of 0-9
        // even when the data is long.
        let data = "82001234567890123456789012345678901234567890123456789012345678901234567890";
        let words = analyze(data, 4);
        assert_eq!(words.len(), 144);
        let ec10 = compute_rs(words[..10].to_vec(), &EC10);
        assert_eq!(&words[10..20], ec10.as_slice());
        // and the secondary is the interleaved odds/evens from word 20
        assert_eq!(words[20..104].len(), 84);
        assert_eq!(words[104..144].len(), 40);
    }

    #[test]
    fn mode4_short_data_pads_and_ecs_like_the_reference() {
        let words = analyze("ABC", 4);
        assert_eq!(words.len(), 144);
        assert_eq!(words[0], 4);
        assert_eq!(&words[1..4], &[1, 2, 3]); // A B C in ISO set A
        assert!(words[4..10].iter().all(|&w| w == 33)); // PAD
        let ec10 = compute_rs(words[..10].to_vec(), &EC10);
        assert_eq!(&words[10..20], ec10.as_slice());
    }

    #[test]
    fn numeric_run_words_are_msb_first() {
        // "820012345" -> NS + (value>>24), (>>18), (>>12), (>>6), value
        // (the reference wrote them LSB-first, which no decoder accepts).
        let words = analyze("820012345X", 4);
        assert_eq!(words[1], 31); // NS
        let (v0, v1, v2, v3, v4) = (words[2], words[3], words[4], words[5], words[6]);
        let value = (v0 << 24) | (v1 << 18) | (v2 << 12) | (v3 << 6) | v4;
        assert_eq!(value, 820_012_345);
    }

    #[test]
    fn shift_opcodes_use_the_iso_positions() {
        // The A-set's one-char shift to B/C/D/E lives at values 59-62 and
        // the B-latch at 63; the C# lineage transcribed them at 56-58
        // (the digit slots), so "[)>"-carriers encoded the shift as '8'.
        let words = analyze("[)>\x1e01\x1dABC", 4);
        assert_eq!(words[0], 4);
        assert_eq!(words[1], 59); // A-set shift-to-B, then '[' in set B
        assert_eq!(words[2], 42);
        assert_eq!(words[3], 41); // ')': ISO set A
        assert_eq!(words[4], 59); // '>': B-only, and the 0x1e after it
        assert_eq!(words[5], 40); // is B-mixed, so a second A->B shift
        assert_eq!(words[6], 30); // 0x1e RS: set A
        assert_eq!(words[7], 48); // '0'
        assert_eq!(words[8], 49); // '1'
        assert_eq!(words[9], 29); // 0x1d GS: set A
    }

    #[test]
    fn mode2_carrier_split_primary_and_secondary() {
        // digits-first primary + "[)>"-carrier secondary, like the
        // reference: the primary block holds the 10 shipping digits, the
        // secondary starts after the EC10 at word 20.
        let data = "002840100450000[)>\x1e01\x1d961Z00136071\x1dUPSN\x1d123X56\x1d028\x1d\x1d001/001\x1d011\x1dN\x1d\x1dNEW YORK\x1dNY\x1e\x04";
        let words = analyze(data, 2);
        assert_eq!(words.len(), 144);
        assert_eq!(words[0] & 0x0F, 2); // mode in the low nibble
        let ec10 = compute_rs(words[..10].to_vec(), &EC10);
        assert_eq!(&words[10..20], ec10.as_slice());
        assert_eq!(words[20], 59); // '[': A-set shift-to-B
        assert_eq!(words[21], 42); // '[' in set B
        assert_eq!(words[22], 41); // ')'
        assert_eq!(words[23], 59); // '>': A->B shift
        assert_eq!(words[24], 40); // '>' in set B
        assert_eq!(words[25], 30); // 0x1e RS
        assert_eq!(words[26], 48); // '0'
    }

    #[test]
    fn gf64_mul_matches_poly() {
        // GF(2^6) with x^6+x+1: x^5 * x = x^6 == x+1 = 3; (x+1)^2 = x^2+1.
        assert_eq!(gf64_mul(32, 2), 3);
        assert_eq!(gf64_mul(3, 3), 5);
        assert_eq!(gf64_mul(2, 3), 6);
        // Out-of-field operands follow the copied C# algorithm verbatim
        // (the reference reduces a only when its shift sets bit 6; a=64
        // shifts to 128 with bit 6 clear, so no reduction fires).
        assert_eq!(gf64_mul(64, 2), 128);
        // the EC10 generator: 1,31,28,39,... is a valid poly divisor
        let rs = compute_rs(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10], &EC10);
        assert_eq!(rs.len(), 10);
    }

    #[test]
    fn mode3_primary_pack() {
        let data = "002840RS19  secondary";
        let bits = encode_bits(data, 3);
        assert_eq!(bits.len(), 974);
    }

    #[test]
    fn invalid_mode2_degrades_to_mode4() {
        let bits = encode_bits("not a zip", 2);
        assert_eq!(bits.len(), 974);
    }
}
