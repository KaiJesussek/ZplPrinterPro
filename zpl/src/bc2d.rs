//! Task 16: 2D barcode encoders - QR (^BQ), DataMatrix (^BX), PDF417 (^B7),
//! Aztec (^B0).
//!
//! All encoders return [`Pattern::Modules`] - a 1bpp module map, MSB-first
//! row-major, black = 1. QR and DataMatrix maps carry NO quiet zone: the
//! reference paints the symbol edge exactly at the ^FO (measured corpus
//! 11/16/12; the label margin is the quiet zone, same model as the 1D
//! codes). PDF417 (2 left/right) and Aztec (1) keep their ISO quiet
//! zones (no corpus measurement). The renderer paints map bits compactly
//! (bit = row*width + col) at modulexmodule dots.
//!
//! Standards implemented (algorithms cross-checked against the zint
//! reference model during development; see the Task 16 report):
//! - QR ISO/IEC 18004: byte mode, versions 1..10, levels L/M/Q/H, model 2,
//!   mask 0..7 (guide default 7). `ponytail:` versions 1..10 - the byte
//!   tables for v11..40 add no behavior, only more blocks; extend
//!   QR_DATA/QR_BLOCKS/QR_TOTAL/QR_CENTERS when a payload needs > 271 B.
//! - DataMatrix ISO/IEC 16022 ECC 200: ASCII encodation (digit pairs,
//!   upper-shift escape for >= 0x80), square 10^2..144^2 + the six standard
//!   rectangular sizes, Reed-Solomon over GF(256) 0x12d, Annex F placement.
//! - PDF417 ISO/IEC 15438: byte compaction, security 0..8 (2..512 EC cws),
//!   GF(929) RS, truncation.
//! - Aztec ISO/IEC 24778: binary (byte) encodation, compact/full, the
//!   d parameter (default / EC% / forced layers 101-104, 201-232).

use crate::bc::Pattern;
use crate::ir::BarcodeField;
use crate::reed_solomon::{Field, Rs};
use crate::tables::aztec_sizes_table::{
    AZTEC_COMPACT_DATA_SIZES, AZTEC_COMPACT_SIZES, AZTEC_DATA_SIZES, AZTEC_SIZES,
};
use crate::tables::aztec_table::{COMPACT_MAP, CORE_MAP};
use crate::tables::dm_table::{
    SIZE_BYTES, SIZE_DATABLOCK, SIZE_FH, SIZE_FW, SIZE_H, SIZE_RSBLOCK, SIZE_W,
};
use crate::tables::pdf_table::PDF_BITPATTERN;

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

/// A module grid under construction (row-major, MSB-first, black = 1).
struct Grid {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

impl Grid {
    fn new(w: usize, h: usize) -> Grid {
        Grid {
            w,
            h,
            data: vec![0u8; w * h],
        }
    }
    fn set(&mut self, x: usize, y: usize, black: bool) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = y * self.w + x;
        if black {
            self.data[i >> 3] |= 0x80 >> (i & 7);
        } else {
            self.data[i >> 3] &= !(0x80 >> (i & 7));
        }
    }
    fn get(&self, x: usize, y: usize) -> bool {
        if x >= self.w || y >= self.h {
            return false;
        }
        let i = y * self.w + x;
        self.data[i >> 3] & (0x80 >> (i & 7)) != 0
    }
    fn pattern(self, qleft: usize, qtop: usize, qright: usize, qbottom: usize) -> Pattern {
        let w = self.w + qleft + qright;
        let h = self.h + qtop + qbottom;
        let mut out = vec![0u8; (w * h).div_ceil(8)];
        for y in 0..self.h {
            for x in 0..self.w {
                if self.get(x, y) {
                    let i = (y + qtop) * w + (x + qleft);
                    out[i >> 3] |= 0x80 >> (i & 7);
                }
            }
        }
        Pattern::Modules {
            width: w as u32,
            height: h as u32,
            data: out,
        }
    }
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

/// Absorb the ^B7 row height (h) and derive rows/cols for PDF417.
fn pdf_row_height(f: &BarcodeField) -> usize {
    let h = f.height.max(1) as usize;
    h.clamp(1, 32000)
}

// ---------------------------------------------------------------------
// QR - ISO/IEC 18004
// ---------------------------------------------------------------------

/// Data codewords per EC level (rows L, M, Q, H) and version 1..10.
///
/// Note: v9-M / v10-M are 182 / 216 data codewords, NOT 180 / 198: the
/// EC codewords must divide evenly across the 5 blocks (v9-M 110 = 5x22,
/// v10-M 130 = 5x26 per ISO/IEC 18004 Table 9; the 180/112 and 198/148
/// variants that float around do not divide and cannot be interleaved -
/// they produce undecodable symbols). Cross-checked against the zxing/
/// rqrr version tables (v9-M: 3 short x 36 data + 2 long x 37 data;
/// v10-M: 4 x 43 + 1 x 44) and Nayuki's qrcodegen tables.
const QR_DATA: [[u16; 10]; 4] = [
    [19, 34, 55, 80, 108, 136, 156, 194, 232, 274], // L
    [16, 28, 44, 64, 86, 108, 124, 154, 182, 216],  // M
    [13, 22, 34, 48, 62, 76, 88, 110, 132, 154],    // Q
    [9, 16, 26, 36, 46, 60, 66, 86, 100, 122],      // H
];

/// EC blocks per level/version.
const QR_BLOCKS: [[u8; 10]; 4] = [
    [1, 1, 1, 1, 1, 2, 2, 2, 2, 4], // L
    [1, 1, 1, 2, 2, 4, 4, 4, 5, 5], // M
    [1, 1, 2, 2, 4, 4, 6, 6, 8, 8], // Q
    [1, 1, 2, 4, 4, 4, 5, 6, 8, 8], // H
];

/// Total codewords per version.
const QR_TOTAL: [u16; 10] = [26, 44, 70, 100, 134, 172, 196, 242, 292, 346];

/// Alignment-pattern centers per version (v1 has none).
const QR_CENTERS: [&[u8]; 10] = [
    &[],
    &[6, 18],
    &[6, 22],
    &[6, 26],
    &[6, 30],
    &[6, 34],
    &[6, 22, 38],
    &[6, 24, 42],
    &[6, 26, 46],
    &[6, 28, 50],
];

fn qr_level(ec: u8) -> usize {
    match ec {
        b'L' => 0,
        b'Q' => 2,
        b'H' => 3,
        _ => 1, // M (and the guide's "M for invalid values")
    }
}

/// Byte-mode capacity of version v at level (count field 8 bits v1..9, 16 v10).
fn qr_capacity(level: usize, v: usize) -> usize {
    let bits = QR_DATA[level][v - 1] as usize * 8;
    (bits - 4 - if v < 10 { 8 } else { 16 }) / 8
}

/// The data-codeword stream: mode 0100 | count | bytes | terminator | pad
/// (ISO/IEC 18004 6.4.9/6.4.10).
fn qr_data_codewords(capacity: usize, data: &[u8], version: usize) -> Vec<u8> {
    // Character count indicator: 8 bits v1-9, 16 bits v10+ (ISO Table 3).
    let countbits = if version >= 10 { 16 } else { 8 };
    let total_bits = capacity * 8;
    let mut bits: Vec<u8> = Vec::with_capacity(total_bits);
    for i in (0..4).rev() {
        bits.push(((0b0100 >> i) & 1) as u8);
    }
    for i in (0..countbits).rev() {
        bits.push((((data.len() as u32) >> i) & 1) as u8);
    }
    for &b in data {
        for i in (0..8).rev() {
            bits.push((b >> i) & 1);
        }
    }
    // Terminator: up to 4 zero bits, to the byte boundary, but never
    // past the capacity.
    let remain = total_bits - bits.len();
    let term = remain.min((8 - bits.len() % 8) % 8).min(4);
    bits.extend(std::iter::repeat_n(0, term));
    while !bits.len().is_multiple_of(8) && bits.len() < total_bits {
        bits.push(0);
    }
    let mut pad = 0xECu8;
    while bits.len() < total_bits {
        for i in (0..8).rev() {
            bits.push((pad >> i) & 1);
        }
        pad = if pad == 0xEC { 0x11 } else { 0xEC };
    }
    bits.chunks(8)
        .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
        .collect()
}

/// Per-block RS + interleaving (ISO/IEC 18004 8.6).
fn qr_interleave(level: usize, version: usize, data: &[u8]) -> Result<Vec<u8>, String> {
    let data_cw = QR_DATA[level][version - 1] as usize;
    let ecc_cw = QR_TOTAL[version - 1] as usize - data_cw;
    let blocks = QR_BLOCKS[level][version - 1] as usize;
    let short_len = data_cw / blocks;
    let qty_short = blocks - data_cw % blocks;
    // ISO/IEC 18004 8.6: the EC codewords divide EVENLY across the blocks
    // (each block gets ecc_cw/blocks); when the DATA codewords do not
    // divide evenly, the last (data_cw % blocks) blocks carry one extra
    // data codeword, emitted right after the uniform data columns, before
    // the EC interleave. (The v9-M/v10-M table values enforce the even EC
    // split; a floor/ceil EC split leaves symbols undecodable.)
    let ecc_len = ecc_cw / blocks;
    let rs = Rs::new(Field::Binary(0x11d), ecc_len, 0)?;
    let mut block_data: Vec<&[u8]> = Vec::with_capacity(blocks);
    let mut block_ecc: Vec<Vec<u16>> = Vec::with_capacity(blocks);
    let mut pos = 0;
    for b in 0..blocks {
        let len = short_len + usize::from(b >= qty_short);
        let block = &data[pos..pos + len];
        block_data.push(block);
        let ec = rs.encode(&block.iter().map(|&x| x as u16).collect::<Vec<_>>())?;
        block_ecc.push(ec);
        pos += len;
    }
    let mut out = Vec::with_capacity(data_cw + ecc_cw);
    for col in 0..short_len {
        out.extend(block_data.iter().map(|block| block[col]));
    }
    // The long blocks' extra data codewords (they are not EC).
    out.extend(block_data[qty_short..].iter().map(|block| block[short_len]));
    for col in 0..ecc_len {
        out.extend(block_ecc.iter().map(|block| block[col] as u8));
    }
    Ok(out)
}

/// 15-bit format sequence: BCH(15,5) with generator 0x537, XOR 0x5412.
/// Verified against ISO/IEC 18004 Annex C (M0..M7 = 5412 5125 5E7C 5B4B
/// 45F9 40CE 4F97 4AA0; L0 = 77C4, Q0 = 355F, H0 = 1689).
fn qr_format_seq(ec2: u8, mask: u8) -> u16 {
    let data = (ec2 << 3) | mask;
    let mut rem = data as u32;
    for _ in 0..10 {
        rem = (rem << 1) ^ (((rem >> 9) & 1) * 0x537);
    }
    (((data as u32) << 10) | rem) as u16 ^ 0x5412
}

/// 18-bit version info: BCH(18,6) with generator 0x1F25. Verified against
/// ISO/IEC 18004 Table D.1 (v7 = 07C94, v8 = 085BC, v9 = 09A99,
/// v10 = 0A4D3).
fn qr_version_info(v: u8) -> u32 {
    let mut rem = v as u32;
    for _ in 0..12 {
        rem = (rem << 1) ^ (((rem >> 11) & 1) * 0x1F25);
    }
    ((v as u32) << 12) | rem
}

/// Which of the eight QR masks flips cell (x, y) (ISO/IEC 18004 Table 21).
fn qr_mask_flip(x: usize, y: usize, pattern: usize) -> bool {
    match pattern {
        0 => (x + y).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (x + y).is_multiple_of(3),
        4 => (x / 3 + y / 2).is_multiple_of(2),
        5 => (x * y) % 2 + (x * y) % 3 == 0,
        6 => ((x * y) % 2 + (x * y) % 3).is_multiple_of(2),
        _ => ((x + y) % 2 + (x * y) % 3).is_multiple_of(2),
    }
}

pub fn qr(f: &BarcodeField, data: &[u8]) -> Result<Pattern, String> {
    // The reference strips a leading MM, marker from ^BQ field data:
    // corpus 11 renders the 17-byte HELLO WORLD 12345 (only v1-L can
    // hold that) and corpus 16 the bare URL. The marker never reaches
    // the symbol.
    let data = match data {
        [b'M', b'M', b',', rest @ ..] => rest,
        _ => data,
    };
    let level = qr_level(f.qr_ec);
    let mut version = 0;
    for v in 1..=10 {
        if data.len() <= qr_capacity(level, v) {
            version = v;
            break;
        }
    }
    if version == 0 {
        return Err("QR: data exceeds version 10 byte-mode capacity".into());
    }
    let size = 17 + 4 * version;
    let capacity = QR_DATA[level][version - 1] as usize;
    let cws = qr_data_codewords(capacity, data, version);
    let stream = qr_interleave(level, version, &cws)?;

    // Symbol grid; cell model mirrors the reference (0x01 = dark, 0x10 =
    // function module, 0x20 = format/timing area reserve).
    #[derive(Clone, Copy, PartialEq)]
    enum Cell {
        Data(u8),
        Func(bool), // finder/separator/alignment/timing: fixed value
        Reserved,   // format/version info areas (value written later)
        Timing(bool),
    }
    let mut g = vec![Cell::Data(0); size * size];
    let ix = |x: usize, y: usize| y * size + x;

    // Timing patterns (alternating, dark at (6,0)/(0,6)).
    for i in 0..size {
        let dark = i % 2 == 0;
        g[ix(i, 6)] = Cell::Timing(dark);
        g[ix(6, i)] = Cell::Timing(dark);
    }
    // Finder patterns at the three corners.
    for (fx, fy) in [(0usize, 0usize), (0, size - 7), (size - 7, 0)] {
        for y in 0..7 {
            for x in 0..7 {
                let dark = x == 0
                    || x == 6
                    || y == 0
                    || y == 6
                    || (2..=4).contains(&x) && (2..=4).contains(&y);
                g[ix(fx + x, fy + y)] = Cell::Func(dark);
            }
        }
    }
    // Separators.
    for i in 0..7 {
        g[ix(i, 7)] = Cell::Func(false);
        g[ix(7, i)] = Cell::Func(false);
        g[ix(size - 1 - i, 7)] = Cell::Func(false);
        g[ix(7, size - 1 - i)] = Cell::Func(false);
        g[ix(i, size - 8)] = Cell::Func(false);
        g[ix(size - 8, i)] = Cell::Func(false);
    }
    g[ix(7, 7)] = Cell::Func(false);
    g[ix(size - 8, 7)] = Cell::Func(false);
    g[ix(7, size - 8)] = Cell::Func(false);
    // Alignment patterns; skip only positions inside the three finder
    // patterns (centers on the timing axes are still placed, per ISO).
    if version > 1 {
        let centers = QR_CENTERS[version - 1];
        for &cy in centers {
            for &cx in centers {
                let (cx, cy) = (cx as usize, cy as usize);
                let in_finder =
                    (cx < 8 && cy < 8) || (cx >= size - 8 && cy < 8) || (cx < 8 && cy >= size - 8);
                if !in_finder {
                    for y in 0..5usize {
                        for x in 0..5usize {
                            let dark = x == 0 || x == 4 || y == 0 || y == 4 || (x == 2 && y == 2);
                            g[ix(cx + x - 2, cy + y - 2)] = Cell::Func(dark);
                        }
                    }
                }
            }
        }
    }
    // Format info reserves (both copies) + dark module. The timing
    // crossings (8,6)/(6,8) stay timing-dark (zint's `|= 0x20`).
    for i in 0..8 {
        if i != 6 {
            g[ix(i, 8)] = Cell::Reserved;
            g[ix(8, i)] = Cell::Reserved;
        }
        g[ix(size - 1 - i, 8)] = Cell::Reserved;
        g[ix(8, size - 1 - i)] = Cell::Reserved;
    }
    g[ix(8, 8)] = Cell::Reserved;
    g[ix(8, size - 8)] = Cell::Func(true); // dark module
                                           // Version info reserves.
    if version >= 7 {
        for i in 0..6 {
            g[ix(i, size - 11)] = Cell::Reserved;
            g[ix(i, size - 10)] = Cell::Reserved;
            g[ix(i, size - 9)] = Cell::Reserved;
            g[ix(size - 11, i)] = Cell::Reserved;
            g[ix(size - 10, i)] = Cell::Reserved;
            g[ix(size - 9, i)] = Cell::Reserved;
        }
    }

    // Populate the data stream in the zig-zag order (ISO 18004 8.7.1).
    let n = stream.len() * 8;
    let mut direction = true; // up
    let mut row = 0usize;
    let mut y = size - 1;
    let mut i = 0usize;
    while i < n {
        let mut x = size as isize - 2 - (row as isize) * 2;
        if x < 6 {
            x -= 1; // skip the vertical timing column
        }
        let x = x as usize;
        if let Cell::Data(_) = g[ix(x + 1, y)] {
            g[ix(x + 1, y)] = Cell::Data((stream[i >> 3] >> (7 - (i & 7))) & 1);
            i += 1;
        }
        if i < n {
            if let Cell::Data(_) = g[ix(x, y)] {
                g[ix(x, y)] = Cell::Data((stream[i >> 3] >> (7 - (i & 7))) & 1);
                i += 1;
            }
        }
        if direction {
            if y == 0 {
                row += 1;
                y = 0;
                direction = false;
            } else {
                y -= 1;
            }
        } else {
            if y == size - 1 {
                row += 1;
                y = size - 1;
                direction = true;
            } else {
                y += 1;
            }
        }
    }

    // Mask (guide: explicit e 0-7, default 7) over data cells only.
    let pattern = (f.qr_mask as usize).min(7);
    for y in 0..size {
        for x in 0..size {
            if let Cell::Data(v) = g[ix(x, y)] {
                let dark = v == 1;
                g[ix(x, y)] = Cell::Data(u8::from(dark ^ qr_mask_flip(x, y, pattern)));
            }
        }
    }

    // Format info (two copies) and version info (v >= 7), written
    // directly into the reserved cells (ISO/IEC 18004 7.9, 8.10).
    let ec2 = match level {
        0 => 0b01, // L
        1 => 0b00, // M
        2 => 0b11, // Q
        _ => 0b10, // H
    };
    let seq = qr_format_seq(ec2, pattern as u8);
    let vi_seq = if version >= 7 {
        qr_version_info(version as u8)
    } else {
        0
    };
    let bit_at = |g: &mut Vec<Cell>, x: usize, y: usize, bit: bool| {
        g[ix(x, y)] = Cell::Data(u8::from(bit));
    };
    for i in 0..6 {
        bit_at(&mut g, 8, i, (seq >> i) & 1 == 1); // vertical, below top-left finder
    }
    for i in 0..8 {
        bit_at(&mut g, size - 1 - i, 8, (seq >> i) & 1 == 1); // horizontal, right of top-left
    }
    for i in 0..6 {
        bit_at(&mut g, 5 - i, 8, (seq >> (i + 9)) & 1 == 1); // second horizontal copy
    }
    for i in 0..7 {
        bit_at(&mut g, 8, size - 7 + i, (seq >> (i + 8)) & 1 == 1); // second vertical copy
    }
    bit_at(&mut g, 8, 7, (seq >> 6) & 1 == 1);
    bit_at(&mut g, 8, 8, (seq >> 7) & 1 == 1);
    bit_at(&mut g, 7, 8, (seq >> 8) & 1 == 1);
    if version >= 7 {
        // Zint qr_add_version_info(): (col i, row size-11+r) holds bit i*3+r.
        for i in 0..6 {
            bit_at(&mut g, i, size - 11, (vi_seq >> (i * 3)) & 1 == 1);
            bit_at(&mut g, i, size - 10, (vi_seq >> (i * 3 + 1)) & 1 == 1);
            bit_at(&mut g, i, size - 9, (vi_seq >> (i * 3 + 2)) & 1 == 1);
            bit_at(&mut g, size - 11, i, (vi_seq >> (i * 3)) & 1 == 1);
            bit_at(&mut g, size - 10, i, (vi_seq >> (i * 3 + 1)) & 1 == 1);
            bit_at(&mut g, size - 9, i, (vi_seq >> (i * 3 + 2)) & 1 == 1);
        }
    }

    // Resolve final module values.
    let mut grid = Grid::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let dark = match g[ix(x, y)] {
                Cell::Func(d) | Cell::Timing(d) => d,
                Cell::Data(v) => v == 1,
                Cell::Reserved => false, // replaced above; defensive
            };
            grid.set(x, y, dark);
        }
    }

    // No interior quiet zone: the reference paints the symbol at the
    // field origin with its edges exactly at the ^FO (measured: corpus
    // 11 ink bbox is the 21x21 symbol exactly; corpus 16 the 25x25) -
    // the label margin supplies the quiet zone, like the 1D codes.
    Ok(grid.pattern(0, 0, 0, 0))
}

// ---------------------------------------------------------------------
// DataMatrix - ISO/IEC 16022 ECC 200
// ---------------------------------------------------------------------

/// ASCII encodation: digit pairs -> 130..229, ASCII -> +1, >= 0x80 via
/// upper-shift escape 235 (+ value - 127). Returns the codeword count.
fn dm_codewords(data: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < data.len() {
        if i + 1 < data.len() && is_digit(data[i]) && is_digit(data[i + 1]) {
            out.push(130 + 10 * (data[i] - b'0') + (data[i + 1] - b'0'));
            i += 2;
        } else if data[i] < 128 {
            out.push(data[i] + 1);
            i += 1;
        } else {
            out.push(235);
            out.push(data[i] - 127);
            i += 1;
        }
    }
}

/// Smallest symbol that fits `count` codewords; `rect` selects the
/// six standard rectangular sizes (guide aspect 2).
fn dm_pick(count: usize, rect: bool) -> Result<usize, String> {
    let prefer: &[usize] = if rect {
        &[2, 4, 6, 10, 13, 20]
    } else {
        &[
            0, 1, 3, 5, 7, 9, 12, 15, 18, 23, 31, 34, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46,
            47,
        ]
    };
    for &i in prefer {
        if (SIZE_BYTES[i] as usize) >= count {
            return Ok(i);
        }
    }
    Err("DataMatrix: data too large (exceeds the 144x144/16x48 capacity)".into())
}

/// ECC 200 tail: first pad 129, then pads randomized by the 253-state
/// algorithm (ISO/IEC 16022 5.2.7.3).
fn dm_pad(target: &mut Vec<u8>, tail: usize) {
    target.push(129);
    for tp in target.len()..(target.len() + tail).saturating_sub(1) {
        let prn = (149 * (tp + 1)) % 253 + 1;
        let v = if prn <= 125 { 129 + prn } else { prn - 125 };
        target.push(v as u8);
    }
}

/// Annex F placement: per-region module map of (codeword<<3)|bit
/// (ISO/IEC 16022 Annex F; ported cell-for-cell from the reference).
fn dm_placement(nr: usize, nc: usize) -> Vec<u16> {
    let mut array = vec![0u16; nr * nc];
    let place = |array: &mut Vec<u16>, r0: isize, c0: isize, b: u8, p: u32| {
        let mut r = r0;
        let mut c = c0;
        if r < 0 {
            r += nr as isize;
            c += 4 - ((nr as isize + 4) % 8);
        }
        if c < 0 {
            c += nc as isize;
            r += 4 - ((nc as isize + 4) % 8);
        }
        if r >= nr as isize {
            r -= nr as isize;
        }
        array[r as usize * nc + c as usize] = ((p << 3) + b as u32) as u16;
    };
    let block = |array: &mut Vec<u16>, r: isize, c: isize, p: u32| {
        place(array, r - 2, c - 2, 7, p);
        place(array, r - 2, c - 1, 6, p);
        place(array, r - 1, c - 2, 5, p);
        place(array, r - 1, c - 1, 4, p);
        place(array, r - 1, c, 3, p);
        place(array, r, c - 2, 2, p);
        place(array, r, c - 1, 1, p);
        place(array, r, c, 0, p);
    };
    let corner_a = |array: &mut Vec<u16>, p: u32| {
        place(array, nr as isize - 1, 0, 7, p);
        place(array, nr as isize - 1, 1, 6, p);
        place(array, nr as isize - 1, 2, 5, p);
        place(array, 0, nc as isize - 2, 4, p);
        place(array, 0, nc as isize - 1, 3, p);
        place(array, 1, nc as isize - 1, 2, p);
        place(array, 2, nc as isize - 1, 1, p);
        place(array, 3, nc as isize - 1, 0, p);
    };
    let corner_b = |array: &mut Vec<u16>, p: u32| {
        place(array, nr as isize - 3, 0, 7, p);
        place(array, nr as isize - 2, 0, 6, p);
        place(array, nr as isize - 1, 0, 5, p);
        place(array, 0, nc as isize - 4, 4, p);
        place(array, 0, nc as isize - 3, 3, p);
        place(array, 0, nc as isize - 2, 2, p);
        place(array, 0, nc as isize - 1, 1, p);
        place(array, 1, nc as isize - 1, 0, p);
    };
    let corner_c = |array: &mut Vec<u16>, p: u32| {
        place(array, nr as isize - 3, 0, 7, p);
        place(array, nr as isize - 2, 0, 6, p);
        place(array, nr as isize - 1, 0, 5, p);
        place(array, 0, nc as isize - 2, 4, p);
        place(array, 0, nc as isize - 1, 3, p);
        place(array, 1, nc as isize - 1, 2, p);
        place(array, 2, nc as isize - 1, 1, p);
        place(array, 3, nc as isize - 1, 0, p);
    };
    let corner_d = |array: &mut Vec<u16>, p: u32| {
        place(array, nr as isize - 1, 0, 7, p);
        place(array, nr as isize - 1, nc as isize - 1, 6, p);
        place(array, 0, nc as isize - 3, 5, p);
        place(array, 0, nc as isize - 2, 4, p);
        place(array, 0, nc as isize - 1, 3, p);
        place(array, 1, nc as isize - 3, 2, p);
        place(array, 1, nc as isize - 2, 1, p);
        place(array, 1, nc as isize - 1, 0, p);
    };
    let mut p = 1u32;
    let mut r = 4isize;
    let mut c = 0isize;
    loop {
        if r == nr as isize && c == 0 {
            corner_a(&mut array, p);
            p += 1;
        }
        if r == nr as isize - 2 && c == 0 && !nc.is_multiple_of(4) {
            corner_b(&mut array, p);
            p += 1;
        }
        if r == nr as isize - 2 && c == 0 && nc % 8 == 4 {
            corner_c(&mut array, p);
            p += 1;
        }
        if r == nr as isize + 4 && c == 2 && nc.is_multiple_of(8) {
            corner_d(&mut array, p);
            p += 1;
        }
        // up/right diagonal sweep
        loop {
            if r < nr as isize && c >= 0 && array[r as usize * nc + c as usize] == 0 {
                block(&mut array, r, c, p);
                p += 1;
            }
            r -= 2;
            c += 2;
            if !(r >= 0 && c < nc as isize) {
                break;
            }
        }
        r += 1;
        c += 3;
        // down/left diagonal sweep
        loop {
            if r >= 0 && c < nc as isize && array[r as usize * nc + c as usize] == 0 {
                block(&mut array, r, c, p);
                p += 1;
            }
            r += 2;
            c -= 2;
            if !(r < nr as isize && c >= 0) {
                break;
            }
        }
        r += 3;
        c += 1;
        if !(r < nr as isize || c < nc as isize) {
            break;
        }
    }
    // Unfilled corner.
    if array[nr * nc - 1] == 0 {
        array[nr * nc - 1] = 1;
        array[nr * nc - nc - 2] = 1;
    }
    array
}

/// EC codewords with block interleave (144x144 skew per the reference).
fn dm_ecc(
    binary: &mut Vec<u8>,
    bytes: usize,
    datablock: usize,
    rsblock: usize,
    skew: bool,
) -> Result<(), String> {
    let blocks = (bytes + 2) / datablock;
    let rs = Rs::new(Field::Binary(0x12d), rsblock, 1)?;
    let total_ec = rsblock * blocks;
    // The skew branch writes EC codewords into the transmitted stream at
    // bytes + n +/- {2,8} - beyond the data region - so grow the buffer
    // first (the biggest symbol needs 1558 + 620 + 2 bytes).
    if skew {
        binary.resize(bytes + total_ec + 2, 0);
    }
    let mut ecc = vec![0u8; total_ec];
    for b in 0..blocks {
        let mut buf = Vec::with_capacity(datablock);
        let mut n = b;
        while n < bytes {
            buf.push(binary[n]);
            n += blocks;
        }
        let ec = rs.encode(&buf.iter().map(|&x| x as u16).collect::<Vec<_>>())?;
        let mut p = 0;
        let mut n = b;
        while n < total_ec {
            // The 144x144 symbol inverts the usual data/ECC ordering: the
            // EC tail is displaced by +2 (blocks 0..7) / -8 (rest) within
            // the transmitted stream, so the buffer needs the ECC space
            // beyond the data codewords (zint's 2200-byte binary array).
            if skew {
                if b < 8 {
                    binary[bytes + n + 2] = ec[p] as u8;
                } else {
                    binary[bytes + n - 8] = ec[p] as u8;
                }
            } else {
                ecc[n] = ec[p] as u8;
            }
            n += blocks;
            p += 1;
        }
    }
    if !skew {
        binary.extend_from_slice(&ecc);
    }
    Ok(())
}

pub fn datamatrix(f: &BarcodeField, data: &[u8]) -> Result<Pattern, String> {
    let rect = f.dm_aspect == 2;
    let mut cws = Vec::with_capacity(data.len() + data.len() / 2 + 2);
    dm_codewords(data, &mut cws);
    let count = cws.len();
    let idx = dm_pick(count, rect)?;
    let h = SIZE_H[idx] as usize;
    let w = SIZE_W[idx] as usize;
    let fh = SIZE_FH[idx] as usize;
    let fw = SIZE_FW[idx] as usize;
    let bytes = SIZE_BYTES[idx] as usize;
    let datablock = SIZE_DATABLOCK[idx] as usize;
    let rsblock = SIZE_RSBLOCK[idx] as usize;
    let tail = bytes - count;
    if tail > 0 {
        dm_pad(&mut cws, tail);
    }
    debug_assert_eq!(cws.len(), bytes);
    let mut binary = cws;
    dm_ecc(&mut binary, bytes, datablock, rsblock, idx == 47)?;
    let nr = h - 2 * (h / fh);
    let nc = w - 2 * (w / fw);
    let places = dm_placement(nr, nc);

    let mut grid = Grid::new(w, h);
    // Finder + clock tracks.
    let mut y = 0;
    while y < h {
        for x in 0..w {
            grid.set(x, h - y - 1, true);
        }
        for x in (0..w).step_by(2) {
            grid.set(x, y, true);
        }
        y += fh;
    }
    let mut x = 0;
    while x < w {
        for yy in 0..h {
            grid.set(x, h - yy - 1, true);
        }
        for yy in (0..h).step_by(2) {
            grid.set(x + fw - 1, h - yy - 1, true);
        }
        x += fw;
    }
    // Data.
    for py in 0..nr {
        for px in 0..nc {
            let v = places[(nr - py - 1) * nc + px];
            if v == 1 || (v > 7 && (binary[((v >> 3) - 1) as usize] & (1 << (v & 7))) != 0) {
                let mx = 1 + px + 2 * (px / (fw - 2));
                let my = h - 1 - (1 + py + 2 * (py / (fh - 2)));
                grid.set(mx, my, true);
            }
        }
    }
    // No interior quiet zone: the reference paints the symbol at the
    // field origin (measured: corpus 12 ink bbox is the 18x18 symbol
    // exactly) - the label margin supplies the quiet zone.
    Ok(grid.pattern(0, 0, 0, 0))
}

// ---------------------------------------------------------------------
// PDF417 - ISO/IEC 15438 (byte compaction)
// ---------------------------------------------------------------------

fn pdf_byte_compaction(data: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(data.len() + 2);
    if data.len() == 1 {
        out.push(901);
        out.push(data[0] as u16);
        return out;
    }
    out.push(if data.len().is_multiple_of(6) {
        924
    } else {
        901
    });
    let mut i = 0;
    while data.len() - i >= 6 {
        let mut v: u64 = 0;
        for &b in &data[i..i + 6] {
            v = (v << 8) | b as u64;
        }
        // 6 bytes in base 256 -> 5 codewords in base 900.
        for j in (0..5).rev() {
            let pow = 900u64.pow(j);
            out.push((v / pow) as u16);
            v %= pow;
        }
        i += 6;
    }
    while i < data.len() {
        out.push(data[i] as u16);
        i += 1;
    }
    out
}

pub fn pdf417(f: &BarcodeField, data: &[u8]) -> Result<Pattern, String> {
    let security = (f.pdf_security as usize).min(8);
    let ecc_cws = 2usize << security;
    let payload = pdf_byte_compaction(data);
    // rows/cols.
    let cols_given = (f.pdf_cols as usize) != 0;
    let rows_given = (f.pdf_rows as usize) != 0;
    let (cols, rows) = if cols_given && rows_given {
        (f.pdf_cols as usize, f.pdf_rows as usize)
    } else if cols_given {
        let c = f.pdf_cols as usize;
        let total = payload.len() + 1 + ecc_cws;
        let r = total / c + usize::from(!total.is_multiple_of(c));
        (c, r.max(3))
    } else if rows_given {
        let r = f.pdf_rows as usize;
        let c = (payload.len() + 1 + ecc_cws).div_ceil(r);
        if c > 30 {
            // Too many codewords for the requested rows: grow rows like
            // the reference until columns fit.
            let max_r = r.max(3);
            let mut rr = max_r;
            while rr <= 90 {
                let cc = (payload.len() + 1 + ecc_cws).div_ceil(rr);
                if cc <= 30 {
                    return pdf417_assemble(f, data, payload, ecc_cws, cc, rr);
                }
                rr += 1;
            }
            return Err("PDF417: too many codewords for the requested row count".into());
        }
        (c, r.max(3))
    } else {
        // Auto: roughly square, like the reference; never grow past the
        // 30-column limit - a payload that cannot fit leaves the guard
        // below to produce the Err (and the parse-time capacity check to
        // warn).
        let total = payload.len() + 1 + ecc_cws;
        let mut c = (((total as f64) - 1.0) / 3.0).sqrt().round() as usize;
        if c < 1 {
            c = 1;
        }
        let mut r = total.div_ceil(c);
        while (r > 90 || r * c > 928) && c < 30 {
            c += 1;
            r = total.div_ceil(c);
        }
        (c, r)
    };
    // Cap sanity.
    let total = payload.len() + 1 + ecc_cws;
    if total > 928 {
        return Err("PDF417: data too large (exceeds 928 codewords)".into());
    }
    if !(1..=30).contains(&cols) || !(3..=90).contains(&rows) || rows * cols < total {
        return Err("PDF417: invalid columns/rows for the data".into());
    }
    pdf417_assemble(f, data, payload, ecc_cws, cols, rows)
}

fn pdf417_assemble(
    f: &BarcodeField,
    _data: &[u8],
    payload: Vec<u16>,
    ecc_cws: usize,
    cols: usize,
    rows: usize,
) -> Result<Pattern, String> {
    let length = rows * cols;
    if length > 928 || payload.len() + 1 + ecc_cws > length {
        return Err("PDF417: too many codewords for the requested columns/rows".into());
    }
    // The symbol is filled completely: SD + payload + pads + EC = rows*cols,
    // so the pads fill what the data leaves after reserving the EC slots.
    let pads = length - 1 - payload.len() - ecc_cws;
    let mut cws = Vec::with_capacity(length);
    cws.push(0u16); // SD, fixed after padding
    cws.extend_from_slice(&payload);
    cws.extend(std::iter::repeat_n(900u16, pads));
    cws[0] = cws.len() as u16; // Symbol Length Descriptor = total data cws
    let rs = Rs::new(Field::Prime(929), ecc_cws, 1)?;
    let data_part = cws.clone();
    let ec = rs.encode(&data_part)?;
    cws.extend(ec);
    let truncated = f.pdf_truncate;
    let c1 = (rows - 1) / 3;
    let sec = (f.pdf_security as usize).min(8);
    let c2 = sec * 3 + (rows - 1) % 3;
    let c3 = cols - 1;
    let row_h = pdf_row_height(f);
    let row_w = if truncated {
        17 * (cols + 2) + 1
    } else {
        17 * (cols + 3) + 18
    };
    let mut grid = Grid::new(row_w, rows * row_h);
    for i in 0..rows {
        let k = (i / 3) * 30;
        let (left, right, cluster) = match i % 3 {
            0 => (k + c1, k + c3, 0),
            1 => (k + c2, k + c1, 1),
            _ => (k + c3, k + c2, 2),
        };
        let mut bits: Vec<bool> = Vec::with_capacity(17 * (cols + 3) + 18);
        pushn(&mut bits, 0x1FEA8, 17); // start
        push17(&mut bits, PDF_BITPATTERN[cluster * 929 + left]);
        // The whole codeword stream (SD at cws[0], then data, pads and EC)
        // is placed sequentially row-major, the SD occupying row 0 col 0
        // (ISO/IEC 15438 5.4.2; matches the reference encoder).
        for j in 0..cols {
            push17(
                &mut bits,
                PDF_BITPATTERN[cluster * 929 + cws[i * cols + j] as usize],
            );
        }
        if truncated {
            bits.push(true); // single-module right bar
        } else {
            push17(&mut bits, PDF_BITPATTERN[cluster * 929 + right]);
            push18(&mut bits, 0x3FA29); // stop
        }
        let w = bits.len();
        for (bit, &dark) in bits.iter().enumerate() {
            for r in 0..row_h {
                grid.set(bit, i * row_h + r, dark);
            }
        }
        debug_assert_eq!(
            w,
            if truncated {
                17 * (cols + 2) + 1
            } else {
                17 * (cols + 3) + 18
            }
        );
    }
    // Quiet zone: 2 modules left/right (ISO 15438 5.6.3).
    Ok(grid.pattern(2, 0, 2, 0))
}

fn push17(bits: &mut Vec<bool>, v: u16) {
    // The table values are 16-bit prefixes; the full 17-module pattern
    // is (v << 1) - the trailing module is implied.
    pushn(bits, (v as u32) << 1, 17);
}

fn push18(bits: &mut Vec<bool>, v: u32) {
    pushn(bits, v, 18);
}

// ---------------------------------------------------------------------
// Aztec - ISO/IEC 24778 (binary encodation)
// ---------------------------------------------------------------------

fn az_codeword_size(layers: usize) -> usize {
    if layers <= 2 {
        6
    } else if layers <= 8 {
        8
    } else if layers <= 22 {
        10
    } else {
        12
    }
}

/// Pure-binary encodation: one B/S run over the whole payload, with the
/// reference encoder's count rules (ISO/IEC 24778): <= 31 bytes get a
/// 5-bit count, 32..62 get two B/S latches (31 + rest), and longer runs
/// get a single B/S with the extended 16-bit (count - 31) prefix.
fn az_binary_bits(data: &[u8], out: &mut Vec<bool>) {
    let count = data.len();
    pushn(out, 31, 5); // B/S
    if count <= 31 {
        pushn(out, count as u32, 5);
        for &b in data {
            pushn(out, b as u32, 8);
        }
    } else if count <= 62 {
        pushn(out, 31, 5);
        for &b in &data[..31] {
            pushn(out, b as u32, 8);
        }
        pushn(out, 31, 5); // B/S
        pushn(out, (count - 31) as u32, 5);
        for &b in &data[31..] {
            pushn(out, b as u32, 8);
        }
    } else {
        pushn(out, (count - 31) as u32, 16);
        for &b in data {
            pushn(out, b as u32, 8);
        }
    }
}

fn pushn(bits: &mut Vec<bool>, v: u32, n: usize) {
    for i in (0..n).rev() {
        bits.push((v >> i) & 1 == 1);
    }
}

/// Bit-run stuffing (ISO/IEC 24778 7.3.1.2).
fn az_stuff(bits: &[bool], cw_size: usize) -> Vec<bool> {
    let mut out = Vec::with_capacity(bits.len() + bits.len() / cw_size + 1);
    let mut count = 0usize;
    for (i, &b) in bits.iter().enumerate() {
        if (out.len() + 1) % cw_size == 0 {
            if count == 0 || count == cw_size - 1 {
                out.push(count == 0);
                count = usize::from(b);
            } else {
                count = 0;
            }
        } else if b {
            count += 1;
        }
        out.push(b);
        let _ = i;
    }
    out
}

/// Pad with 1s to a codeword boundary; flip the final bit if the last
/// codeword became all 1s (ISO/IEC 24778 8.4).
fn az_pad(bits: &mut Vec<bool>, cw_size: usize) {
    let rem = bits.len() % cw_size;
    if rem != 0 {
        for _ in rem..cw_size {
            bits.push(true);
        }
        if bits[bits.len() - cw_size..].iter().all(|&b| b) {
            let last = bits.len() - 1;
            bits[last] = false;
        }
    }
}

/// The d parameter -> EC level index used with the capacity tables:
/// d = 0 default (23%), 01-99 EC% mapped to the standard's nearest
/// fixed levels (10/23/36/50).
fn az_level_index(d: u32) -> usize {
    match d {
        0 => 2,       // default 23%
        1..=10 => 1,  // 10%
        11..=23 => 2, // 23%
        24..=36 => 3, // 36%
        _ => 4,       // 50%
    }
}

/// Smallest compact (1..4) then full (1..32) symbol whose capacity at
/// the EC level fits the stuffed+padded data; capacity tables are in
/// data bits (ISO/IEC 24778 Table 8).
fn az_auto_layer(level: usize, data: &[bool]) -> Result<(bool, usize, usize), String> {
    let candidate = |compact: bool, l: usize| -> usize {
        if compact {
            AZTEC_COMPACT_DATA_SIZES[level - 1][l - 1] as usize
        } else {
            AZTEC_DATA_SIZES[level - 1][l - 1] as usize
        }
    };
    // Biggest compact boundary first (allows compact to be chosen at all).
    let max_compact = AZTEC_COMPACT_DATA_SIZES[level - 1][3] as usize;
    let max_full = AZTEC_DATA_SIZES[level - 1][31] as usize;
    let fits = |compact: bool, l: usize| {
        let cw = az_codeword_size(l);
        let stuffed = az_stuff(data, cw);
        let mut adjusted = stuffed;
        az_pad(&mut adjusted, cw);
        let max = candidate(compact, l);
        let fits = adjusted.len() <= max;
        (fits, adjusted.len(), max)
    };
    if data.len() <= max_compact {
        for l in 1..=4 {
            let (ok, _adj, max) = fits(true, l);
            if ok {
                return Ok((true, l, max));
            }
        }
    }
    if data.len() <= max_full {
        for l in 1..=32 {
            let (ok, _adj, max) = fits(false, l);
            if ok {
                return Ok((false, l, max));
            }
        }
    }
    Err("Aztec: data too large".into())
}

// ---------------------------------------------------------------------
// public entry
// ---------------------------------------------------------------------

/// Encode `data` (UTF-8 bytes of the decoded field text) for the 2D
/// symbologies. Never panics: hostile sizes -> Err.
pub fn encode_2d(f: &BarcodeField, data: &[u8]) -> Result<Pattern, String> {
    match f.symbology {
        b'Q' => qr(f, data),
        b'X' => datamatrix(f, data),
        b'7' => pdf417(f, data),
        b'0' => aztec(f, data),
        _ => Err("not a 2D symbology".into()),
    }
}

/// Parse-time capacity guard for ^FS validation: flags data that can
/// never fit, so the printer warns instead of silently skipping.
pub fn size_error(
    symbology: u8,
    qr_ec: u8,
    _qr_mask: u8,
    pdf_security: u8,
    len: usize,
) -> Option<String> {
    match symbology {
        b'Q' => {
            let level = qr_level(qr_ec);
            let cap = qr_capacity(level, 10);
            if len > cap {
                Some(format!(
                    "QR: {len} bytes exceed the version 10 capacity ({cap})"
                ))
            } else {
                None
            }
        }
        b'X' => {
            if len > 1558 {
                Some(format!(
                    "DataMatrix: {len} bytes exceed the 144x144 capacity (1558)"
                ))
            } else {
                None
            }
        }
        b'7' => {
            // Real byte-compaction codeword count (SD + latch + 6-to-5
            // groups + tail) like the encoder, not a bytes ~ codewords
            // estimate.
            let payload_cws = if len == 0 {
                0
            } else if len == 1 {
                2
            } else {
                1 + 5 * (len / 6) + (len % 6)
            };
            let total = 1 + payload_cws + (2usize << pdf_security as usize);
            if total > 928 {
                Some(format!(
                    "PDF417: {len} bytes exceed the 928 codeword capacity"
                ))
            } else {
                None
            }
        }
        b'0' => {
            if len > 1917 {
                Some(format!("Aztec: {len} bytes exceed the 32-layer capacity"))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn aztec(f: &BarcodeField, data: &[u8]) -> Result<Pattern, String> {
    // d = 300 (Rune), menu symbols and structured append are unsupported.
    if f.az_level == 300 {
        return Err("Aztec Rune (d=300) is not supported".into());
    }
    if f.az_menu {
        return Err("Aztec menu symbols are not supported".into());
    }
    if f.az_append > 1 {
        return Err("Aztec structured append is not supported".into());
    }
    if data.is_empty() {
        return Err("Aztec: no data".into());
    }

    let mut bits: Vec<bool> = Vec::with_capacity(data.len() * 8 + 16);
    az_binary_bits(data, &mut bits);

    // Layer selection.
    let (compact, layers, data_maxsize) = if (101..=104).contains(&f.az_level) {
        (
            true,
            (f.az_level - 100) as usize,
            (AZTEC_COMPACT_SIZES[(f.az_level - 101) as usize] as usize - 3)
                * az_codeword_size((f.az_level - 100) as usize),
        )
    } else if (201..=232).contains(&f.az_level) {
        (
            false,
            (f.az_level - 200) as usize,
            (AZTEC_SIZES[(f.az_level - 201) as usize] as usize - 3)
                * az_codeword_size((f.az_level - 200) as usize),
        )
    } else {
        az_auto_layer(az_level_index(f.az_level), &bits)?
    };
    let cw_size = az_codeword_size(layers);
    let stuffed = az_stuff(&bits, cw_size);
    let mut adjusted = stuffed.clone();
    az_pad(&mut adjusted, cw_size);
    let data_len = adjusted.len();
    if data_len > data_maxsize {
        return Err("Aztec: data too large for the selected symbol".into());
    }
    let num_data_cws = data_len / cw_size;
    let mut num_ecc_cws = if compact {
        AZTEC_COMPACT_SIZES[layers - 1] as usize - num_data_cws
    } else {
        AZTEC_SIZES[layers - 1] as usize - num_data_cws
    };
    if compact && layers == 4 {
        num_ecc_cws += 12; // spare codewords
    }
    if num_ecc_cws < 3 {
        return Err("Aztec: EC codeword count below minimum".into());
    }

    // Split into codewords and RS-encode (field by codeword size).
    let mut data_part: Vec<u32> = Vec::with_capacity(num_data_cws);
    for c in adjusted.chunks(cw_size) {
        let mut v = 0u32;
        for &b in c {
            v = (v << 1) | u32::from(b);
        }
        data_part.push(v);
    }
    let poly = match cw_size {
        6 => 0x43u32,
        8 => 0x12d,
        10 => 0x409,
        _ => 0x1069,
    };
    let rs = Rs::new(Field::Binary(poly), num_ecc_cws, 1)?;
    let ecc_part = rs.encode(&data_part.iter().map(|&v| v as u16).collect::<Vec<_>>())?;
    for &v in &ecc_part {
        pushn(&mut adjusted, v as u32, cw_size);
    }

    // Invert: data on the outside, EC on the inside.
    let total_bits = (num_data_cws + num_ecc_cws) * cw_size;
    let mut bit_pattern = vec![0u16; az_map_len()];
    for i in 0..total_bits {
        bit_pattern[i] = u16::from(adjusted[total_bits - i - 1]);
    }

    // Mode message descriptor.
    let mut descriptor = [0u16; 40];
    if compact {
        for (i, d) in descriptor.iter_mut().take(2).enumerate() {
            *d = (((layers - 1) >> (1 - i)) & 1) as u16;
        }
        for i in 0..6 {
            descriptor[2 + i] = (((num_data_cws - 1) >> (5 - i)) & 1) as u16;
        }
    } else {
        for (i, d) in descriptor.iter_mut().take(5).enumerate() {
            *d = (((layers - 1) >> (4 - i)) & 1) as u16;
        }
        for i in 0..11 {
            descriptor[5 + i] = (((num_data_cws - 1) >> (10 - i)) & 1) as u16;
        }
    }
    let desc_words = if compact { 8usize } else { 16usize };
    let mut desc_data = [0u16; 4];
    for i in 0..desc_words / 4 {
        let mut v = 0u16;
        for p in 0..4 {
            v = (v << 1) | descriptor[i * 4 + p];
        }
        desc_data[i] = v;
    }
    let desc_ecc_n = if compact { 5 } else { 6 };
    let rs16 = Rs::new(Field::Binary(0x13), desc_ecc_n, 1)?;
    let desc_ecc = rs16.encode(&desc_data[..desc_words / 4])?;
    let desc_total = desc_words / 4 + desc_ecc_n; // 7 (compact) or 10 (full)
    for i in 0..desc_ecc_n {
        for p in 0..4 {
            descriptor[desc_words + i * 4 + p] = (desc_ecc[i] >> (3 - p)) & 1;
        }
    }
    let desc_base = if compact { 1998 } else { 19998 };
    for (i, &b) in descriptor.iter().take(desc_total * 4).enumerate() {
        bit_pattern[desc_base + i] = b;
    }

    // Placement.
    let (offset, map_size, mapval) = if compact {
        let o = [6usize, 4, 2, 0][layers - 1];
        (o, 27usize, MapSrc::Compact)
    } else {
        let o = AZTEC_OFFSET[layers - 1];
        let map = az_populate_map(layers, o);
        (o, 151usize, MapSrc::Full(map))
    };
    let dim = map_size - 2 * offset;
    let mut grid = Grid::new(dim, dim);
    for row in offset..map_size - offset {
        for col in offset..map_size - offset {
            let v = match &mapval {
                MapSrc::Compact => COMPACT_MAP[row * 27 + col] as isize,
                MapSrc::Full(m) => m[row * 151 + col] as isize,
            };
            if v == 1 || (v >= 2 && bit_pattern[(v - 2) as usize] == 1) {
                grid.set(col - offset, row - offset, true);
            }
        }
    }
    // Quiet zone. 1 module.
    Ok(grid.pattern(1, 1, 1, 1))
}

fn az_map_len() -> usize {
    20040
}

enum MapSrc {
    Compact,
    Full(Vec<i16>),
}

/// Aztec full-symbol offsets (dim = 151 - 2*offset); the staggered
/// ISO/IEC 24778 sequence - dims 19,21,...,149,151 (the layer-12
/// offset drops by 3, not 2, and layers 26+/29+ drop by 3 too).
const AZTEC_OFFSET: [usize; 32] = [
    66, 64, 62, 60, 57, 55, 53, 51, 49, 47, 45, 42, 40, 38, 36, 34, 32, 30, 28, 25, 23, 21, 19, 17,
    15, 13, 10, 8, 6, 4, 2, 0,
];

fn az_avoid(output: usize) -> usize {
    if output > 10 {
        output + (output - 11) / 15 + 1
    } else {
        output
    }
}

/// The 151x151 spiral + core + reference guide bars.
fn az_populate_map(layers: usize, offset: usize) -> Vec<i16> {
    let endoffset = 151 - offset;
    let mut map = vec![0i16; 151 * 151];
    for layer in 0..layers {
        let start = 112 * layer + 16 * layer * layer + 2;
        let length = 28 + layer * 4 + (layer + 1) * 4;
        let mut n: usize = start;
        let mut end = start + length;
        let mut x: isize = 64 - (layer as isize) * 2;
        let mut y: isize = 63 - (layer as isize) * 2;
        // The reference places the two members of each ring pair with
        // CONSECUTIVE codeword numbers (n, n+1), one per outer and inner
        // ring cell - not the same number on both.
        // top
        let av0 = az_avoid(y as usize) * 151;
        let av1 = az_avoid((y - 1) as usize) * 151;
        while n < end {
            let ax = az_avoid(x as usize);
            map[av0 + ax] = n as i16;
            n += 1;
            map[av1 + ax] = n as i16;
            n += 1;
            x += 1;
        }
        // right
        x = 78 + (layer as isize) * 2;
        y = 64 - (layer as isize) * 2;
        let av0 = az_avoid(x as usize);
        let av1 = az_avoid((x + 1) as usize);
        end += length;
        while n < end {
            let ay = az_avoid(y as usize) * 151;
            map[ay + av0] = n as i16;
            n += 1;
            map[ay + av1] = n as i16;
            n += 1;
            y += 1;
        }
        // bottom
        x = 77 + (layer as isize) * 2;
        y = 78 + (layer as isize) * 2;
        let av0 = az_avoid(y as usize) * 151;
        let av1 = az_avoid((y + 1) as usize) * 151;
        end += length;
        while n < end {
            let ax = az_avoid(x as usize);
            map[av0 + ax] = n as i16;
            n += 1;
            map[av1 + ax] = n as i16;
            n += 1;
            x -= 1;
        }
        // left
        x = 63 - (layer as isize) * 2;
        y = 77 + (layer as isize) * 2;
        let av0 = az_avoid(x as usize);
        let av1 = az_avoid((x - 1) as usize);
        end += length;
        while n < end {
            let ay = az_avoid(y as usize) * 151;
            map[ay + av0] = n as i16;
            n += 1;
            map[ay + av1] = n as i16;
            n += 1;
            y -= 1;
        }
    }
    // Core.
    for y in 0..15 {
        for x in 0..15 {
            map[(y + 68) * 151 + x + 68] = CORE_MAP[y * 15 + x] as i16;
        }
    }
    // Reference guide bars.
    let mut y = if offset <= 11 {
        11usize
    } else {
        AZTEC_GRID_Y[(offset - 11) / 16]
    };
    while y < endoffset {
        for x in offset..endoffset {
            map[x * 151 + y] = (x & 1) as i16;
            map[y * 151 + x] = (x & 1) as i16;
        }
        y += 16;
    }
    map
}

/// First reference grid line per offset band (only reached for
/// offset > 11; bands are 16 wide like the offsets themselves).
const AZTEC_GRID_Y: [usize; 8] = [27, 43, 59, 75, 91, 107, 123, 139];
