//! Shared graphic-data decoding for every ASCII-hex bitmap payload in the
//! language: `~DG` downloads (guide p.175), `~DY` .GRF image and font
//! downloads (p.181, 178-180), and `^GF` inline graphic fields (p.215,
//! whose A-format data "follows the format for other download commands").
//!
//! `~DG` and `^GF` are sibling specs with the same data grammar; both feed
//! through this one decoder so they cannot drift.

/// Decode ASCII-hex graphic data (guide p.175-176 and the alternative
/// compression scheme on p.1761): each hex character is one horizontal
/// nibble (4 dots), bytes assembled MSB-first, row-major, `w` bytes per
/// row, `t` bytes total. Compression adds:
/// - a run of repeat-count letters (`G..Y` = 1..19, `g..z` = 20..400 in
///   steps of 20; the counts of a run sum) followed by the hex digit to
///   repeat, e.g. `M6` = seven `6`s, `hB` = forty `B`s, `vMB` = 327 `B`s;
/// - `,` / `!` fill the rest of the current row with `0x00` / `0xFF`;
/// - `:` repeats the previous row;
/// - `;` terminates the data (legacy ZPL I form);
/// - CR/LF inside multi-line downloads are ignored.
///
/// Returns `None` on any malformed input (odd nibble count, unknown
/// character, row op mid-nibble, `:` without a previous row, decoded size
/// != `t`). Data after `t` bytes is ignored - the printer stops at the
/// declared size (guide ^GF: "any numbers sent after count is satisfied
/// are ignored").
pub(crate) fn decode_graphic_data(data: &[u8], t: usize, w: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(t);
    let mut i = 0;
    let mut hi: Option<u8> = None; // pending high nibble of an open byte
    while i < data.len() && out.len() < t {
        let c = data[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b';' => break,
            b',' | b'!' | b':' => {
                if hi.is_some() {
                    // Guide p.1762 shorthand: a trailing run of an ODD
                    // number of zeros is written "one zero and a single
                    // comma" - the comma closes the dangling 0 nibble into
                    // a 0x00 byte before filling the row. Any other
                    // dangling nibble (or `!`/`:` there) is malformed.
                    if c == b',' && hi == Some(0) {
                        out.push(0x00);
                        hi = None;
                    } else {
                        return None;
                    }
                }
                let fill = match c {
                    b',' => 0x00,
                    b'!' => 0xFF,
                    _ => {
                        // ':' repeats the previous full row (row start only).
                        if out.len() % w != 0 || out.len() < w {
                            return None;
                        }
                        let prev = out.len() - w;
                        if out.len() + w > t {
                            return None;
                        }
                        out.extend_from_within(prev..prev + w);
                        i += 1;
                        continue;
                    }
                };
                // Row fill: zeros/ones to the end of the current row, never
                // past the declared size (a row already complete at `t` -
                // the nibble-close case - fills nothing).
                if out.len() != t {
                    let n = w - (out.len() % w);
                    if out.len() + n > t {
                        return None;
                    }
                    out.extend(std::iter::repeat(fill).take(n));
                }
                i += 1;
            }
            // 0-9 A-F: a nibble.
            _ if c.is_ascii_hexdigit() => {
                let v = hex_val(c).unwrap(); // open nibble: holds the byte
                match hi {
                    Some(h) => {
                        out.push((h << 4) | v);
                        hi = None;
                    }
                    None => hi = Some(v),
                }
                i += 1;
            }
            // Repeat-count letters: a run of letters sums; the following
            // char must be the hex digit to repeat (p.1761).
            _ if repeat_count(c).is_some() => {
                let mut sum = 0usize;
                let mut j = i;
                while let Some(count) = data.get(j).copied().and_then(repeat_count) {
                    sum += count;
                    j += 1;
                }
                let Some(v) = data.get(j).copied().and_then(hex_val) else {
                    return None; // repeat run with no value digit
                };
                // Set bits by expanding `sum` nibbles of value `v`.
                let pending = usize::from(hi.is_some());
                if out.len() + (pending + sum) / 2 > t {
                    return None;
                }
                for _ in 0..sum {
                    match hi {
                        Some(h) => {
                            out.push((h << 4) | v);
                            hi = None;
                        }
                        None => hi = Some(v),
                    }
                }
                i = j + 1;
            }
            _ => return None, // unrecognized data character
        }
    }
    if hi.is_some() {
        return None; // odd nibble count
    }
    (out.len() == t).then_some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Compression repeat-count letter -> count (guide p.1761): `G`..`Y` =
/// 1..19, `g`..`z` = 20..400 in steps of 20 (combinable runs sum).
fn repeat_count(c: u8) -> Option<usize> {
    match c {
        b'G'..=b'Y' => Some(1 + (c - b'G') as usize),
        b'g'..=b'z' => Some(20 + 20 * (c - b'g') as usize),
        _ => None,
    }
}
