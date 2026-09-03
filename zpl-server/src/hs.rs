//! Byte-exact ~HS host-status serializer (Task 20), transcribed from the
//! ZPL II guide pages 233-236. Three strings, each
//! `<STX>...<ETX><CR><LF>`:
//!
//! ```text
//! <STX>aaa,b,c,dddd,eee,f,g,h,iii,j,k,l<ETX><CR><LF>
//! <STX>mmm,n,o,p,q,r,s,t,uuuuuuuu,v,www<ETX><CR><LF>
//! <STX>xxxx,y<ETX><CR><LF>
//! ```
//!
//! Suppression (guide p.233 note): no response at all when the printer is
//! MEDIA OUT, RIBBON OUT, HEAD OPEN, REWINDER FULL, or HEAD
//! OVER-TEMPERATURE - modeled by the `None` return.

use crate::state::{PrinterState, PrintMode, Temp};

/// String-1 `aaa` - communication (interface) settings for the emulated
/// serial port: 9600 baud, 8 data bits, no parity, 1 stop bit, DTR
/// handshaking.
///
/// Bit layout (guide p.234, "aaa = a8 a7 a6 a5 a4 a3 a2 a1 a0"):
///
/// | bits | meaning | chosen |
/// |---|---|---|
/// | a8, a2, a1, a0 | baud-rate nibble (0 110 = 9600) | 0, 1, 1, 0 |
/// | a7 | handshake: 0 Xon/Xoff, 1 DTR | 1 (DTR) |
/// | a6 | parity: 0 odd, 1 even | 0 (odd - inert, parity disabled) |
/// | a5 | parity: 0 disable, 1 enable | 0 (disabled) |
/// | a4 | stop bits: 0 two, 1 one | 1 (one) |
/// | a3 | data bits: 0 seven, 1 eight | 1 (eight) |
///
/// -> 9-bit value 0 1001 1110 = 158 decimal, printed as `158`. (Even
/// parity would add bit a6 -> 222; Xon/Xoff clears a7 -> 30.)
pub const COMM_SETTINGS: u32 = 0b1001_1110;

/// String-3 field `xxxx` - firmware/model value. The guide documents only
/// the trailing `y` on this line; the plan rules to report the plain
/// constant `0000` (recorded in the task report).
pub const STRING3_MODEL: &str = "0000";

/// Serialize the host-status response, or `None` under any suppression
/// condition (paper out | ribbon out | head open | rewinder full | head
/// over-temperature).
pub fn hs_response(s: &PrinterState, images: usize) -> Option<Vec<u8>> {
    let c = &s.conditions;
    if c.paper_out
        || c.ribbon_out
        || c.head_open
        || c.rewind_full
        || c.head_temp == Temp::OverTemp
    {
        return None;
    }

    // String 2 `mmm` - function settings (guide p.235): eight bits read
    // m7 (media type: 0 die-cut / 1 continuous), m6 (sensor profile, off -
    // the emulator has no sensor profile), m5 (communications diagnostics),
    // m4..m1 (unused, 0), m0 (print mode: 0 direct thermal / 1 thermal
    // transfer). Default all-clear -> 000.
    let mmm = (c.media_type as u8) << 7 // m7
        | (c.comm_diag as u8) << 5 // m5
        | (matches!(c.print_mode, PrintMode::ThermalTransfer) as u8); // m0

    let mut out: Vec<u8> = Vec::with_capacity(96); // 3 lines, ~30 bytes each

    // String 1. `k` (under temperature) has no state to drive it - the
    // emulator's ladder has no cold rung - and `l` covers Warm and
    // OverTemp; OverTemp never reaches a response (suppression), so a
    // delivered `l=1` unambiguously means Warm.
    let s1 = format!(
        "{:03},{},{},{:04},{:03},{},{},{},{:03},{},{},{}",
        COMM_SETTINGS,
        c.paper_out as u8,                                     // b
        c.pause as u8,                                         // c
        s.label_length_dots.min(9999),                         // dddd
        s.formats_in_buffer.min(999),                          // eee
        s.buffer_full as u8,                                   // f
        c.comm_diag as u8,                                     // g
        s.partial_format as u8,                                // h
        0,                                                     // iii
        s.corrupt_ram as u8,                                   // j
        0,                                                     // k
        matches!(c.head_temp, Temp::Warm | Temp::OverTemp) as u8, // l
    );

    // String 2. `o` (head up) and `p` (ribbon out) map their conditions
    // directly; both are suppression conditions, so a delivered response
    // always shows 0 for them. `r` prints raw (codes 0-9; the guide's
    // single-letter Kiosk modes are out of scope for a u8 state field).
    let s2 = format!(
        "{:03},{},{},{},{},{},{},{},{:08},{},{:03}",
        mmm,                                       // mmm
        0,                                         // n (unused)
        c.head_open as u8,                         // o
        c.ribbon_out as u8,                        // p
        matches!(c.print_mode, PrintMode::ThermalTransfer) as u8, // q
        s.print_mode_code,                         // r
        s.print_width_mode,                        // s
        s.label_waiting as u8,                     // t
        s.labels_remaining.min(99_999_999),        // uuuuuuuu
        1,                                         // v (always 1)
        images.min(999),                           // www
    );

    // String 3.
    let s3 = format!("{STRING3_MODEL},{}", c.static_ram as u8);

    for line in [s1, s2, s3] {
        out.push(0x02); // STX
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\x03\r\n"); // ETX CR LF
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Conditions, MediaType, PrinterState, PrintMode, Temp};

    #[test]
    fn comm_settings_bit_math() {
        // The transcription's anchor: 9600-8-N-1-DTR must be 158.
        assert_eq!(COMM_SETTINGS, 158);
    }
}
