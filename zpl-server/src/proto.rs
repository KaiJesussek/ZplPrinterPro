//! Host-query responders: ~HQ, ~WD, ~HI, and ^HV (Task 20). The ~HS
//! three-string serializer lives in `hs.rs`; the parse-level recording of
//! these requests lives in the zpl crate (`ParseContext::queries`).
//!
//! Framing follows the guide: ~HQ responses start with STX, join lines
//! with CRLF, and end with ETX (p.227 "Comments"); ~HI is a bare line
//! (p.224 shows no framing); ^HV is raw data (p.239 example).

use std::collections::BTreeMap;

use zpl::memory::Memory;
use zpl::state::HostValueRequest;

use crate::state::PrinterState;

/// The emulated printer's factory serial number (~HQSN, ~WD).
pub const EMULATOR_SERIAL: &str = "ZPLDEMO0001";
/// Firmware version string (~WD, ~HI; the guide's ~HS print-mode footnote
/// references V60.14.x for exactly this class of printer).
pub const EMULATOR_FIRMWARE: &str = "V60.14.0";

/// `~HB` (p.220): battery status, STX/ETX-framed. The emulator has no
/// battery model; a nominal "good" reading is returned.
pub fn respond_hb(s: &PrinterState) -> Vec<u8> {
    let charge = 0; // line-powered emulator: no battery model
    let line = format!(
        "BATTERY {}; CHARGE {}%; TEMP {}",
        if charge > 0 { "GOOD" } else { "N/A" },
        charge,
        temp_name(s.conditions.head_temp)
    );
    frame_stx(&line)
}

/// `~HD` (p.221): head diagnostic - power supply voltage and head
/// temperature.
pub fn respond_hd(s: &PrinterState) -> Vec<u8> {
    let line = format!(
        "HEAD TEMP {}; SUPPLY 24V; DIAG OK",
        temp_name(s.conditions.head_temp)
    );
    frame_stx(&line)
}

/// `~HM` (p.223): host RAM status - total, maximum, available bytes
/// (per-object accounting over the stored images/fonts/formats).
pub fn respond_hm(mem: &Memory) -> Vec<u8> {
    let total = (mem.images.len() + mem.fonts.len() + mem.formats.len()) as u64 * 8192;
    let max: u64 = 16 * 1024 * 1024;
    let avail = max.saturating_sub(total);
    let line = format!("TOTAL: {total}; MAX: {max}; AVAILABLE: {avail}");
    frame_stx(&line)
}

/// `^HH` (p.222): one-line host status.
pub fn respond_hh(s: &PrinterState) -> Vec<u8> {
    let line = format!(
        "ZPL II EMULATOR {}; LABEL LENGTH {} DOTS; {}",
        EMULATOR_FIRMWARE,
        s.label_length_dots,
        if s.partial_format {
            "FORMAT OPEN"
        } else {
            "IDLE"
        }
    );
    frame_stx(&line)
}

/// Map the emulator's coarse head-temperature state to a readable reading.
fn temp_name(t: crate::state::Temp) -> &'static str {
    match t {
        crate::state::Temp::Normal => "NORMAL",
        crate::state::Temp::Warm => "WARM",
        crate::state::Temp::OverTemp => "OVERTEMP",
    }
}

fn frame_stx(line: &str) -> Vec<u8> {
    let mut out = vec![0x02];
    out.extend_from_slice(line.as_bytes());
    out.push(0x03);
    out
}

/// `^HGd:o.x` (p.221): upload a stored graphic as ~DG-style ASCII hex:
/// `"<name>,<bytes-per-row>,<byte count>,<hex>"`.
pub fn respond_hg(name: &str, mem: &Memory) -> Vec<u8> {
    let Some(img) = mem.images.get(name) else {
        return format!("*ERROR*: {name} NOT FOUND").into_bytes();
    };
    let mut out = format!("{name},{},{}", img.width.div_ceil(8), img.data.len()).into_bytes();
    for b in &img.data {
        out.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    out
}

/// `^HWd:o.x,f` (p.224): host directory listing. The pattern's name and
/// extension parts (default `*`) are matched against the stored images,
/// formats, and fonts, one `DIR *.GRF`-style line per object
/// ("<name> <size> <date>").
pub fn respond_hw(spec: &str, mem: &Memory) -> Vec<u8> {
    let (name_p, ext_p) = spec.rsplit_once('.').map_or((spec, "*"), |(n, e)| (n, e));
    let name_p = name_p.trim_end_matches(':');
    let mut lines: Vec<String> = Vec::new();
    let wc = |s: &str, p: &str| {
        p == "*"
            || s.eq_ignore_ascii_case(p)
            || (p.ends_with('*')
                && s.len() >= p.len() - 1
                && s[..p.len() - 1].eq_ignore_ascii_case(&p[..p.len() - 1]))
    };
    let push = |name: &str, size: usize, lines: &mut Vec<String>| {
        let (n, e) = name.rsplit_once('.').unwrap_or((name, ""));
        if wc(n, name_p) && (ext_p == "*" || wc(e, ext_p)) {
            lines.push(format!("{n}.{e} {size}"));
        }
    };
    for (k, v) in &mem.images {
        push(k, v.data.len(), &mut lines);
    }
    for (k, v) in &mem.formats {
        push(k, v.len(), &mut lines);
    }
    for (k, v) in &mem.fonts {
        push(k, v.len(), &mut lines);
    }
    if lines.is_empty() {
        return format!("NO FILES FOR {spec}").into_bytes();
    }
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(l);
    }
    out.into_bytes()
}

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

/// `^HV#,n,h,t,a` (p.239): header + the numbered field's data + termination,
/// from the last completed label. `n` is "the number of bytes to be
/// returned" (guide values 1..256): the field's UTF-8 bytes are truncated
/// to exactly `n` raw bytes - a cut that splits a multibyte code point is
/// fine, because the response is opaque transfer bytes and the host sizes
/// its buffer to n. A field number the last label never defined returns
/// just the header and terminator (real printers send the empty field with
/// its framing).
pub fn respond_hv(req: &HostValueRequest, numbered: &BTreeMap<u32, String>) -> Vec<u8> {
    let mut out = Vec::with_capacity(req.header.len() + req.max_bytes + req.terminator.len());
    out.extend_from_slice(&req.header);
    if let Some(data) = numbered.get(&req.field) {
        let n = req.max_bytes.min(data.len());
        out.extend_from_slice(&data.as_bytes()[..n]);
    }
    out.extend_from_slice(&req.terminator);
    out
}
