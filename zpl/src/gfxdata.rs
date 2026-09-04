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
/// - `,` / `!` fill the rest of the current row with 0-nibbles (`0x00`
///   bytes) / 1-nibbles (`0x11` bytes - the fill is a single set bit,
///   not an all-ones byte);
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
    let mut hi: Option<u8> = None; // pending low nibble of an open byte
    // Nibble slots consumed in the current row; resets when a row reaches
    // its full width (2w nibbles). The reference-verified fill semantics
    // (20k-case differential against the p.1761 compression scheme)
    // hinge on this counter: ','/'!' always pad the CURRENT row to 2w,
    // closing a pending low nibble with the fill digit first.
    let mut row = 0usize;
    let nibble = |out: &mut Vec<u8>, hi: &mut Option<u8>, row: &mut usize, v: u8| {
        match *hi {
            Some(h) => {
                out.push((h << 4) | v);
                *hi = None;
            }
            None => *hi = Some(v),
        }
        *row += 1;
        if *row == 2 * w {
            *row = 0;
        }
    };
    while i < data.len() && out.len() < t {
        let c = data[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b';' => break,
            b',' | b'!' | b':' => {
                // A fill char with a pending low nibble closes it with the
                // fill's digit first (the odd-run shorthand: "...G3!" =
                // three ones then the fill 1, so the pending '3' pairs into
                // 0x31; guide p.1762's "odd zeros ... single comma" is the
                // ',' case of the same rule).
                if hi.is_some() {
                    // A dangling nibble may only be closed by a ',' or '!'
                    // fill (the odd-run shorthand); a ':' with a pending
                    // nibble is malformed - the encoder emits ':' only in
                    // lieu of a whole repeated line (row boundary).
                    if c == b':' {
                        return None;
                    }
                    out.push((hi.unwrap() << 4) | if c == b',' { 0 } else { 1 });
                    hi = None;
                    row += 1;
                    if row == 2 * w {
                        // The close itself completed the row: the encoder
                        // writes a fill char in lieu of a run that ENDS the
                        // line, so a completed row must not pad further.
                        row = 0;
                        i += 1;
                        continue;
                    }
                }
                if c == b':' {
                    // ':' repeats the previous full row (the encoder emits
                    // it in lieu of a repeated line; row start only).
                    if row != 0 || out.len() < w || out.len() + w > t {
                        return None;
                    }
                    let prev = out.len() - w;
                    out.extend_from_within(prev..prev + w);
                    i += 1;
                    continue;
                }
                // Row fill: pad the current row to its full width. The
                // encoder writes a fill char IN LIEU OF the row's final
                // run, so the pad always completes the row - a bare fill
                // pads a whole row, a run that already ended the row has
                // 'row' reset to 0 by then and pads a full row too (the
                // all-zeros/ones line case). ',' pads 0-nibbles (0x00
                // bytes); '!' pads 1-nibbles, which pack two per byte as
                // 0x11 - NOT 0xFF.
                let n = (2 * w - row) / 2;
                if out.len() + n > t {
                    return None;
                }
                out.extend(std::iter::repeat_n(if c == b',' { 0x00 } else { 0x11 }, n));
                row = 0;
                i += 1;
            }
            // 0-9 A-F: a nibble.
            _ if c.is_ascii_hexdigit() => {
                let v = hex_val(c).unwrap(); // open nibble: holds the byte
                nibble(&mut out, &mut hi, &mut row, v);
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
                for _ in 0..sum {
                    nibble(&mut out, &mut hi, &mut row, v);
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

/// Decode MIME Base64 - the `B64` form of Zebra's ZB64 encoding (guide
/// p.1602: the same MIME scheme used in e-mail; it emits only printable
/// ASCII, which is why a ZB64 payload never needs binary framing). The
/// standard alphabet plus `=` padding; CR/LF and other ASCII whitespace
/// are ignored (the guide permits new-lines inside encoded data). A final
/// 2- or 3-char group without padding also decodes. Any other byte is
/// malformed -> `None`.
pub(crate) fn decode_base64(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    for &b in data {
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break, // padding ends the stream
            _ => return None,
        };
        acc = (acc << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    Some(out)
}

/// Decode a PNG file into the emulator's 1bpp row-packed bitmap model
/// (MSB-first, `1` = black, rows padded to whole bytes - the same layout
/// `~DG`/`^GF` A-format builds). Any color type and bit depth are
/// accepted: pixels are luminance-thresholded at 50%, because printable
/// graphics are monochrome (guide ~DY: "graphic files must be monochrome
/// (black and white) or dithered"). Fully transparent pixels print white.
/// Returns `(width, height, data)`; `None` on malformed PNG data.
pub(crate) fn png_to_bitmap(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(png);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return None, // paletted input is expanded by normalize_to_color8
    };
    let row = crate::memory::packed_row_bytes(info.width);
    let mut out = vec![0u8; row * height];
    for y in 0..height {
        for x in 0..width {
            let off = (y * width + x) * channels;
            let dark = channel_dark(&buf[off..off + channels]);
            // Only RGBA carries a real alpha channel (the expansion keeps
            // grayscale+alpha as two); fully transparent pixels are white.
            let opaque = channels != 4 || buf[off + 3] >= 128;
            if dark && opaque {
                out[y * row + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    Some((info.width, info.height, out))
}

/// Whether a pixel's color is dark: the gray sample alone (gray/alpha) or
/// luminance below half (RGB/RGBA).
fn channel_dark(px: &[u8]) -> bool {
    match px.len() {
        1 | 2 => px[0] < 128,
        _ => luma(px) < 128,
    }
}

/// Perceptual luminance of an RGB pixel (Rec. 601 weights), 0-255.
fn luma(rgb: &[u8]) -> u8 {
    ((u32::from(rgb[0]) * 299 + u32::from(rgb[1]) * 587 + u32::from(rgb[2]) * 114) / 1000) as u8
}

#[cfg(test)]
mod tests {
    #[test]
    fn reference_compression_vectors_decode() {
        // Deterministic vectors generated against the p.1761
        // compression-scheme encoder (20k-case differential,
        // BinaryKits-verified semantics): edge constructs plus
        // seeded random grids incl. `:`-row-repeats.
        let cases: &[(&[u8], &[u8], usize)] = &[
            (b",", b"\x00\x00\x00\x00", 4),
            (b"NF", b"\xff\xff\xff\xff", 4),
            (b"G8,", b"\x80\x00", 2),
            (b"GE,,", b"\xe0\x00\x00\x00", 2),
            (b"!", b"\x11\x11\x11\x11", 4),
            (b"G1G2!!", b"\x12\x11\x11\x11", 2),
            (b"HAH0HA,", b"\xaa\x00\xaa\x00", 4),
            (b"GFG0GF,,", b"\xf0\xf0\x00\x00\x00\x00", 3),
            (b"GCGBG8GFG7GFG7GE", b"\xcb\x8f\x7f~", 4),
            (b"G0G4G8GAGBGAGFG3H8G7G2G8GCG1GDH4G2GDGBG8G9GD", b"\x04\x8a\xba\xf3\x88r\x8c\x1dD-\xb8\x9d", 3),
            (b"G6G4GD!H8G0GE", b"d\xd1\x88\x0e", 1),
            (b"GFG3H8G2GDG3GEGCG5GEG1G0G6GBGEG1G9HD", b"\xf3\x88->\xc5\xe1\x06\xbe\x19\xdd", 5),
            (b"G4G3GDGEGCGFG8H0GEG8GDG4G2G3G1GEGAG8GDGAGDGAG4GEGDG1GFGAGE", b"C\xde\xcf\x80\x0e\x8dB1\xea\x8d\xad\xa4\xed\x1f\xae", 3),
            (b"HDGEG0GBH6G5G9G3G7HCGFG3GE", b"\xdd\xe0\xb6e\x93|\xcf>", 4),
            (b"G4G3G9G4GCG6GEG8G0GEG6GCG4G6G4G7G5G8H4", b"C\x94\xc6\xe8\x0elFGXD", 2),
            (b"G1G0H9GFGDGBG6G5GFGAG5GEG0G2GAGEGCG5G4G2G9G1GAGDG6G5GFG0G9G7G5G7G8G3GAGCGAGBG9G0H4G1G9G6G7G8G4GDGBG8G5G7G3HFGCH0G9G1GFGD", b"\x10\x99\xfd\xb6_\xa5\xe0*\xecT)\x1a\xd6_\x09ux:\xca\xb9\x04A\x96xM\xb8W?\xfc\x00\x91\xfd", 8),
            (b"GBGDGBG2", b"\xbd\xb2", 1),
        ];
        for (comp, want, w) in cases {
            assert_eq!(
                decode_graphic_data(comp, want.len(), *w).as_deref(),
                Some(*want),
                "vector failed: {:?}",
                String::from_utf8_lossy(comp)
            );
        }
    }

    use super::*;

    #[test]
    fn base64_round_trips_with_and_without_padding() {
        let data = b"hello, binary world!";
        // 24 bytes: padded, unpadded, and whitespace-laced forms decode
        // to the same bytes.
        assert_eq!(decode_base64(b"aGVsbG8sIGJpbmFyeSB3b3JsZCE=").as_deref(), Some(data.as_slice()));
        assert_eq!(decode_base64(b"aGVsbG8sIGJpbmFyeSB3b3JsZCE").as_deref(), Some(data.as_slice()));
        assert_eq!(decode_base64(b"aGVsbG8s\nIGJpbmFyeSB3b3JsZCE=").as_deref(), Some(data.as_slice()));
        assert_eq!(decode_base64(b"aG!sbG8"), None); // '!' is not base64
    }

    #[test]
    fn png_to_bitmap_thresholds_gray_to_1bpp() {
        // 10x8 1-bit gray PNG (the shape ^GF P test payloads use): a
        // filled row decodes back to the same packed bits MSB-first.
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 10, 8);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            let mut w = enc.write_header().unwrap();
            // PNG grayscale: 0 = black, 1 = white (the ZPL model is the
            // inverse). Rows of 10 pixels: 8 black then 2 white, packed
            // MSB-first -> [0x00, 0xC0]; decoding flips them to 1-bits ->
            // [0xFF, 0x00].
            let row = [0x00u8, 0xC0];
            let data: Vec<u8> = (0..8).flat_map(|_| row).collect();
            w.write_image_data(&data).unwrap();
        }
        let (w, h, bits) = png_to_bitmap(&png).expect("valid PNG decodes");
        assert_eq!((w, h), (10, 8));
        let mut expect = Vec::new();
        for _ in 0..8 {
            expect.extend_from_slice(&[0xFF, 0x00]);
        }
        assert_eq!(bits, expect);
        assert_eq!(png_to_bitmap(b"not a png"), None);
    }
}
