//! Code 128 encoder (^BC), ISO/IEC 15417 - the pattern every later 1D
//! symbology (Task 15) mirrors: parse ^FD data -> value sequence with
//! subset planning -> mod-103 check digit -> bar widths.
//!
//! Runtime behavior follows the ZPL II guide's ^BC section (p.94-99):
//! `>` invocation codes select subsets and special values (Table 2), a
//! leading run of four or more digits auto-starts Subset C (start-only:
//! mid-data digit runs stay put - corpus 09 measured), the default
//! start is Subset B. The sheet's legacy "Subset A data is digit pairs"
//! sentence dates from pre-mode firmware; mode A/D auto subsetting (the
//! guide's A-mode text) is implemented instead - literal ASCII data in
//! every subset, documented in the Task 9 report.

use crate::bc::{Bar, Pattern};

/// Start-code values.
const START_A: u8 = 103;
const START_B: u8 = 104;
const START_C: u8 = 105;
/// Control values used by the subset planner.
const CODE_A: u8 = 101;
const CODE_B: u8 = 100;
const CODE_C: u8 = 99;
const SHIFT: u8 = 98;
/// The check digit is mod 103; `STOP` is value 106.
const STOP: u8 = 106;

/// Six-element (bar, space, bar, space, bar, space) widths in modules for
/// values 0..=106, transcribed from the public ISO/IEC 15417 value table
/// (cross-checked: every pattern sums to 11 modules, the bar-module sums
/// are even and the space-module sums odd - the standard's parity rule).
#[rustfmt::skip]
const WIDTHS: [[u8; 6]; 107] = [
    // 0 = 2 1 2 2 2 2
    [2, 1, 2, 2, 2, 2],
    // 1 = 2 2 2 1 2 2
    [2, 2, 2, 1, 2, 2],
    // 2 = 2 2 2 2 2 1
    [2, 2, 2, 2, 2, 1],
    // 3 = 1 2 1 2 2 3
    [1, 2, 1, 2, 2, 3],
    // 4 = 1 2 1 3 2 2
    [1, 2, 1, 3, 2, 2],
    // 5 = 1 3 1 2 2 2
    [1, 3, 1, 2, 2, 2],
    // 6 = 1 2 2 2 1 3
    [1, 2, 2, 2, 1, 3],
    // 7 = 1 2 2 3 1 2
    [1, 2, 2, 3, 1, 2],
    // 8 = 1 3 2 2 1 2
    [1, 3, 2, 2, 1, 2],
    // 9 = 2 2 1 2 1 3
    [2, 2, 1, 2, 1, 3],
    // 10 = 2 2 1 3 1 2
    [2, 2, 1, 3, 1, 2],
    // 11 = 2 3 1 2 1 2
    [2, 3, 1, 2, 1, 2],
    // 12 = 1 1 2 2 3 2
    [1, 1, 2, 2, 3, 2],
    // 13 = 1 2 2 1 3 2
    [1, 2, 2, 1, 3, 2],
    // 14 = 1 2 2 2 3 1
    [1, 2, 2, 2, 3, 1],
    // 15 = 1 1 3 2 2 2
    [1, 1, 3, 2, 2, 2],
    // 16 = 1 2 3 1 2 2
    [1, 2, 3, 1, 2, 2],
    // 17 = 1 2 3 2 2 1
    [1, 2, 3, 2, 2, 1],
    // 18 = 2 2 3 2 1 1
    [2, 2, 3, 2, 1, 1],
    // 19 = 2 2 1 1 3 2
    [2, 2, 1, 1, 3, 2],
    // 20 = 2 2 1 2 3 1
    [2, 2, 1, 2, 3, 1],
    // 21 = 2 1 3 2 1 2
    [2, 1, 3, 2, 1, 2],
    // 22 = 2 2 3 1 1 2
    [2, 2, 3, 1, 1, 2],
    // 23 = 3 1 2 1 3 1
    [3, 1, 2, 1, 3, 1],
    // 24 = 3 1 1 2 2 2
    [3, 1, 1, 2, 2, 2],
    // 25 = 3 2 1 1 2 2
    [3, 2, 1, 1, 2, 2],
    // 26 = 3 2 1 2 2 1
    [3, 2, 1, 2, 2, 1],
    // 27 = 3 1 2 2 1 2
    [3, 1, 2, 2, 1, 2],
    // 28 = 3 2 2 1 1 2
    [3, 2, 2, 1, 1, 2],
    // 29 = 3 2 2 2 1 1
    [3, 2, 2, 2, 1, 1],
    // 30 = 2 1 2 1 2 3
    [2, 1, 2, 1, 2, 3],
    // 31 = 2 1 2 3 2 1
    [2, 1, 2, 3, 2, 1],
    // 32 = 2 3 2 1 2 1
    [2, 3, 2, 1, 2, 1],
    // 33 = 1 1 1 3 2 3
    [1, 1, 1, 3, 2, 3],
    // 34 = 1 3 1 1 2 3
    [1, 3, 1, 1, 2, 3],
    // 35 = 1 3 1 3 2 1
    [1, 3, 1, 3, 2, 1],
    // 36 = 1 1 2 3 1 3
    [1, 1, 2, 3, 1, 3],
    // 37 = 1 3 2 1 1 3
    [1, 3, 2, 1, 1, 3],
    // 38 = 1 3 2 3 1 1
    [1, 3, 2, 3, 1, 1],
    // 39 = 2 1 1 3 1 3
    [2, 1, 1, 3, 1, 3],
    // 40 = 2 3 1 1 1 3
    [2, 3, 1, 1, 1, 3],
    // 41 = 2 3 1 3 1 1
    [2, 3, 1, 3, 1, 1],
    // 42 = 1 1 2 1 3 3
    [1, 1, 2, 1, 3, 3],
    // 43 = 1 1 2 3 3 1
    [1, 1, 2, 3, 3, 1],
    // 44 = 1 3 2 1 3 1
    [1, 3, 2, 1, 3, 1],
    // 45 = 1 1 3 1 2 3
    [1, 1, 3, 1, 2, 3],
    // 46 = 1 1 3 3 2 1
    [1, 1, 3, 3, 2, 1],
    // 47 = 1 3 3 1 2 1
    [1, 3, 3, 1, 2, 1],
    // 48 = 3 1 3 1 2 1
    [3, 1, 3, 1, 2, 1],
    // 49 = 2 1 1 3 3 1
    [2, 1, 1, 3, 3, 1],
    // 50 = 2 3 1 1 3 1
    [2, 3, 1, 1, 3, 1],
    // 51 = 2 1 3 1 1 3
    [2, 1, 3, 1, 1, 3],
    // 52 = 2 1 3 3 1 1
    [2, 1, 3, 3, 1, 1],
    // 53 = 2 1 3 1 3 1
    [2, 1, 3, 1, 3, 1],
    // 54 = 3 1 1 1 2 3
    [3, 1, 1, 1, 2, 3],
    // 55 = 3 1 1 3 2 1
    [3, 1, 1, 3, 2, 1],
    // 56 = 3 3 1 1 2 1
    [3, 3, 1, 1, 2, 1],
    // 57 = 3 1 2 1 1 3
    [3, 1, 2, 1, 1, 3],
    // 58 = 3 1 2 3 1 1
    [3, 1, 2, 3, 1, 1],
    // 59 = 3 3 2 1 1 1
    [3, 3, 2, 1, 1, 1],
    // 60 = 3 1 4 1 1 1
    [3, 1, 4, 1, 1, 1],
    // 61 = 2 2 1 4 1 1
    [2, 2, 1, 4, 1, 1],
    // 62 = 4 3 1 1 1 1
    [4, 3, 1, 1, 1, 1],
    // 63 = 1 1 1 2 2 4
    [1, 1, 1, 2, 2, 4],
    // 64 = 1 1 1 4 2 2
    [1, 1, 1, 4, 2, 2],
    // 65 = 1 2 1 1 2 4
    [1, 2, 1, 1, 2, 4],
    // 66 = 1 2 1 4 2 1
    [1, 2, 1, 4, 2, 1],
    // 67 = 1 4 1 1 2 2
    [1, 4, 1, 1, 2, 2],
    // 68 = 1 4 1 2 2 1
    [1, 4, 1, 2, 2, 1],
    // 69 = 1 1 2 2 1 4
    [1, 1, 2, 2, 1, 4],
    // 70 = 1 1 2 4 1 2
    [1, 1, 2, 4, 1, 2],
    // 71 = 1 2 2 1 1 4
    [1, 2, 2, 1, 1, 4],
    // 72 = 1 2 2 4 1 1
    [1, 2, 2, 4, 1, 1],
    // 73 = 1 4 2 1 1 2
    [1, 4, 2, 1, 1, 2],
    // 74 = 1 4 2 2 1 1
    [1, 4, 2, 2, 1, 1],
    // 75 = 2 4 1 2 1 1
    [2, 4, 1, 2, 1, 1],
    // 76 = 2 2 1 1 1 4
    [2, 2, 1, 1, 1, 4],
    // 77 = 4 1 3 1 1 1
    [4, 1, 3, 1, 1, 1],
    // 78 = 2 4 1 1 1 2
    [2, 4, 1, 1, 1, 2],
    // 79 = 1 3 4 1 1 1
    [1, 3, 4, 1, 1, 1],
    // 80 = 1 1 1 2 4 2
    [1, 1, 1, 2, 4, 2],
    // 81 = 1 2 1 1 4 2
    [1, 2, 1, 1, 4, 2],
    // 82 = 1 2 1 2 4 1
    [1, 2, 1, 2, 4, 1],
    // 83 = 1 1 4 2 1 2
    [1, 1, 4, 2, 1, 2],
    // 84 = 1 2 4 1 1 2
    [1, 2, 4, 1, 1, 2],
    // 85 = 1 2 4 2 1 1
    [1, 2, 4, 2, 1, 1],
    // 86 = 4 1 1 2 1 2
    [4, 1, 1, 2, 1, 2],
    // 87 = 4 2 1 1 1 2
    [4, 2, 1, 1, 1, 2],
    // 88 = 4 2 1 2 1 1
    [4, 2, 1, 2, 1, 1],
    // 89 = 2 1 2 1 4 1
    [2, 1, 2, 1, 4, 1],
    // 90 = 2 1 4 1 2 1
    [2, 1, 4, 1, 2, 1],
    // 91 = 4 1 2 1 2 1
    [4, 1, 2, 1, 2, 1],
    // 92 = 1 1 1 1 4 3
    [1, 1, 1, 1, 4, 3],
    // 93 = 1 1 1 3 4 1
    [1, 1, 1, 3, 4, 1],
    // 94 = 1 3 1 1 4 1
    [1, 3, 1, 1, 4, 1],
    // 95 = 1 1 4 1 1 3
    [1, 1, 4, 1, 1, 3],
    // 96 = 1 1 4 3 1 1
    [1, 1, 4, 3, 1, 1],
    // 97 = 4 1 1 1 1 3
    [4, 1, 1, 1, 1, 3],
    // 98 = 4 1 1 3 1 1
    [4, 1, 1, 3, 1, 1],
    // 99 = 1 1 3 1 4 1
    [1, 1, 3, 1, 4, 1],
    // 100 = 1 1 4 1 3 1
    [1, 1, 4, 1, 3, 1],
    // 101 = 3 1 1 1 4 1
    [3, 1, 1, 1, 4, 1],
    // 102 = 4 1 1 1 3 1
    [4, 1, 1, 1, 3, 1],
    // 103 = 2 1 1 4 1 2  (start A)
    [2, 1, 1, 4, 1, 2],
    // 104 = 2 1 1 2 1 4  (start B)
    [2, 1, 1, 2, 1, 4],
    // 105 = 2 1 1 2 3 2  (start C)
    [2, 1, 1, 2, 3, 2],
    // 106 = 2 3 3 1 1 1  (stop, six elements)
    [2, 3, 3, 1, 1, 1],
];

/// The printed stop: the six-element stop plus the standard trailing
/// 2-module bar - seven elements, 13 modules.
const STOP_WIDTHS: [u8; 7] = [2, 3, 3, 1, 1, 1, 2];

/// A parsed piece of ^FD data: a literal byte (^FH already applied by the
/// parser) or a pre-resolved value code from a `>` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Char(u8),
    /// A control/start value from the guide's Table 2 (FNC1 = 102, CODE C =
    /// 99, a start code = 103..105, ...).
    Val(u8),
}

/// The guide's Table 2 invocation codes: `>` followed by one character maps
/// to a value (62 `=`; 30 `>`; 94 `~`; 95 US/DEL; 96 FNC3; 97 FNC2; 98
/// SHIFT; 99 CODE C; 100 CODE B; 101 CODE A; 102 FNC1; 103..105 starts).
/// An unrecognized `>x` stays a literal `>` (defensive; never a panic).
fn parse_items(data: &[u8]) -> Vec<Item> {
    let mut items = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] != b'>' {
            items.push(Item::Char(data[i]));
            i += 1;
            continue;
        }
        let value = match data.get(i + 1).copied() {
            Some(b'<') => Some(62),
            Some(b'0') => Some(30),
            Some(b'=') => Some(94),
            Some(b'1') => Some(95),
            Some(b'2') => Some(96),
            Some(b'3') => Some(97),
            Some(b'4') => Some(98),
            Some(b'5') => Some(99),
            Some(b'6') => Some(100),
            Some(b'7') => Some(101),
            Some(b'8') => Some(102),
            Some(b'9') => Some(START_A),
            Some(b':') => Some(START_B),
            Some(b';') => Some(START_C),
            _ => None,
        };
        match value {
            Some(v) => {
                items.push(Item::Val(v));
                i += 2;
            }
            None => {
                items.push(Item::Char(b'>'));
                i += 1;
            }
        }
    }
    items
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subset {
    A,
    B,
    C,
}

/// Length of the run of consecutive digit characters starting at `from`
/// (used only for the start-subset decision).
fn digit_run(items: &[Item], from: usize) -> usize {
    let mut n = 0;
    for item in &items[from..] {
        match item {
            Item::Char(c) if c.is_ascii_digit() => n += 1,
            _ => break,
        }
    }
    n
}

/// Emit one literal character in the current subset, switching subsets when
/// the character is not representable. Value mapping per ISO/IEC 15417
/// (the guide's Table 3, p.97 - the two agree exactly):
/// `B` holds ASCII 32..127 as value c-32; `A` holds ASCII 32..95 (SP.._)
/// as c-32 and ASCII 0..31 (controls) as c+64; `C` holds digit pairs.
/// ASCII 96..127 is not in A (`96`/`101` are FNC symbols there) and
/// switches to B. Bytes above 127 substitute `?` - Code 128 is a 7-bit
/// symbology and real printers substitute the '?' character. Values never
/// exceed 105 here, so WIDTHS indexing in `bars` cannot go out of bounds.
fn emit_char(vals: &mut Vec<u8>, subset: &mut Subset, c: u8) {
    let (switch, value) = match *subset {
        Subset::A => match c {
            0..=31 => (None, c + 64),           // controls NUL..US
            32..=95 => (None, c - 32),          // SP.._
            96..=127 => (Some(CODE_B), c - 32), // '`'..DEL live in B
            _ => (None, 31),                    // '?'
        },
        Subset::B => match c {
            32..=127 => (None, c - 32),
            0..=31 => (Some(CODE_A), c + 64),
            _ => (None, 31), // '?'
        },
        Subset::C => {
            // C cannot carry a single character; leave for A (controls) or
            // B (the rest, including substituted '?').
            let (code, target, value) = if c < 32 {
                (CODE_A, Subset::A, c + 64)
            } else if c <= 127 {
                (CODE_B, Subset::B, c - 32)
            } else {
                (CODE_B, Subset::B, 31) // '?'
            };
            *subset = target;
            (Some(code), value)
        }
    };
    if let Some(code) = switch {
        vals.push(code);
    }
    vals.push(value);
}

/// Turn the parsed items into the value sequence: start code + data values
/// with the subset switches the guide and ISO/IEC 15417 require.
///
/// Start choice (guide): an explicit `>9`/`>:`/`>;` at the very beginning
/// selects the subset; otherwise Subset B - except that a leading run of
/// four or more (or an all-digit) payload auto-starts Subset C. The
/// auto-C decision applies at the START only: digit runs that appear
/// after letters stay in the current subset, matching the reference's
/// codeword sequence (corpus 09 - see the bc128_midstream_digit_run
/// test).
fn plan(items: &[Item]) -> Vec<u8> {
    let mut vals = Vec::with_capacity(items.len() + 2);

    // Explicit start code in front.
    let lead_start = match items.first() {
        Some(Item::Val(v)) if (START_A..=START_C).contains(v) => Some(*v),
        _ => None,
    };
    // Otherwise: the first literal decides - a 4+ digit run starts C.
    let auto_c = match lead_start {
        Some(_) => false,
        None => {
            let first_literal = items.iter().position(|i| matches!(i, Item::Char(_)));
            matches!(first_literal, Some(pos) if digit_run(items, pos) >= 4)
        }
    };
    let start = lead_start.unwrap_or(if auto_c { START_C } else { START_B });
    vals.push(start);
    let mut subset = match start {
        START_A => Subset::A,
        START_C => Subset::C,
        _ => Subset::B,
    };

    // The explicit start item was consumed as the start code; walk the rest.
    let mut i = if lead_start.is_some() { 1 } else { 0 };
    // `>4` SHIFT: the next data character is encoded with the other of
    // A/B's mapping, without leaving the subset.
    let mut shift = false;
    while i < items.len() {
        match items[i] {
            Item::Val(v) => {
                vals.push(v);
                match v {
                    SHIFT => shift = true,
                    CODE_C => subset = Subset::C,
                    CODE_B => {
                        // Value 100 is Code B outside Subset B; inside B it
                        // is FNC4 (guide Table 2: `>6` in B = FNC 4).
                        if subset != Subset::B {
                            subset = Subset::B;
                        }
                    }
                    CODE_A
                        // Value 101 is Code A outside Subset A; inside A it
                        // is FNC4.
                        if subset != Subset::A => {
                            subset = Subset::A;
                        }
                    _ => {}
                }
                i += 1;
            }
            Item::Char(c) if c.is_ascii_digit() => {
                // No mid-stream auto-switch: the reference keeps literal
                // digit runs in the current subset (measured corpus 09
                // "(01)12345678901234(10)ABC123": the 14-digit run stays
                // all Subset B - only a leading all-digit payload starts
                // in C, see `plan`).
                if subset == Subset::C {
                    // Pair digits in C; an unpaired digit (odd tail) leaves
                    // through Code B.
                    if let Item::Char(c2) = items.get(i + 1).copied().unwrap_or(Item::Char(0)) {
                        if c2.is_ascii_digit() {
                            vals.push((c - b'0') * 10 + (c2 - b'0'));
                            i += 2;
                            continue;
                        }
                    }
                    vals.push(CODE_B);
                    subset = Subset::B;
                    vals.push(c - b'0' + 16); // digit in B
                    i += 1;
                    continue;
                }
                // Short digit runs stay in the current subset.
                if shift && subset != Subset::C {
                    emit_shifted(&mut vals, subset, c);
                } else {
                    emit_char(&mut vals, &mut subset, c);
                }
                shift = false;
                i += 1;
            }
            Item::Char(c) => {
                if shift && subset != Subset::C {
                    emit_shifted(&mut vals, subset, c);
                } else {
                    emit_char(&mut vals, &mut subset, c);
                }
                shift = false;
                i += 1;
            }
        }
    }
    vals
}

/// A pending SHIFT: encode `c` with the other of A/B's mapping, with no
/// lasting subset change (a value emitted under SHIFT still scans as that
/// value). SHIFT in C is undefined by the standard; the flag is dropped.
fn emit_shifted(vals: &mut Vec<u8>, subset: Subset, c: u8) {
    let mut other = match subset {
        Subset::A => Subset::B,
        Subset::B => Subset::A,
        Subset::C => Subset::C,
    };
    emit_char(vals, &mut other, c);
}

/// Mod-103 check digit: (start value + sum of value_i x position) mod 103, the
/// first data symbol at position 1. The start value contributes once, as
/// its own symbol. The accumulation is u64: a barcode body at the field
/// input ceiling (55 KB of data, positions up to ~55 K) sums to ~10^1^1 -
/// far past u32 - and only the residue matters, so widen, never wrap
/// (fuzz finding: `u32` overflow panicked in debug builds on large data).
fn checksum(vals: &[u8]) -> u8 {
    let mut sum = vals[0] as u64;
    for (i, &v) in vals.iter().enumerate().skip(1) {
        sum += (v as u64) * (i as u64);
    }
    (sum % 103) as u8
}

/// Expand the value sequence into alternating bar/space runs.
fn bars(vals: &[u8], module: u32) -> Pattern {
    let mut bars = Vec::with_capacity(vals.len() * 6 + 1);
    for &v in vals {
        let widths: &[u8] = if v == STOP {
            &STOP_WIDTHS
        } else {
            &WIDTHS[v as usize]
        };
        for (k, &w) in widths.iter().enumerate() {
            bars.push(Bar {
                width: (w as u32).saturating_mul(module),
                black: k % 2 == 0,
            });
        }
    }
    Pattern::Bars(bars)
}

/// Encode Code 128 from already-prepared `^FD` data (^FH-unhexed, UCC prep
/// applied by `bc::barcode_field`). `module` is the ^BY module width in
/// dots; the returned bar widths are in dots.
pub(crate) fn encode(data: &[u8], module: u32) -> Result<Pattern, String> {
    if data.is_empty() {
        return Err("Code 128: empty barcode data".into());
    }
    // ^BY module width is resolved before this is reached; 0 means a
    // caller that bypassed the defaults - fall back to the guide default.
    let module = if module == 0 { 2 } else { module };
    let items = parse_items(data);
    let mut vals = plan(&items);
    let check = checksum(&vals);
    vals.push(check);
    vals.push(STOP);
    Ok(bars(&vals, module))
}
