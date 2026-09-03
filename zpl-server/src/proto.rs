//! Host-query responders: ~HQ, ~WD, ~HI, and ^HV (Task 20). The ~HS
//! three-string serializer lives in `hs.rs`; the parse-level recording of
//! these requests lives in the zpl crate (`ParseContext::queries`).
//!
//! Framing follows the guide: ~HQ responses start with STX, join lines
//! with CRLF, and end with ETX (p.227 "Comments"); ~HI is a bare line
//! (p.224 shows no framing); ^HV is raw data (p.239 example).

use std::collections::BTreeMap;

use zpl::state::HostValueRequest;

use crate::state::PrinterState;

/// The emulated printer's factory serial number (~HQSN, ~WD).
pub const EMULATOR_SERIAL: &str = "ZPLDEMO0001";
/// Firmware version string (~WD, ~HI; the guide's ~HS print-mode footnote
/// references V60.14.x for exactly this class of printer).
pub const EMULATOR_FIRMWARE: &str = "V60.14.0";

/// `~HQ<query-type>`: respond per the guide's pages 226-232, or `None` for
/// an unsupported/empty query-type (the guide: "must be an accepted value
/// or the command is ignored" - the session logs and answers nothing, and
/// the socket stays open).
pub fn respond_hq(query_type: &[u8], s: &PrinterState) -> Option<Vec<u8>> {
    let mut lines: Vec<String> = match query_type {
        b"OD" => vec![
            "PRINT METERS".into(),
            format!("TOTAL NONRESETTABLE: {} \"", s.meter_total),
            format!("USER RESETTABLE CNTR1: {} \"", s.meter_cntr1),
            format!("USER RESETTABLE CNTR2: {} \"", s.meter_cntr2),
        ],
        b"SN" => vec!["SERIAL NUMBER".into(), EMULATOR_SERIAL.into()],
        _ => return None,
    };
    lines.push(String::new()); // trailing CRLF before ETX (guide examples)
    let mut out = vec![0x02];
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(b'\r');
            out.push(b'\n');
        }
        out.extend_from_slice(l.as_bytes());
    }
    out.push(0x03);
    Some(out)
}

/// `~WD` (p.361, low fidelity per the plan): a minimal identity/config
/// dump. Fleet tools mostly check that the printer answers; the model line
/// and firmware let them identify the peer. Params (a directory request on
/// real printers) are ignored - the emulator's whole storage is the dump.
pub fn respond_wd(s: &PrinterState) -> Vec<u8> {
    let lines = [
        "ZPL II EMULATOR",
        EMULATOR_FIRMWARE,
        &format!("SERIAL NUMBER: {EMULATOR_SERIAL}"),
        "RESOLUTION: 203 DPI",
        &format!("LABEL LENGTH: {} DOTS", s.label_length_dots),
        &format!("STATIC RAM: {}", s.conditions.static_ram as u8),
    ];
    let mut out = vec![0x02];
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(b'\r');
            out.push(b'\n');
        }
        out.extend_from_slice(l.as_bytes());
    }
    out.push(0x03);
    out
}

/// `~HI` (p.224): `XXXXXX,V1.0.0,dpm,000KB,X` - model, firmware,
/// resolution, memory, options. Chosen constants: 203 dpi = 8 dots/mm;
/// 4096KB RAM; no detected options.
pub fn respond_hi() -> Vec<u8> {
    format!("ZPL20,{EMULATOR_FIRMWARE},8,4096KB,0\r\n").into_bytes()
}

/// `^HV#,n,h,t,a` (p.239): header + the numbered field's data (truncated
/// to `n` bytes) + termination, from the last completed label. A field
/// number the last label never defined returns just the header and
/// terminator (real printers send the empty field with its framing).
pub fn respond_hv(req: &HostValueRequest, numbered: &BTreeMap<u32, String>) -> Vec<u8> {
    let mut out = Vec::with_capacity(req.header.len() + req.max_bytes + req.terminator.len());
    out.extend_from_slice(&req.header);
    if let Some(data) = numbered.get(&req.field) {
        for ch in data.chars().take(req.max_bytes) {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(&req.terminator);
    out
}
