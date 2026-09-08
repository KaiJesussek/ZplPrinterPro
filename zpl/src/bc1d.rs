//! 1D barcode encoders (Task 15): ^B1 Code 11, ^B2 Interleaved 2 of 5,
//! ^B3 Code 39, ^B8 EAN-8, ^B9 UPC-E, ^BA Code 93, ^BK Codabar, ^BE
//! EAN-13, ^BU UPC-A, ^BM MSI. Pure data: bytes -> `Pattern::Bars`, no
//! rendering.
//!
//! Mnemonic mapping transcribed from the ZPL II guide (the plan's
//! preliminary guess had errors - the guide wins): ^B1 = Code 11 (p.66),
//! ^B2 = Interleaved 2 of 5 (p.68), ^B3 = Code 39 (p.70), ^B8 = EAN-8
//! (p.83), ^B9 = UPC-E (p.85), ^BA = Code 93 (p.87), ^BK = Codabar
//! (p.118), ^BE = EAN-13 (p.109), ^BU = UPC-A (p.142), ^BM = MSI (p.122).
//! (^B0 = Aztec, ^B4 = Code 49, ^B5 = Planet and ^BB = CODABLOCK are 2D or
//! stacked symbologies outside the 1D scope - their commands warn and
//! skip; Task 16 owns the 2D mnemonics.)
//!
//! Pattern tables are transcribed from the published standards: Code 39
//! ISO/IEC 16388 (Table 1 + Annex A full ASCII), Code 93 ANSI/AIM BC5,
//! I2of5 ISO/IEC 16390, EAN/UPC GS1 (the ISO/IEC 15420 tables), Codabar
//! EN 798, and Code 11 / MSI by the common conventions - all cross-checked
//! against zint's reference implementation.
//!
//! The ^BY ratio applies to the wide/narrow symbologies (^B1 ^B2 ^B3 ^BK
//! ^BM): the printed wide:narrow ratio is trunc(w*r)/w - the guide's
//! Table 6 (p.148) transcribed (its worked "round 21.6 to 22" example
//! contradicts its own 2.3:1 cell at w=9; the table wins - see the Task
//! 15 report). Fixed-ratio symbologies (^B8 ^B9 ^BA ^BE ^BU) scale the
//! table widths by the module directly.

use crate::bc::{Bar, Pattern};
use crate::ir::BarcodeField;

/// The 43 basic Code 39 characters in value order 0..=42.
const C39_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-. $/+%";

/// Code 39 patterns (ISO/IEC 16388 Table 1), ten elements per character:
/// nine bar/space elements (1 = narrow, 2 = wide) plus the trailing narrow
/// inter-character gap. Index 43 is the '*' start/stop character (full
/// ten; the stop prints its first nine elements only).
#[rustfmt::skip]
const C39: [[u8; 10]; 44] = [
    [1,1,1,2,2,1,2,1,1,1], // 0
    [2,1,1,2,1,1,1,1,2,1], // 1
    [1,1,2,2,1,1,1,1,2,1], // 2
    [2,1,2,2,1,1,1,1,1,1], // 3
    [1,1,1,2,2,1,1,1,2,1], // 4
    [2,1,1,2,2,1,1,1,1,1], // 5
    [1,1,2,2,2,1,1,1,1,1], // 6
    [1,1,1,2,1,1,2,1,2,1], // 7
    [2,1,1,2,1,1,2,1,1,1], // 8
    [1,1,2,2,1,1,2,1,1,1], // 9
    [2,1,1,1,1,2,1,1,2,1], // A
    [1,1,2,1,1,2,1,1,2,1], // B
    [2,1,2,1,1,2,1,1,1,1], // C
    [1,1,1,1,2,2,1,1,2,1], // D
    [2,1,1,1,2,2,1,1,1,1], // E
    [1,1,2,1,2,2,1,1,1,1], // F
    [1,1,1,1,1,2,2,1,2,1], // G
    [2,1,1,1,1,2,2,1,1,1], // H
    [1,1,2,1,1,2,2,1,1,1], // I
    [1,1,1,1,2,2,2,1,1,1], // J
    [2,1,1,1,1,1,1,2,2,1], // K
    [1,1,2,1,1,1,1,2,2,1], // L
    [2,1,2,1,1,1,1,2,1,1], // M
    [1,1,1,1,2,1,1,2,2,1], // N
    [2,1,1,1,2,1,1,2,1,1], // O
    [1,1,2,1,2,1,1,2,1,1], // P
    [1,1,1,1,1,1,2,2,2,1], // Q
    [2,1,1,1,1,1,2,2,1,1], // R
    [1,1,2,1,1,1,2,2,1,1], // S
    [1,1,1,1,2,1,2,2,1,1], // T
    [2,2,1,1,1,1,1,1,2,1], // U
    [1,2,2,1,1,1,1,1,2,1], // V
    [2,2,2,1,1,1,1,1,1,1], // W
    [1,2,1,1,2,1,1,1,2,1], // X
    [2,2,1,1,2,1,1,1,1,1], // Y
    [1,2,2,1,2,1,1,1,1,1], // Z
    [1,2,1,1,1,1,2,1,2,1], // -
    [2,2,1,1,1,1,2,1,1,1], // .
    [1,2,2,1,1,1,2,1,1,1], // space
    [1,2,1,2,1,2,1,1,1,1], // $
    [1,2,1,2,1,1,1,2,1,1], // /
    [1,2,1,1,1,2,1,2,1,1], // +
    [1,1,1,2,1,2,1,2,1,1], // %
    [1,2,1,1,2,1,2,1,1,1], // * start; stop = first nine elements
];

/// Full-ASCII Code 39 (ISO/IEC 16388 Annex A, the guide's ^B3 Table 1):
/// each byte 0..=127 expands to one or two basic-set characters (second
/// byte 0 = single; DEL = `%T%T`, the guide's doubled pair, handled when
/// the table is consumed). Auto-applied - the modern-printer behavior; the
/// guide's `+$`/`-$` mode-toggle prose is legacy (Task 15 report).
#[rustfmt::skip]
const EXT39: [[u8; 2]; 128] = [
    *b"%U",*b"$A",*b"$B",*b"$C",*b"$D",*b"$E",*b"$F",*b"$G",
    *b"$H",*b"$I",*b"$J",*b"$K",*b"$L",*b"$M",*b"$N",*b"$O",
    *b"$P",*b"$Q",*b"$R",*b"$S",*b"$T",*b"$U",*b"$V",*b"$W",
    *b"$X",*b"$Y",*b"$Z",*b"%A",*b"%B",*b"%C",*b"%D",*b"%E",
    [b' ', 0],*b"/A",*b"/B",*b"/C",*b"/D",*b"/E",*b"/F",*b"/G",
    *b"/H",*b"/I",*b"/J",*b"/K",*b"/L",[b'-', 0],[b'.', 0],*b"/O",
    [b'0', 0],[b'1', 0],[b'2', 0],[b'3', 0],[b'4', 0],[b'5', 0],[b'6', 0],[b'7', 0],
    [b'8', 0],[b'9', 0],*b"/Z",*b"%F",*b"%G",*b"%H",*b"%I",*b"%J",
    *b"%V",[b'A', 0],[b'B', 0],[b'C', 0],[b'D', 0],[b'E', 0],[b'F', 0],[b'G', 0],
    [b'H', 0],[b'I', 0],[b'J', 0],[b'K', 0],[b'L', 0],[b'M', 0],[b'N', 0],[b'O', 0],
    [b'P', 0],[b'Q', 0],[b'R', 0],[b'S', 0],[b'T', 0],[b'U', 0],[b'V', 0],[b'W', 0],
    [b'X', 0],[b'Y', 0],[b'Z', 0],*b"%K",*b"%L",*b"%M",*b"%N",*b"%O",
    *b"%W",*b"+A",*b"+B",*b"+C",*b"+D",*b"+E",*b"+F",*b"+G",
    *b"+H",*b"+I",*b"+J",*b"+K",*b"+L",*b"+M",*b"+N",*b"+O",
    *b"+P",*b"+Q",*b"+R",*b"+S",*b"+T",*b"+U",*b"+V",*b"+W",
    *b"+X",*b"+Y",*b"+Z",*b"%P",*b"%Q",*b"%R",*b"%S",*b"%T",
];

/// Code 93 patterns (ANSI/AIM BC5), six elements per character: three bars
/// and three spaces, each 1..=4 modules, summing to nine modules. Values
/// 0..=46 are the standard set (43-46 = the ($) (%) (/) (+) shifts);
/// index 47 is the '*' start/stop (111141).
#[rustfmt::skip]
const C93: [[u8; 6]; 48] = [
    [1,3,1,1,1,2], // 0
    [1,1,1,2,1,3], // 1
    [1,1,1,3,1,2], // 2
    [1,1,1,4,1,1], // 3
    [1,2,1,1,1,3], // 4
    [1,2,1,2,1,2], // 5
    [1,2,1,3,1,1], // 6
    [1,1,1,1,1,4], // 7
    [1,3,1,2,1,1], // 8
    [1,4,1,1,1,1], // 9
    [2,1,1,1,1,3], // A
    [2,1,1,2,1,2], // B
    [2,1,1,3,1,1], // C
    [2,2,1,1,1,2], // D
    [2,2,1,2,1,1], // E
    [2,3,1,1,1,1], // F
    [1,1,2,1,1,3], // G
    [1,1,2,2,1,2], // H
    [1,1,2,3,1,1], // I
    [1,2,2,1,1,2], // J
    [1,3,2,1,1,1], // K
    [1,1,1,1,2,3], // L
    [1,1,1,2,2,2], // M
    [1,1,1,3,2,1], // N
    [1,2,1,1,2,2], // O
    [1,3,1,1,2,1], // P
    [2,1,2,1,1,2], // Q
    [2,1,2,2,1,1], // R
    [2,1,1,2,2,2], // S
    [2,1,1,2,2,1], // T
    [2,2,1,1,2,1], // U
    [2,2,2,1,1,1], // V
    [1,1,2,1,2,2], // W
    [1,1,2,2,2,1], // X
    [1,2,2,1,2,1], // Y
    [1,2,3,1,1,1], // Z
    [1,2,1,1,3,1], // -
    [3,1,1,1,1,2], // .
    [3,1,1,2,1,1], // space
    [3,2,1,1,1,1], // $
    [1,1,2,1,3,1], // /
    [1,1,3,1,2,1], // +
    [2,1,1,1,3,1], // %
    [1,2,1,2,2,1], // ($) shift
    [3,1,2,1,1,1], // (%) shift
    [3,1,1,1,2,1], // (/) shift
    [1,2,2,2,1,1], // (+) shift
    [1,1,1,1,4,1], // * start/stop
];

/// Full-ASCII Code 93 (the guide's ^BA tables): each byte 0..=127 expands
/// to one or two VALUES drawn from the 47-symbol set (43-46 are the four
/// shift symbols). Same expansion as Code 39's except the four special
/// shifts replace $/%/+/. DEL = a single (%)T pair (the AIM table allows
/// (%)T / (%)X / (%)Y / (%)Z interchangeably - one pair per DEL, unlike
/// Code 39's doubled %T%T).
#[rustfmt::skip]
const EXT93: [[u8; 2]; 128] = [
    [44,30],[43,10],[43,11],[43,12],[43,13],[43,14],[43,15],[43,16],
    [43,17],[43,18],[43,19],[43,20],[43,21],[43,22],[43,23],[43,24],
    [43,25],[43,26],[43,27],[43,28],[43,29],[43,30],[43,31],[43,32],
    [43,33],[43,34],[43,35],[44,10],[44,11],[44,12],[44,13],[44,14],
    [38, 0],[45,10],[45,11],[45,12],[39, 0],[42, 0],[45,15],[45,16],
    [45,17],[45,18],[45,19],[41, 0],[45,21],[36, 0],[37, 0],[40, 0],
    [ 0, 0],[ 1, 0],[ 2, 0],[ 3, 0],[ 4, 0],[ 5, 0],[ 6, 0],[ 7, 0],
    [ 8, 0],[ 9, 0],[45,35],[44,15],[44,16],[44,17],[44,18],[44,19],
    [44,31],[10, 0],[11, 0],[12, 0],[13, 0],[14, 0],[15, 0],[16, 0],
    [17, 0],[18, 0],[19, 0],[20, 0],[21, 0],[22, 0],[23, 0],[24, 0],
    [25, 0],[26, 0],[27, 0],[28, 0],[29, 0],[30, 0],[31, 0],[32, 0],
    [33, 0],[34, 0],[35, 0],[44,20],[44,21],[44,22],[44,23],[44,24],
    [44,32],[46,10],[46,11],[46,12],[46,13],[46,14],[46,15],[46,16],
    [46,17],[46,18],[46,19],[46,20],[46,21],[46,22],[46,23],[46,24],
    [46,25],[46,26],[46,27],[46,28],[46,29],[46,30],[46,31],[46,32],
    [46,33],[46,34],[46,35],[44,25],[44,26],[44,27],[44,28],[44,29],
];

/// Interleaved 2 of 5 digit patterns (ISO/IEC 16390): five elements, two
/// wide (2), three narrow (1).
#[rustfmt::skip]
const I25: [[u8; 5]; 10] = [
    [1,1,3,3,1], // 0
    [3,1,1,1,3], // 1
    [1,3,1,1,3], // 2
    [3,3,1,1,1], // 3
    [1,1,3,1,3], // 4
    [3,1,3,1,1], // 5
    [1,3,3,1,1], // 6
    [1,1,1,3,3], // 7
    [3,1,1,3,1], // 8
    [1,3,1,3,1], // 9
];

/// Codabar patterns (EN 798): 20 characters in `CODA_CHARS` order, eight
/// elements each - seven bar/space elements (1 narrow, 2 wide) plus the
/// inter-character narrow space. Checked against EN 798 (zint's
/// CodaTable).
#[rustfmt::skip]
const CODA: [[u8; 8]; 20] = [
    [1,1,1,1,1,2,2,1], // 0
    [1,1,1,1,2,2,1,1], // 1
    [1,1,1,2,1,1,2,1], // 2
    [2,2,1,1,1,1,1,1], // 3
    [1,1,2,1,1,2,1,1], // 4
    [2,1,1,1,1,2,1,1], // 5
    [1,2,1,1,1,1,2,1], // 6
    [1,2,1,1,2,1,1,1], // 7
    [1,2,2,1,1,1,1,1], // 8
    [2,1,1,2,1,1,1,1], // 9
    [1,1,1,2,2,1,1,1], // -
    [1,1,2,2,1,1,1,1], // $
    [2,1,1,1,2,1,2,1], // :
    [2,1,2,1,1,1,2,1], // /
    [2,1,2,1,2,1,1,1], // .
    [1,1,2,1,2,1,2,1], // +
    [1,1,2,2,1,2,1,1], // A
    [1,2,1,2,1,1,2,1], // B
    [1,1,1,2,1,2,2,1], // C
    [1,1,1,2,2,2,1,1], // D
];
const CODA_CHARS: &[u8] = b"0123456789-$:/.+ABCD";

/// Code 11 patterns: 11 digits/`-` (five elements + trailing narrow gap;
/// index 10 = '-') and the '*' start/stop (full six; the stop prints its
/// first five elements).
#[rustfmt::skip]
const C11: [[u8; 6]; 12] = [
    [1,1,1,1,2,1], // 0
    [2,1,1,1,2,1], // 1
    [1,2,1,1,2,1], // 2
    [2,2,1,1,1,1], // 3
    [1,1,2,1,2,1], // 4
    [2,1,2,1,1,1], // 5
    [1,2,2,1,1,1], // 6
    [1,1,1,2,2,1], // 7
    [2,1,1,2,1,1], // 8
    [2,1,1,1,1,1], // 9
    [1,1,2,1,1,1], // -
    [1,1,2,2,1,1], // * start; stop = first five elements
];

/// MSI (Modified Plessey) digit patterns: four bars and four spaces (1
/// narrow, 2 wide pulse-width). Start = wide bar + narrow space; stop =
/// 1,2,1.
#[rustfmt::skip]
const MSIT: [[u8; 8]; 10] = [
    [1,2,1,2,1,2,1,2], // 0
    [1,2,1,2,1,2,2,1], // 1
    [1,2,1,2,2,1,1,2], // 2
    [1,2,1,2,2,1,2,1], // 3
    [1,2,2,1,1,2,1,2], // 4
    [1,2,2,1,1,2,2,1], // 5
    [1,2,2,1,2,1,1,2], // 6
    [1,2,2,1,2,1,2,1], // 7
    [2,1,1,2,1,2,1,2], // 8
    [2,1,1,2,1,2,2,1], // 9
];

/// Planet Code digit patterns (USPS PLANET spec - the ^B5 symbology):
/// five bars per digit, two tall (1) and three short (0), at one-module
/// pitch. The symbol is framed by tall bars at both ends.
#[rustfmt::skip]
const PLANET: [[u8; 5]; 10] = [
    [1,1,0,0,0], // 0
    [0,0,0,1,1], // 1
    [0,0,1,0,1], // 2
    [0,0,1,1,0], // 3
    [0,1,0,0,1], // 4
    [0,1,0,1,0], // 5
    [0,1,1,0,0], // 6
    [1,0,0,0,1], // 7
    [1,0,0,1,0], // 8
    [1,0,1,0,0], // 9
];

/// GS1 element tables (ISO/IEC 15420): seven modules per digit.
#[rustfmt::skip]
const L_BITS: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011",
    "0110001", "0101111", "0111011", "0110111", "0001011",
];
#[rustfmt::skip]
const G_BITS: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101",
    "0111001", "0000101", "0010001", "0001001", "0010111",
];
/// R-code = bitwise complement of the L-code.
#[rustfmt::skip]
const R_BITS: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100",
    "1001110", "1010000", "1000100", "1001000", "1110100",
];

/// EAN-13 first-digit parity rows over the six left digits (L/G).
const EAN13_PARITY: [&[u8; 6]; 10] = [
    b"LLLLLL", b"LLGLGG", b"LLGGLG", b"LLGGGL", b"LGLLGG", b"LGGLLG", b"LGGGLL", b"LGLGLG",
    b"LGLGGL", b"LGGLGL",
];

/// UPC-E number-system-0 parity rows selected by the check digit
/// (G = even, L = odd; ISO/IEC 15420 Table 4).
const UPCE_PARITY: [&[u8; 6]; 10] = [
    b"GGGLLL", b"GGLGLL", b"GGLLGL", b"GGLLLG", b"GLGGLL", b"GLLGGL", b"GLLLGG", b"GLGLGL",
    b"GLGLLG", b"GLLGLG",
];

// --- shared helpers --------------------------------------------------------

/// The ^BY wide element in dots: trunc(w*r)/w - guide Table 6 (p.148).
/// The table's fixed-ratio symbologies never reach this. 64-bit math with
/// saturation keeps hostile hand-built IR (u32::MAX modules/ratios) from
/// panicking in debug builds.
fn wide_dots(module: u32, ratio: u32) -> u32 {
    sat_dots((module as u64) * (ratio as u64) / 10)
}

/// Scale fixed-ratio element widths (modules) by the module size.
fn bars_fixed(runs: &[u32], module: u32) -> Pattern {
    Pattern::Bars(
        runs.iter()
            .enumerate()
            .map(|(i, &r)| Bar {
                width: sat_dots((r as u64) * (module as u64)),
                black: i % 2 == 0,
                height: None,
            })
            .collect(),
    )
}

fn sat_dots(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Scale wide/narrow runs: 1 -> narrow dots, 2 -> wide dots.
fn bars_ratio(runs: &[u32], narrow: u32, wide: u32) -> Pattern {
    Pattern::Bars(
        runs.iter()
            .enumerate()
            .map(|(i, &r)| Bar {
                width: if r == 1 { narrow } else { wide },
                black: i % 2 == 0,
                height: None,
            })
            .collect(),
    )
}

/// Digit-only validation shared by the parse-time check and the encoders.
fn check_digits(data: &[u8], sym: &str) -> Result<(), String> {
    match data.iter().find(|&&b| !b.is_ascii_digit()) {
        None => Ok(()),
        Some(&b) => Err(format!(
            "{sym}: invalid character '{}' (0x{b:02X}); field data must be digits only",
            b as char
        )),
    }
}

/// Character-set validation against an allowed byte string.
fn check_charset(data: &[u8], set: &[u8], sym: &str) -> Result<(), String> {
    match data.iter().find(|&&b| !set.contains(&b)) {
        None => Ok(()),
        Some(&b) => Err(format!(
            "{sym}: invalid character '{}' (0x{b:02X}); not in the symbology's character set",
            b as char
        )),
    }
}

/// The 1D symbologies encode byte values; bytes >= 0x80 are unmappable
/// everywhere (the pair tables cover exactly 0x00..=0x7F).
fn check_ascii(data: &[u8], sym: &str) -> Result<(), String> {
    match data.iter().find(|&&b| b >= 0x80) {
        None => Ok(()),
        Some(&b) => Err(format!(
            "{sym}: byte 0x{b:02X} cannot be encoded (1D symbologies are 7-bit)"
        )),
    }
}

// --- data preparation (check digits, start/stop, expansions) ---------------

/// Left-pad with zeros / truncate on the left to `n` characters (the
/// guide's ^B8/^BE/^BU rule).
fn pad_left(data: &[u8], n: usize) -> Vec<u8> {
    let mut out = vec![b'0'; n];
    let take = data.len().min(n);
    out[n - take..].copy_from_slice(&data[data.len() - take..]);
    out
}

/// ^B1: append the Code 11 check digit(s) decided by `e` (guide p.66:
/// Y = 1 digit, N = 2 digits). Values: digits 0-9, '-' = 10. C uses
/// weights 1..10 from the right; K weights 1..9 over [C, data].
fn prepare_code11(check: u8, data: &[u8]) -> Vec<u8> {
    let Some(vals): Option<Vec<u8>> = data
        .iter()
        .map(|&b| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'-' => Some(10),
            _ => None,
        })
        .collect()
    else {
        return data.to_vec(); // encoder rejects invalid data
    };
    if check != 1 && check != 2 {
        return data.to_vec();
    }
    let mut out = data.to_vec();
    let c = weighted(&vals, 10, 11);
    out.push(code11_char(c));
    if check == 2 {
        let mut with_c = vals;
        with_c.push(c);
        out.push(code11_char(weighted(&with_c, 9, 11)));
    }
    out
}

fn code11_char(v: u8) -> u8 {
    if v == 10 {
        b'-'
    } else {
        b'0' + v
    }
}

/// Weighted sum by the Code 11 / Code 93 rule: weights 1..=wrap repeat
/// from the rightmost value; Code 11 wraps at 11 values (mod 11), Code 93
/// at 47 (mod 47).
fn weighted(vals: &[u8], wrap: u32, modulus: u32) -> u8 {
    let mut sum = 0u32;
    let mut w = 1u32;
    for &v in vals.iter().rev() {
        sum += v as u32 * w;
        w = if w == wrap { 1 } else { w + 1 };
    }
    (sum % modulus) as u8
}

/// ^B2 e=Y: the Mod-10 check digit over the (leading-zero-padded, even)
/// data - pad first, then check, so the total stays even (zint's ITF
/// ordering, GS1-consistent).
fn prepare_i25_check(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    let all_digits = out.iter().all(u8::is_ascii_digit);
    if all_digits && out.len().is_multiple_of(2) {
        out.insert(0, b'0');
    }
    if let Some(c) = crate::bc::ucc_mod10(&out) {
        out.push(b'0' + c);
    }
    out
}

/// ^B3: expand full ASCII to the basic Code 39 set (apply-once; the
/// encoder takes the strict set), then append the Mod-43 check character
/// when e=Y (guide p.70 + Mod 43 p.1592).
fn prepare_code39(check: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    for &b in data {
        if b < 128 {
            let p = EXT39[b as usize];
            out.push(p[0]);
            if p[1] != 0 {
                out.push(p[1]);
            }
            if b == 127 {
                // DEL = %T%T (guide Table 1).
                out.extend_from_slice(b"%T");
            }
        } else {
            out.push(b); // the encoder's validation reports it
        }
    }
    if check == b'Y' {
        let sum: u32 = out
            .iter()
            .filter_map(|&b| position(C39_CHARS, b))
            .map(|v| v as u32)
            .sum();
        out.push(C39_CHARS[(sum % 43) as usize]);
    }
    out
}

/// ^BK: the k/l start and stop characters wrap the field data (guide
/// p.118; defaults A/A).
fn prepare_codabar(start: u8, stop: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    out.push(if start == 0 { b'A' } else { start });
    out.extend_from_slice(data);
    out.push(if stop == 0 { b'A' } else { stop });
    out
}

/// MSI check digits per ^BM e (guide p.122): A none, B one Mod 10, C two
/// Mod 10, D one Mod 11 + one Mod 10. Mod 10 = the Luhn-style MSI check
/// (rightmost digit doubled, digit-summed products); Mod 11 = NCR weights
/// 2..=7 repeating from the rightmost digit (reference-verified on
/// ^BMN,D "1234567": cd11=4, cd10=1).
fn prepare_msi(mode: u8, data: &[u8]) -> Vec<u8> {
    if !data.iter().all(u8::is_ascii_digit) {
        return data.to_vec(); // encoder rejects invalid data
    }
    // Field-data limit (guide p.122): 1..13 digits for e=A, 1..14
    // otherwise. The encoder is option-free, so an over-long field is
    // poisoned with a non-digit - its character check skips the symbol
    // (the parse-time diagnostic names the real cause).
    let max = if mode == b'A' { 13 } else { 14 };
    if data.len() > max {
        let mut out = data.to_vec();
        out.push(b':');
        return out;
    }
    let mut out = data.to_vec();
    match mode {
        b'B' => out.push(b'0' + msi_mod10(&out)),
        b'C' => {
            out.push(b'0' + msi_mod10(&out));
            out.push(b'0' + msi_mod10(&out));
        }
        b'D' => {
            let c = msi_mod11(&out);
            if c == 10 {
                out.extend_from_slice(b"10");
            } else {
                out.push(b'0' + c);
            }
            out.push(b'0' + msi_mod10(&out));
        }
        _ => {}
    }
    out
}

fn msi_mod10(data: &[u8]) -> u8 {
    // Luhn with the rightmost digit doubled (the MSI convention).
    const DOUBLE: [u32; 10] = [0, 2, 4, 6, 8, 1, 3, 5, 7, 9];
    let mut sum = 0u32;
    let mut double = true;
    for &b in data.iter().rev() {
        let d = (b - b'0') as usize;
        sum += if double { DOUBLE[d] } else { d as u32 };
        double = !double;
    }
    ((10 - sum % 10) % 10) as u8
}

fn msi_mod11(data: &[u8]) -> u8 {
    // NCR weights 2..=7 repeat from the rightmost digit (reference-verified).
    let mut sum = 0u32;
    let mut w = 2u32;
    for &b in data.iter().rev() {
        sum += (b - b'0') as u32 * w;
        w = if w == 7 { 2 } else { w + 1 };
    }
    ((11 - sum % 11) % 11) as u8
}

/// Per-symbology data preparation for [`crate::bc::barcode_field`]: the
/// 1D options the encoder itself must not know about (^B1 count, ^B2
/// check, ^B3 expansion+check, ^BK start/stop, ^BM mode) are folded into
/// the data, mirroring how ^BC's UCC options are prepared. The encoders
/// take raw already-shaped data.
pub(crate) fn prepare(f: &BarcodeField, data: &[u8]) -> Vec<u8> {
    match f.symbology {
        b'1' => prepare_code11(f.check, data),
        b'2' if f.ucc => prepare_i25_check(data),
        b'3' => prepare_code39(f.check, data),
        b'K' => prepare_codabar(f.start, f.stop, data),
        b'L' => prepare_logmars(data),
        b'M' => prepare_msi(f.check, data),
        _ => data.to_vec(),
    }
}

/// Parse-time data validation (the "no zero-length pattern without a
/// diagnostic" contract): what each encoder would reject, as a message.
/// `check` carries the symbology's check option where it bounds the data
/// (^BM length).
pub(crate) fn data_error(symbology: u8, check: u8, data: &[u8]) -> Option<String> {
    let err: Result<(), String> = match symbology {
        b'1' => check_charset(data, b"0123456789-", "Code 11 (^B1)"),
        b'2' => check_digits(data, "Interleaved 2 of 5 (^B2)"),
        b'3' => check_ascii(data, "Code 39 (^B3)"),
        b'8' => check_digits(data, "EAN-8 (^B8)"),
        b'9' => check_digits(data, "UPC-E (^B9)").and_then(|()| {
            if data.len() != 10 {
                Err("UPC-E (^B9): field data must be exactly 10 digits (5 manufacturer + 5 product)".into())
            } else if upce_digits(data).is_none() {
                Err("UPC-E (^B9): the number has no valid zero-suppression form (see the guide's rules)".into())
            } else {
                Ok(())
            }
        }),
        b'A' => check_ascii(data, "Code 93 (^BA)"),
        b'K' => check_charset(data, CODA_CHARS, "Codabar (^BK)"),
        b'M' => check_digits(data, "MSI (^BM)").and_then(|()| {
            let max = if check == b'A' { 13 } else { 14 };
            if data.len() > max {
                Err(format!(
                    "MSI (^BM): field data is limited to {max} digits with check digit mode '{}'",
                    check as char
                ))
            } else {
                Ok(())
            }
        }),
        b'E' => check_digits(data, "EAN-13 (^BE)"),
        b'U' => check_digits(data, "UPC-A (^BU)"),
        b'5' => check_digits(data, "Planet Code (^B5)"),
        b'I' => check_digits(data, "Industrial 2 of 5 (^BI)"),
        b'J' => check_digits(data, "Standard 2 of 5 (^BJ)"),
        b'L' => check_charset(data, C39_CHARS, "LOGMARS (^BL)"),
        b'P' => check_charset(data, b"0123456789ABCDEF", "Plessey (^BP)"),
        b'Z' => check_digits(data, "POSTNET (^BZ)").and_then(|()| {
            if !matches!(data.len(), 5 | 9 | 11) {
                Err("POSTNET (^BZ): 5, 9, or 11 digit ZIP data is required".into())
            } else {
                Ok(())
            }
        }),
        b'S' => check_digits(data, "UPC/EAN add-on (^BS)").and_then(|()| {
            if !matches!(data.len(), 2 | 5) {
                Err("UPC/EAN add-on (^BS): 2- or 5-digit extension data is required".into())
            } else {
                Ok(())
            }
        }),
        _ => Ok(()),
    };
    err.err()
}

// --- encoders --------------------------------------------------------------

/// Encode Code 39 from basic-set data (expansion and the optional Mod-43
/// character arrive via `prepare`; the start/stop '*' is added here -
/// guide: "Code 39 automatically generates the start and stop character
/// (*)").
pub(crate) fn code39(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    if let Some(&b) = data.iter().find(|&&b| position(C39_CHARS, b).is_none()) {
        return Err(format!(
            "Code 39: '{}' (0x{b:02X}) is not in the Code 39 character set (start/stop '*' is generated)",
            b as char
        ));
    }
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs = Vec::with_capacity(data.len() * 10 + 19);
    runs.extend(C39[43].iter().map(|&e| e as u32)); // start '*' with its trailing gap
    for &b in data {
        runs.extend(
            C39[position(C39_CHARS, b).unwrap() as usize]
                .iter()
                .map(|&e| e as u32),
        );
    }
    runs.extend(C39[43][..9].iter().map(|&e| e as u32)); // stop, no trailing gap
    Ok(bars_ratio(&runs, narrow, wide))
}

/// Encode Code 93 (ANSI/AIM BC5): full-ASCII expansion (the guide's ^BA
/// substitutes & ' ( ) start the ($) (%) (/) (+) shift pairs) -> values ->
/// the mandatory C and K check characters -> * data C K * + termination
/// bar. The guide's `e` only affects the interpretation line; the bars
/// always carry both check characters.
pub(crate) fn code93(data: &[u8], module: u32) -> Result<Pattern, String> {
    let mut vals = expand93(data)?;
    let c = weighted(&vals, 20, 47);
    vals.push(c);
    vals.push(weighted(&vals, 15, 47)); // K over [C, data]
    let mut runs = Vec::with_capacity(vals.len() * 6 + 13);
    runs.extend(C93[47].iter().map(|&e| e as u32)); // *
    for &v in &vals {
        runs.extend(C93[v as usize].iter().map(|&e| e as u32));
    }
    runs.extend(C93[47].iter().map(|&e| e as u32)); // *
    runs.push(1); // termination bar
    Ok(bars_fixed(&runs, module))
}

/// Expand ^BA field data to Code 93 values. The guide's four substitute
/// characters (& ' ( )) followed by a letter emit the matching shift pair
/// (so `&A` is SOH); any other byte expands through the full-ASCII table.
fn expand93(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut vals = Vec::with_capacity(data.len() + 2);
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        let shift = match b {
            b'&' => Some(43),
            b'\'' => Some(44),
            b'(' => Some(45),
            b')' => Some(46),
            _ => None,
        };
        if let Some(shift) = shift {
            if let Some(&next) = data.get(i + 1) {
                if next.is_ascii_uppercase() {
                    vals.push(shift);
                    vals.push(10 + (next - b'A'));
                    i += 2;
                    continue;
                }
            }
        }
        if b < 128 {
            let p = EXT93[b as usize];
            vals.push(p[0]);
            if p[1] != 0 {
                vals.push(p[1]);
            }
        } else {
            return Err(format!(
                "Code 93: byte 0x{b:02X} cannot be encoded (1D symbologies are 7-bit)"
            ));
        }
        i += 1;
    }
    Ok(vals)
}

/// Encode Interleaved 2 of 5 (ISO/IEC 16390): digits only; an odd count
/// gains a leading zero (guide p.69); start NNNN, pairs interleaved
/// (bars/spaces), stop WNN.
pub(crate) fn i2of5(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    check_digits(data, "Interleaved 2 of 5 (^B2)")?;
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut digits: Vec<u8> = Vec::with_capacity(data.len() + 1);
    if data.len() % 2 == 1 {
        digits.push(b'0');
    }
    digits.extend_from_slice(data);
    let mut runs: Vec<u32> = vec![1, 1, 1, 1]; // start NNNN
    for pair in digits.chunks(2) {
        let bars = &I25[(pair[0] - b'0') as usize];
        let spaces = &I25[(pair[1] - b'0') as usize];
        for j in 0..5 {
            runs.push(bars[j] as u32);
            runs.push(spaces[j] as u32);
        }
    }
    runs.extend_from_slice(&[2, 1, 1]); // stop WNN
    Ok(bars_ratio(&runs, narrow, wide))
}

/// Encode Codabar (EN 798): start/stop characters arrive in the data from
/// `prepare` (^BK k/l).
pub(crate) fn codabar(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    if let Some(&b) = data.iter().find(|&&b| !CODA_CHARS.contains(&b)) {
        return Err(format!(
            "Codabar: '{}' (0x{b:02X}) is not in the Codabar character set",
            b as char
        ));
    }
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs = Vec::with_capacity(data.len() * 8);
    for &b in data {
        runs.extend(
            CODA[position(CODA_CHARS, b).unwrap() as usize]
                .iter()
                .map(|&e| e as u32),
        );
    }
    Ok(bars_ratio(&runs, narrow, wide))
}

/// Encode Code 11: check digits arrive in the data from `prepare`; the
/// start/stop '*' surrounds it (start = full pattern, stop = five
/// elements).
pub(crate) fn code11(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    check_charset(data, b"0123456789-", "Code 11 (^B1)")?;
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs = Vec::with_capacity(data.len() * 6 + 11);
    runs.extend(C11[11].iter().map(|&e| e as u32)); // start (six elements incl. gap)
    for &b in data {
        let v = if b == b'-' { 10 } else { (b - b'0') as usize };
        runs.extend(C11[v].iter().map(|&e| e as u32));
    }
    runs.extend(C11[11][..5].iter().map(|&e| e as u32)); // stop
    Ok(bars_ratio(&runs, narrow, wide))
}

/// Encode MSI: check digits arrive in the data from `prepare`.
pub(crate) fn msi(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    check_digits(data, "MSI (^BM)")?;
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs = Vec::with_capacity(data.len() * 8 + 5);
    runs.extend_from_slice(&[2, 1]); // start: wide bar, narrow space
    for &b in data {
        runs.extend(MSIT[(b - b'0') as usize].iter().map(|&e| e as u32));
    }
    runs.extend_from_slice(&[1, 2, 1]); // stop
    Ok(bars_ratio(&runs, narrow, wide))
}

/// Encode EAN-13 (guide ^BE p.109): 12 data digits (left-padded /
/// truncated), the Mod-10 check digit, first-digit parity over the left
/// six, R-codes on the right.
pub(crate) fn ean13(data: &[u8], module: u32) -> Result<Pattern, String> {
    check_digits(data, "EAN-13 (^BE)")?;
    let d = pad_left(data, 12);
    let check = crate::bc::ucc_mod10(&d).unwrap() as usize;
    let parity = EAN13_PARITY[(d[0] - b'0') as usize];
    let mut bits = String::with_capacity(95);
    bits.push_str("101");
    for (i, &c) in d[1..7].iter().enumerate() {
        let code = if parity[i] == b'L' { L_BITS } else { G_BITS }[(c - b'0') as usize];
        bits.push_str(code);
    }
    bits.push_str("01010");
    for &c in &d[7..12] {
        bits.push_str(R_BITS[(c - b'0') as usize]);
    }
    bits.push_str(R_BITS[check]);
    bits.push_str("101");
    Ok(bars_fixed(&bit_runs(&bits), module))
}

/// Encode EAN-8 (guide ^B8 p.83): seven data digits + check, four L-codes
/// then three R-codes plus the check.
pub(crate) fn ean8(data: &[u8], module: u32) -> Result<Pattern, String> {
    check_digits(data, "EAN-8 (^B8)")?;
    let d = pad_left(data, 7);
    let check = crate::bc::ucc_mod10(&d).unwrap() as usize;
    let mut bits = String::with_capacity(67);
    bits.push_str("101");
    for &c in &d[0..4] {
        bits.push_str(L_BITS[(c - b'0') as usize]);
    }
    bits.push_str("01010");
    for &c in &d[4..7] {
        bits.push_str(R_BITS[(c - b'0') as usize]);
    }
    bits.push_str(R_BITS[check]);
    bits.push_str("101");
    Ok(bars_fixed(&bit_runs(&bits), module))
}

/// Encode UPC-A (guide ^BU p.142): eleven data digits + check; the left
/// six are L-codes, the right six R-codes.
pub(crate) fn upca(data: &[u8], module: u32) -> Result<Pattern, String> {
    check_digits(data, "UPC-A (^BU)")?;
    let d = pad_left(data, 11);
    let check = crate::bc::ucc_mod10(&d).unwrap() as usize;
    let mut bits = String::with_capacity(95);
    bits.push_str("101");
    for &c in &d[0..6] {
        bits.push_str(L_BITS[(c - b'0') as usize]);
    }
    bits.push_str("01010");
    for &c in &d[6..11] {
        bits.push_str(R_BITS[(c - b'0') as usize]);
    }
    bits.push_str(R_BITS[check]);
    bits.push_str("101");
    Ok(bars_fixed(&bit_runs(&bits), module))
}

/// Encode UPC-E (guide ^B9 p.85): the full 10-character number
/// (manufacturer, product) zero-suppresses to six digits, the check digit
/// comes from the expanded 11-digit UPC-A, and number-system-0 parity row
/// (by check digit) encodes the six.
pub(crate) fn upce(data: &[u8], module: u32) -> Result<Pattern, String> {
    check_digits(data, "UPC-E (^B9)")?;
    if data.len() != 10 {
        return Err(
            "UPC-E: field data must be exactly 10 digits (5 manufacturer + 5 product)".into(),
        );
    }
    let Some(d) = upce_digits(data) else {
        return Err("UPC-E: the number has no valid zero-suppression form".into());
    };
    let check = crate::bc::ucc_mod10(&upce_expand(&d)).unwrap() as usize;
    let parity = UPCE_PARITY[check];
    let mut bits = String::with_capacity(51);
    bits.push_str("101");
    for (i, &c) in d.iter().enumerate() {
        let code = if parity[i] == b'L' { L_BITS } else { G_BITS }[(c - b'0') as usize];
        bits.push_str(code);
    }
    bits.push_str("010101");
    Ok(bars_fixed(&bit_runs(&bits), module))
}

/// Encode Planet Code (USPS; guide ^B5 p.78): digits 0-9 only. Each
/// digit is five bars (two tall, three short) with tall frame bars
/// opening and closing the symbol; the tall/short heights ARE the data.
///
/// Geometry measured against the reference (parity probe 31, module 2,
/// height 120): bars one ^BY module wide, the inter-bar space 3/2
/// modules (the USPS 2:3 element ratio at 203 dpi), tall bars the full
/// field height, short bars 2/5 of it, bottom-anchored. PLANET digit
/// patterns are the POSTNET patterns with inverted polarity - the
/// table's 1-bit columns paint SHORT bars (the reference renders "1"
/// as T T T S S). The USPS mod-10 check digit is appended to the bars
/// ("0123" renders as 01234) while the interpretation line shows the
/// field data.
pub(crate) fn planet(data: &[u8], module: u32, height: u32) -> Result<Pattern, String> {
    check_digits(data, "Planet Code (^B5)")?;
    if data.is_empty() {
        return Err("Planet Code (^B5): field data is required".into());
    }
    // Reference check digit (corpus 31: "12345" -> bars "123455").
    let sum: u32 = data.iter().map(|&d| u32::from(d - b'0')).sum();
    let check = (10 - sum % 10) % 10;
    let short_h = (2 * height.max(1)) / 5;
    let space = 3 * module / 2;
    let mut bars = Vec::with_capacity(2 + data.len() * 10);
    bars.push(Bar {
        width: module,
        black: true,
        height: None,
    }); // leading frame
    for digit in data
        .iter()
        .copied()
        .chain(std::iter::once(b'0' + check as u8))
    {
        for &short in &PLANET[(digit - b'0') as usize] {
            bars.push(Bar {
                width: space,
                black: false,
                height: None,
            });
            bars.push(Bar {
                width: module,
                black: true,
                height: if short == 1 { Some(short_h) } else { None },
            });
        }
    }
    bars.push(Bar {
        width: space,
        black: false,
        height: None,
    });
    bars.push(Bar {
        width: module,
        black: true,
        height: None,
    }); // trailing frame
    Ok(Pattern::Bars(bars))
}

/// Zero-suppression (GS1 Table 5, forward direction; the guide's p.86
/// product-code rules): the four mfr/product cases in priority order.
fn upce_digits(data: &[u8]) -> Option<[u8; 6]> {
    let m = &data[0..5];
    let p = &data[5..10];
    let d1 = m[0];
    let d2 = m[1];
    let m3 = m[2];
    let m4 = m[3];
    let m5 = m[4];
    let p1 = p[0];
    let p2 = p[1];
    let p3 = p[2];
    let p4 = p[3];
    let p5 = p[4];
    if m4 == b'0' && m5 == b'0' && (b'0'..=b'2').contains(&m3) && p1 == b'0' && p2 == b'0' {
        // mfr ends 000/100/200, product <= 999 (guide rule 1)
        Some([d1, d2, p3, p4, p5, m3])
    } else if m4 == b'0'
        && m5 == b'0'
        && (b'3'..=b'9').contains(&m3)
        && p1 == b'0'
        && p2 == b'0'
        && p3 == b'0'
    {
        // mfr ends 300..900, product <= 99 (guide rule 2)
        Some([d1, d2, m3, p4, p5, b'3'])
    } else if m5 == b'0' && m4 != b'0' && p1 == b'0' && p2 == b'0' && p3 == b'0' && p4 == b'0' {
        // mfr ends 10..90, product <= 9 (guide rule 3)
        Some([d1, d2, m3, m4, p5, b'4'])
    } else if m5 != b'0'
        && p1 == b'0'
        && p2 == b'0'
        && p3 == b'0'
        && p4 == b'0'
        && (b'5'..=b'9').contains(&p5)
    {
        // mfr not ending in 0, product 00005..00009 (guide rule 4)
        Some([d1, d2, m3, m4, m5, p5])
    } else {
        None
    }
}

/// The 11-digit UPC-A equivalent of a six-digit UPC-E number (GS1 Table
/// 5, decode direction; number system 0).
fn upce_expand(d: &[u8; 6]) -> [u8; 11] {
    let mut a = [b'0'; 11];
    a[1] = d[0];
    a[2] = d[1];
    match d[5] {
        b'0'..=b'2' => {
            a[3] = d[5];
            a[8] = d[2];
            a[9] = d[3];
            a[10] = d[4];
        }
        b'3' => {
            a[3] = d[2];
            a[9] = d[3];
            a[10] = d[4];
        }
        b'4' => {
            a[3] = d[2];
            a[4] = d[3];
            a[10] = d[4];
        }
        _ => {
            a[3] = d[2];
            a[4] = d[3];
            a[5] = d[4];
            a[10] = d[5];
        }
    }
    a
}

/// Collapse a 1/0 module string into merged run lengths.
fn bit_runs(bits: &str) -> Vec<u32> {
    let bytes = bits.as_bytes();
    let mut runs = Vec::with_capacity(bytes.len() / 2 + 1);
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
    runs
}

fn position(set: &[u8], b: u8) -> Option<u8> {
    set.iter().position(|&c| c == b).map(|i| i as u8)
}

// --- ^BI / ^BJ (zint 2of5.c tables) ---------------------------------------

/// Industrial 2 of 5 element rows: five bars with narrow spaces, two
/// wide bars per digit ('1' narrow, '3' wide; zint C25IndustTable).
#[rustfmt::skip]
const C25_INDUST: [[u8; 10]; 10] = [
    [1, 1, 1, 1, 3, 1, 3, 1, 1, 1], // 0
    [3, 1, 1, 1, 1, 1, 1, 1, 3, 1], // 1
    [1, 1, 3, 1, 1, 1, 1, 1, 3, 1], // 2
    [3, 1, 3, 1, 1, 1, 1, 1, 1, 1], // 3
    [1, 1, 1, 1, 3, 1, 1, 1, 3, 1], // 4
    [3, 1, 1, 1, 3, 1, 1, 1, 1, 1], // 5
    [1, 1, 3, 1, 3, 1, 1, 1, 1, 1], // 6
    [1, 1, 1, 1, 1, 1, 3, 1, 3, 1], // 7
    [3, 1, 1, 1, 1, 1, 3, 1, 1, 1], // 8
    [1, 1, 3, 1, 1, 1, 3, 1, 1, 1], // 9
];

/// Standard (Matrix) 2 of 5 element rows: six elements per digit with
/// two wide bars (zint C25MatrixTable).
#[rustfmt::skip]
const C25_MATRIX: [[u8; 6]; 10] = [
    [1, 1, 3, 3, 1, 1], // 0
    [3, 1, 1, 1, 3, 1], // 1
    [1, 3, 1, 1, 3, 1], // 2
    [3, 3, 1, 1, 1, 1], // 3
    [1, 1, 3, 1, 3, 1], // 4
    [3, 1, 3, 1, 1, 1], // 5
    [1, 3, 3, 1, 1, 1], // 6
    [1, 1, 1, 3, 3, 1], // 7
    [3, 1, 1, 3, 1, 1], // 8
    [1, 3, 1, 3, 1, 1], // 9
];

/// ^BI: bars carry the data, spaces are narrow. Guards: wide-narrow-wide
/// start, wide-narrow-narrow-wide stop; e.g. the ^BY ratio scales the wide
/// elements.
pub(crate) fn industrial25(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    if data.iter().any(|b| !b.is_ascii_digit()) {
        return Err("Industrial 2 of 5 (^BI): digits only".into());
    }
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs: Vec<u32> = Vec::with_capacity(6 + data.len() * 10);
    runs.extend([2, 1, 2, 1, 1, 1]); // start
    for &b in data {
        for &e in &C25_INDUST[(b - b'0') as usize] {
            runs.push(if e == 1 { 1 } else { 2 });
        }
    }
    runs.extend([2, 1, 1, 1, 2]); // stop
    Ok(bars_ratio(&runs, narrow, wide))
}

/// ^BJ: six elements per digit; start guard wide-bar, stop wide-bar.
pub(crate) fn standard25(data: &[u8], module: u32, ratio: u32) -> Result<Pattern, String> {
    if data.iter().any(|b| !b.is_ascii_digit()) {
        return Err("Standard 2 of 5 (^BJ): digits only".into());
    }
    let narrow = module;
    let wide = wide_dots(module, ratio);
    let mut runs: Vec<u32> = Vec::with_capacity(6 + data.len() * 7);
    runs.extend([2, 1, 1, 1, 1, 1]); // start
    for &b in data {
        for &e in &C25_MATRIX[(b - b'0') as usize] {
            runs.push(if e == 1 { 1 } else { 2 });
        }
    }
    runs.push(1); // stop gap
    runs.extend([2, 1, 1, 1, 1]); // stop
    Ok(bars_ratio(&runs, narrow, wide))
}

// --- ^BL LOGMARS -----------------------------------------------------------

/// LOGMARS field data: Code 39 with a mandatory Mod-43 check character
/// (DoD UID spec; zint code 50).
fn prepare_logmars(data: &[u8]) -> Vec<u8> {
    if data.iter().any(|&b| position(C39_CHARS, b).is_none()) {
        return data.to_vec(); // the encoder reports it
    }
    let sum: u32 = data
        .iter()
        .filter_map(|&b| position(C39_CHARS, b))
        .map(u32::from)
        .sum();
    let mut out = data.to_vec();
    out.push(C39_CHARS[(sum % 43) as usize]);
    out
}

// --- ^BP Plessey (zint plessey.c) -------------------------------------------

/// Plesey element table: four pulses (bar,space) per character, '1'
/// narrow, '3' wide modules (zint PlessTable).
#[rustfmt::skip]
const PLESSEY: [[u8; 8]; 16] = [
    [1, 3, 1, 3, 1, 3, 1, 3], // 0
    [3, 1, 1, 3, 1, 3, 1, 3], // 1
    [1, 3, 3, 1, 1, 3, 1, 3], // 2
    [3, 1, 3, 1, 1, 3, 1, 3], // 3
    [1, 3, 1, 3, 3, 1, 1, 3], // 4
    [3, 1, 1, 3, 3, 1, 1, 3], // 5
    [1, 3, 3, 1, 3, 1, 1, 3], // 6
    [3, 1, 3, 1, 3, 1, 1, 3], // 7
    [1, 3, 1, 3, 1, 3, 3, 1], // 8
    [3, 1, 1, 3, 1, 3, 3, 1], // 9
    [1, 3, 3, 1, 1, 3, 3, 1], // A
    [3, 1, 3, 1, 1, 3, 3, 1], // B
    [1, 3, 1, 3, 3, 1, 3, 1], // C
    [3, 1, 1, 3, 3, 1, 3, 1], // D
    [1, 3, 3, 1, 3, 1, 3, 1], // E
    [3, 1, 3, 1, 3, 1, 3, 1], // F
];

/// ^BP: pulse-width code with an inherent 4x CRC check (zint grid
/// 100101001 / generator x.4+x+1). Fixed 1/3-module pulse widths; the ^BY
/// ratio does not apply.
pub(crate) fn plessey(data: &[u8], module: u32) -> Result<Pattern, String> {
    if data
        .iter()
        .any(|b| position(b"0123456789ABCDEF", *b).is_none())
    {
        return Err("Plessey (^BP): hexadecimal data (0-9, A-F) required".into());
    }
    const START: [u32; 8] = [3, 1, 3, 1, 1, 3, 3, 1];
    const STOP: [u32; 9] = [3, 3, 1, 3, 1, 1, 3, 1, 3];
    let mut bits: Vec<u8> = Vec::with_capacity(data.len() * 4 + 8);
    for &b in data {
        let v = u8::from_str_radix(&(b as char).to_string(), 16).unwrap_or(0);
        for k in 0..4 {
            bits.push((v >> k) & 1);
        }
    }
    // 4x CRC over the data bits (grid 100101001 -> 111101001 plus the
    // padded zeroes; zint's loop XORs the remainder bits).
    let mut rem = vec![0u8; bits.len() + 8];
    rem[..bits.len()].copy_from_slice(&bits);
    for i in 0..bits.len() {
        if rem[i] == 1 {
            for (j, &g) in [1u8, 1, 1, 1, 0, 1, 0, 0, 1].iter().enumerate() {
                rem[i + j] ^= g;
            }
        }
    }
    let mut runs: Vec<u32> = Vec::with_capacity(8 + data.len() * 8 + 16 + 9);
    runs.extend(START);
    for &b in data {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'A'..=b'F' => b - b'A' + 10,
            _ => unreachable!(),
        };
        runs.extend(PLESSEY[v as usize].iter().map(|&e| e as u32));
    }
    for k in 0..8 {
        runs.extend(if rem[bits.len() + k] == 0 {
            [1, 3]
        } else {
            [3, 1]
        });
    }
    runs.extend(STOP);
    Ok(bars_fixed(&runs, module))
}

// --- ^BZ POSTNET (zint postal.c) -------------------------------------------

/// ^BZ (t = 0): POSTNET 5/9/11-digit ZIP with the guard bars and the
/// (10 - sum mod 10) check digit; e.g. the short bars print at 2/5 of the
/// height, like ^B5.
pub(crate) fn postnet(data: &[u8], module: u32, height: u32) -> Result<Pattern, String> {
    check_digits(data, "POSTNET (^BZ)")?;
    let sum: u32 = data.iter().map(|&d| u32::from(d - b'0')).sum();
    let check = (10 - sum % 10) % 10;
    let short_h = (2 * height.max(1)) / 5;
    let space = 3 * module / 2;
    let mut bars = Vec::with_capacity(2 + data.len() * 10);
    bars.push(Bar {
        width: module,
        black: true,
        height: None,
    }); // start guard
    for digit in data
        .iter()
        .copied()
        .chain(std::iter::once(b'0' + check as u8))
    {
        for &short in &PLANET[(digit - b'0') as usize] {
            bars.push(Bar {
                width: space,
                black: false,
                height: None,
            });
            bars.push(Bar {
                width: module,
                black: true,
                height: if short == 1 { Some(short_h) } else { None },
            });
        }
    }
    bars.push(Bar {
        width: space,
        black: false,
        height: None,
    });
    bars.push(Bar {
        width: module,
        black: true,
        height: None,
    }); // stop guard
    Ok(Pattern::Bars(bars))
}

// --- ^BS UPC/EAN add-on (zint upcean.c ean_add_on) --------------------------

/// Add-on digit element widths: EANsetA/B are bar,space,bar,space module
/// runs; the guard is 1,1,2 and the digit separator 1,1. Parity rows come
/// from GS1 Table 7.
#[rustfmt::skip]
const EAN_ADD_A: [[u32; 4]; 10] = [
    [3, 2, 1, 1], [2, 2, 2, 1], [2, 1, 2, 2], [1, 4, 1, 1], [1, 1, 3, 2],
    [1, 2, 3, 1], [1, 1, 1, 4], [1, 3, 1, 2], [1, 2, 1, 3], [3, 1, 1, 2],
];
#[rustfmt::skip]
const EAN_ADD_B: [[u32; 4]; 10] = [
    [1, 1, 2, 3], [1, 2, 2, 2], [2, 2, 1, 2], [1, 1, 4, 1], [2, 3, 1, 1],
    [2, 1, 3, 1], [1, 1, 1, 4], [2, 1, 1, 2], [2, 2, 1, 2], [3, 1, 1, 2],
];

/// EAN-2/EAN-5 add-on parity rows (GS1 Table 7; 0 = A digit set, 1 = B).
#[rustfmt::skip]
const EAN_ADD2_PARITY: [[u8; 2]; 4] = [
    [0, 0], [0, 1], [1, 0], [1, 1],
];
#[rustfmt::skip]
const EAN_ADD5_PARITY: [[u8; 5]; 10] = [
    [1, 1, 0, 0, 0], [1, 0, 1, 0, 0], [1, 0, 0, 1, 0], [1, 0, 0, 0, 1], [0, 1, 1, 0, 0],
    [0, 0, 1, 1, 0], [0, 0, 0, 1, 1], [0, 1, 0, 1, 0], [0, 1, 0, 0, 1], [0, 0, 1, 0, 1],
];

/// ^BS: the 2- or 5-digit publisher add-on extension (guard 1,1,2, digit
/// separators 1,1). The digit parity set follows the extension value
/// (EAN-2: value mod 4; EAN-5: 3-odd + 9-even weighted sum mod 10, GS1
/// Table 7). Fixed module widths; no ratio.
pub(crate) fn ean_addon(data: &[u8], module: u32) -> Result<Pattern, String> {
    check_digits(data, "UPC/EAN add-on (^BS)")?;
    if !matches!(data.len(), 2 | 5) {
        return Err("UPC/EAN add-on (^BS): 2- or 5-digit extension data is required".into());
    }
    let parity: [u8; 5] = if data.len() == 2 {
        let value = u32::from(data[0] - b'0') * 10 + u32::from(data[1] - b'0');
        let p = EAN_ADD2_PARITY[(value % 4) as usize];
        [p[0], p[1], 0, 0, 0]
    } else {
        let v: Vec<u32> = data.iter().map(|&d| u32::from(d - b'0')).collect();
        let sum = 3 * (v[0] + v[2] + v[4]) + 9 * (v[1] + v[3]);
        EAN_ADD5_PARITY[(sum % 10) as usize]
    };
    let mut runs: Vec<u32> = Vec::with_capacity(3 + data.len() * 4 + (data.len() - 1) * 2);
    runs.extend([1, 1, 2]); // guard
    for (i, &b) in data.iter().enumerate() {
        let d = (b - b'0') as usize;
        runs.extend(if parity[i] == 0 {
            EAN_ADD_A[d]
        } else {
            EAN_ADD_B[d]
        });
        if i + 1 < data.len() {
            runs.extend([1, 1]); // digit separator
        }
    }
    Ok(bars_fixed(&runs, module))
}
