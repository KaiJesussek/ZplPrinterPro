//! Byte-exact ~HS serializer tests (Task 20): the three-string golden
//! frame, the five suppression conditions, and flag/bit reflection.
//!
//! Expected bytes are hand-computed from the guide's field tables
//! (pages 233-236): String 1 aaa = 158 (9600 baud, 8 data bits, no
//! parity, 1 stop bit, DTR - see the task report for the bit math),
//! String 2 mmm = 000 (die-cut, sensors off, direct thermal), String 3
//! xxxx = 0000 with static RAM installed (y = 1).

use zpl_server::hs::hs_response;
use zpl_server::state::{Conditions, MediaType, PrinterState, PrintMode, Temp};

/// All-clear, freshly-powered printer (no labels received, no images):
/// the three STX...ETX-CRLF strings transcribed from the guide verbatim.
const GOLDEN: &[u8] = b"\x02158,0,0,0000,000,0,0,0,000,0,0,0\x03\r\n\
\x02000,0,0,0,0,2,0,0,00000000,1,000\x03\r\n\
\x020000,1\x03\r\n";

#[test]
fn hs_golden_bytes() {
    let out = hs_response(&PrinterState::default(), 0)
        .expect("an all-clear state must produce a response");
    assert_eq!(out, GOLDEN, "byte-exact three-string frame");

    // Frame asserts: exactly three lines (the split sees an empty fourth
    // piece after the final LF), each starting with STX and ending with
    // ETX CR LF.
    let lines: Vec<&[u8]> = out.split(|&b| b == b'\n').collect();
    assert_eq!(lines.len(), 4, "three CR-LF-terminated strings");
    assert!(lines[3].is_empty());
    for l in &lines[..3] {
        assert_eq!(l.first(), Some(&0x02), "each string starts with STX");
        assert_eq!(l.last(), Some(&0x0d), "each string ends with CR before LF");
        assert_eq!(l[l.len() - 2], 0x03, "ETX precedes the CR");
    }
}

#[test]
fn hs_suppressed_on_hard_conditions() {
    // Each of the guide's five no-response conditions (p.233 note):
    // MEDIA OUT, RIBBON OUT, HEAD OPEN, REWINDER FULL, HEAD OVER-TEMP.
    let conditions: Vec<(&str, Box<dyn Fn(&mut PrinterState)>)> = vec![
        ("paper out", Box::new(|s: &mut PrinterState| s.conditions.paper_out = true)),
        ("ribbon out", Box::new(|s: &mut PrinterState| s.conditions.ribbon_out = true)),
        ("head open", Box::new(|s: &mut PrinterState| s.conditions.head_open = true)),
        ("rewinder full", Box::new(|s: &mut PrinterState| s.conditions.rewind_full = true)),
        ("head over-temperature", Box::new(|s: &mut PrinterState| s.conditions.head_temp = Temp::OverTemp)),
    ];
    for (name, condition) in conditions {
        let mut s = PrinterState::default();
        condition(&mut s);
        assert_eq!(hs_response(&s, 0), None, "no response while {name}");
    }
}

#[test]
fn hs_flags_reflect_state() {
    // (b/o/p - paper out, head up, ribbon out - are suppression
    // conditions, so their "1" states are observationally dead in a
    // response by design: the guide's printer answers nothing. Those
    // setters are exercised by hs_suppressed_on_hard_conditions; the
    // reachable flags below are the rest of the state.)

    // pause -> c; warm head -> l (the only elevated-temperature bit; OverTemp
    // suppresses, so l=1 in a response means Warm).
    let mut s = PrinterState::default();
    s.conditions.pause = true;
    s.conditions.head_temp = Temp::Warm;
    let out = hs_response(&s, 0).expect("pause + warm do not suppress");
    let l1 = s1(&out);
    assert_eq!(field(&l1, 2), "1", "c = pause flag");
    assert_eq!(field(&l1, 11), "1", "l = over-temperature-range flag (Warm)");
    assert_eq!(field(&l1, 10), "0", "k = under-temperature range stays clear");

    // media/print-mode bits: TT mode -> q=1 and mmm bit 0; continuous
    // media -> mmm bit 7; comm diagnostics -> g=1 and mmm bit 5.
    let mut s = PrinterState::default();
    s.conditions.print_mode = PrintMode::ThermalTransfer;
    s.conditions.comm_diag = true;
    s.conditions.media_type = MediaType::Continuous;
    let out = hs_response(&s, 0).unwrap();
    let l1 = s1(&out);
    let l2 = s2(&out);
    assert_eq!(field(&l1, 6), "1", "g = comm diagnostics flag");
    assert_eq!(field(&l2, 4), "1", "q = thermal-transfer flag");
    // mmm = 1<<7 (continuous) | 1<<5 (comm diag) | 1<<0 (TT) = 0xA1 = 161.
    assert_eq!(field(&l2, 0), "161");

    // www = number of stored images, right-aligned in three digits.
    let out = hs_response(&PrinterState::default(), 3).unwrap();
    let l2 = s2(&out);
    assert_eq!(field(&l2, 10), "003");

    // v = 1 always, and the print-mode code r defaults to Tear-Off (2).
    let out = hs_response(&PrinterState::default(), 0).unwrap();
    let l2 = s2(&out);
    assert_eq!(field(&l2, 5), "2", "r = print mode");
    assert_eq!(field(&l2, 9), "1", "v = format-while-printing, always 1");
}

#[test]
fn hs_string3_static_ram_and_counts() {
    let mut s = PrinterState::default();
    s.conditions.static_ram = false;
    let out = hs_response(&s, 0).unwrap();
    assert!(out.ends_with(b"\x020000,0\x03\r\n"), "static RAM flag clears y");
}

/// First line ("String 1"), verbatim bytes between STX and ETX.
fn s1(resp: &[u8]) -> Vec<u8> {
    let l = resp.split(|&b| b == b'\n').nth(0).unwrap();
    l[1..l.len() - 2].to_vec() // strip STX ... ETX CR
}

fn s2(resp: &[u8]) -> Vec<u8> {
    let l = resp.split(|&b| b == b'\n').nth(1).unwrap();
    l[1..l.len() - 2].to_vec()
}

fn field(line: &[u8], idx: usize) -> String {
    String::from_utf8(line.split(|&b| b == b',').nth(idx).unwrap().to_vec()).unwrap()
}
