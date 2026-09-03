//! Command table and dispatch for ZPL II.

use crate::bc;
use crate::bc1d;
use crate::config;
use crate::diag::Diagnostic;
use crate::geom;
use crate::ir::Label;
use crate::memory;
use crate::scanner::{Cmd, Scanner, Token};
use crate::state::{FieldDraft, FormatState, ParseContext, QueryRequest};
use crate::textfmt;

/// Command prefix: `^` (format commands) or `~` (printer operations).
/// Dispatch matches these against the mode's *current* command/tilde
/// characters (task 17: ^CC/^CT can move them off the literal bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Caret,
    Tilde,
}

/// When a command is handled: format commands are meaningful inside
/// ^XA..^XZ; printer operations are handled any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    Format,
    PrinterOp,
}

/// One row of the static command table.
pub struct CommandSpec {
    /// Two-byte mnemonic as scanned, e.g. `[b'X', b'A']` for ^XA;
    /// single-letter mnemonics pad the second byte with a space.
    pub mnemonic: [u8; 2],
    pub prefix: Prefix,
    pub kind: CmdKind,
    pub handler: fn(&mut ParseContext, &[u8]),
}

fn start_format(ctx: &mut ParseContext, _params: &[u8]) {
    // A stored format being re-executed (^XF) must not open a nested label.
    if ctx.recall_depth > 0 {
        ctx.diags.push(Diagnostic::warning(
            "XA",
            "stored format contains ^XA; ignored (recall is inline)",
        ));
        return;
    }
    // Record where this ^XA sits in the global input stream (driver-set
    // token offsets + stream_base); the archive (Task 19) slices the raw
    // .zpl span [format_start, ^XZ end) for the completed label.
    ctx.format_start = ctx.token_start;
    ctx.state.start_format();
}

fn end_format(ctx: &mut ParseContext, _params: &[u8]) {
    if ctx.recall_depth > 0 {
        ctx.diags.push(Diagnostic::warning(
            "XZ",
            "stored format contains ^XZ; ignored (recall is inline)",
        ));
        return;
    }
    // An open barcode field closed by ^XZ without data can't be encoded.
    check_barcode_data(ctx);
    check_malformed_utf16(ctx);
    expand_serials(ctx);
    if let Some(label) = ctx.state.end_format(&ctx.mode) {
        ctx.pending_label = Some(label);
    }
}

/// Expand `^SN` serialized fields at `^XZ` (plan Task 17 ruling: expansion
/// at parse time; counters live on [`ParseContext`] so the server can
/// persist them, Task 21). Each serialized draft's data gets its counter's
/// current value substituted for its indexed portion (the rightmost <=12
/// integers), then the counter advances by step x `copies` - v1 renders
/// the first copy's value
/// on every copy (`Label.copies` carries multiplicity) while keeping the
/// counter trajectory of a real printer, which increments once per printed
/// label. Counters are keyed by the field's ordinal among serialized
/// fields in the format; the same ordinal in a later format continues the
/// counter (real printers reset transient formats to v on re-send - a
/// documented v1 simplification; stored/resident formats keep running).
fn expand_serials(ctx: &mut ParseContext) {
    let drafts: Vec<&mut FieldDraft> = match &mut ctx.state {
        FormatState::InFormat { fields } => fields.iter_mut().collect(),
        FormatState::Field { draft, fields } => {
            fields.iter_mut().chain(core::iter::once(draft)).collect()
        }
        _ => return,
    };
    let mut ordinal = 0u8;
    for draft in drafts {
        let (step, zeros) = match &draft.serial {
            Some(serial) => (serial.step, serial.zeros),
            None => continue,
        };
        // The guide caps indexing at the rightmost 12 integers; an
        // overlength run is not an error - its leading digits stay literal.
        let counter = ctx.serial_counters.entry(ordinal).or_insert_with(|| {
            indexed_run(&draft.data)
                .and_then(|(start, end)| parse_digits(&draft.data[start..end]))
                .unwrap_or(0)
        });
        draft.data = substitute_run(&draft.data, *counter, zeros);
        let advance = step.saturating_mul(ctx.mode.copies as i64);
        *counter = counter.saturating_add(advance);
        // Saturated, never wrapped: far beyond the guide's 100-150-field
        // limit, later serials would share a counter instead of panicking.
        ordinal = ordinal.saturating_add(1);
    }
}

/// The byte range of the rightmost contiguous ASCII-digit run in `data`
/// (the guide's x.14 rule: "the first integer found when scanning from the
/// end of the backing store towards the beginning").
fn rightmost_run(data: &[u8]) -> Option<(usize, usize)> {
    let mut end = data.len();
    while end > 0 && !data[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && data[start - 1].is_ascii_digit() {
        start -= 1;
    }
    Some((start, end))
}

/// The byte range of the ^SN *indexed* portion: the rightmost digit run,
/// capped to the guide's 12 rightmost integers ("A maximum of 12 of the
/// right-most integers are subject to indexing"). The leading digits of an
/// overlength run stay literal - they are never replaced or dropped.
fn indexed_run(data: &[u8]) -> Option<(usize, usize)> {
    let (start, end) = rightmost_run(data)?;
    let index_start = end.saturating_sub(12).max(start);
    Some((index_start, end))
}

/// Parse an ASCII-digit slice as i64 with checked accumulation (runs up to
/// 12 digits saturate safely; oversized input clamps rather than panics).
fn parse_digits(p: &[u8]) -> Option<i64> {
    if p.is_empty() || !p.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for &b in p {
        v = v.saturating_mul(10).saturating_add((b - b'0') as i64);
    }
    Some(v)
}

/// Substitute `value` for the indexed portion of `data` (the rightmost 12
/// digits of its rightmost integer run - see [`indexed_run`]), printing the
/// value's sign when a decrement carries it below zero. `zeros` pads with
/// leading zeros to the indexed portion's width; the width grows if the
/// value outgrows it (real printers do not truncate the counter). Leading
/// digits of an overlength run are preserved verbatim, so the first copy of
/// a >12-digit serial prints its run unchanged (the review finding: the
/// saturated-counter substitution had silently corrupted it).
fn substitute_run(data: &[u8], value: i64, zeros: bool) -> Vec<u8> {
    let Some((start, end)) = indexed_run(data) else {
        return data.to_vec();
    };
    let width = end - start;
    let mag = value.unsigned_abs().to_string();
    let mut digits = if zeros && mag.len() < width {
        let mut padded = vec![b'0'; width - mag.len()];
        padded.extend_from_slice(mag.as_bytes());
        padded
    } else {
        mag.into_bytes()
    };
    let mut out = Vec::with_capacity(data.len() + 1);
    out.extend_from_slice(&data[..start]);
    if value < 0 {
        out.push(b'-');
    }
    out.append(&mut digits);
    out.extend_from_slice(&data[end..]);
    out
}

fn end_field(ctx: &mut ParseContext, _params: &[u8]) {
    // An open field closes into its format; a ^DF download hands its
    // captured bytes over to Memory.formats.
    check_barcode_data(ctx);
    check_malformed_utf16(ctx);
    if let Some((name, bytes)) = ctx.state.end_field() {
        ctx.memory.formats.insert(memory::format_name(name.as_bytes()), bytes);
    }
}

/// A `^CI29`/`^CI30` field whose bytes are malformed UTF-16 (odd length,
/// unpaired surrogate) decodes lossily; warn so the parser's "malformed ->
/// warning + lossy, never panic" contract is observable. Mirrors
/// `check_barcode_data`'s inspect-the-open-draft pattern.
fn check_malformed_utf16(ctx: &mut ParseContext) {
    if let FormatState::Field { draft, .. } = &ctx.state {
        if matches!(draft.code_page, 29 | 30)
            && crate::codepage::is_malformed_utf16(&draft.data, draft.code_page == 30)
        {
            ctx.diags.push(Diagnostic::warning(
                "CI",
                "malformed UTF-16 data; decoding lossily",
            ));
        }
    }
}

/// Barcode data problems visible at field close: no ^FD data (prints
/// nothing on a real printer) and bytes >= 0x80 (Code 128 is 7-bit - the
/// encoder substitutes '?', and the guide's A-mode is "full ASCII").
/// Both warn, never panic.
fn check_barcode_data(ctx: &mut ParseContext) {
    if let FormatState::Field { draft, .. } = &ctx.state {
        let Some(bc) = &draft.barcode else {
            return;
        };
        if draft.data.is_empty() {
            ctx.diags.push(Diagnostic::warning(
                "BC",
                "barcode field has no data; nothing to encode",
            ));
            return;
        }
        if bc.symbology == b'C' {
            // Code 128 substitutes '?' for high bytes (7-bit symbology).
            if let Some(&hi) = draft.data.iter().find(|&&b| b >= 0x80) {
                ctx.diags.push(Diagnostic::warning(
                    "BC",
                    format!("barcode data contains non-ASCII byte 0x{hi:02X}; encoded as '?' (Code 128 is 7-bit)"),
                ));
            }
            return;
        }
        // 2D symbologies: flag data that can never fit at parse close.
        if matches!(bc.symbology, b'Q' | b'X' | b'7' | b'0') {
            if let Some(msg) = crate::bc2d::size_error(bc.symbology, bc.qr_ec, bc.qr_mask, bc.pdf_security, draft.data.len()) {
                ctx.diags.push(Diagnostic::warning("B*", msg));
            }
            return;
        }
        // Task 15 1D symbologies: report what the encoder would reject so
        // the skipped field never goes out without a diagnostic.
        if let Some(msg) = bc1d::data_error(bc.symbology, bc.check, &draft.data) {
            ctx.diags.push(Diagnostic::warning("B*", msg));
        }
    }
}

fn start_download(ctx: &mut ParseContext, params: &[u8]) {
    // A stored format executing inline must not start a nested ^DF capture:
    // it would swallow the rest of the outer stream.
    if ctx.recall_depth > 0 {
        ctx.diags.push(Diagnostic::warning(
            "DF",
            "stored format contains ^DF; ignored (recall is inline)",
        ));
        return;
    }
    ctx.state.start_download(params);
}

/// `^CCx`/`~CCx`: change the format-command prefix character (guide p.152).
/// The scanner consumed exactly one parameter byte (its "next character
/// received" rule); the parse driver applies the change to the scanner
/// (`Scanner::set_chars`) after this dispatch. An empty command has nothing
/// to record and warns - the guide requires a character.
fn change_command_char(ctx: &mut ParseContext, params: &[u8]) {
    match params.first() {
        Some(&c) => ctx.mode.cmd_char = c,
        None => ctx.diags.push(Diagnostic::warning(
            "CC",
            "no character given; command ignored",
        )),
    }
}

/// `^CTx`/`~CTx`: change the control-command (tilde) prefix character
/// (guide p.166). Same mechanics as [`change_command_char`].
fn change_tilde_char(ctx: &mut ParseContext, params: &[u8]) {
    match params.first() {
        Some(&c) => ctx.mode.tilde_char = c,
        None => ctx.diags.push(Diagnostic::warning(
            "CT",
            "no character given; command ignored",
        )),
    }
}

/// `^FX`: comment block (guide p.209). The body is the command's parameter
/// run, which the scanner already ended at the next command character -
/// nothing here is ever dispatched, so arbitrary bytes stay inert.
fn comment(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `^HU`/`~HU`: return the configured ZebraNet Alert table to the host.
/// The response is server-side (Task 20); parsing accepts and discards it.
fn host_get_alerts(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `^HYd:o.x`: upload a graphic object (as a ~DY reply). The response is
/// server-side (Task 20); parsing accepts and discards the request.
fn host_upload_graphic(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `^HZx,y,l`: printer description in XML. The response is server-side
/// (Task 20); parsing accepts and discards the request.
fn host_description(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `~WC`: print a configuration label (printer-state dump). The dump label
/// has no user-format rendering role here; accept and discard.
fn print_config_label(_ctx: &mut ParseContext, _params: &[u8]) {}

/// `~JR`: power-on reset. A protocol-level action with no parse state;
/// accept and discard.
fn power_on_reset(_ctx: &mut ParseContext, _params: &[u8]) {}

// Task 20: host queries. The responses are server-side - the parser
// records the request on `ParseContext::queries`, which the TCP session
// drains after each parse window and answers on the socket. Registration
// in the normal table is what makes detection scanner-driven: inside a
// ^FD field body the scanner treats `~` as literal data, so an embedded
// `~HS` never reaches these handlers (the pointer to the old byte-scan
// false-positive regression). ~HS suppression (paper out, ribbon out,
// head open, rewinder full, head over-temperature) is also server-side.
fn query_host_status(ctx: &mut ParseContext, _params: &[u8]) {
    ctx.queries.push(QueryRequest::HostStatus);
}

fn query_head_status(ctx: &mut ParseContext, params: &[u8]) {
    ctx.queries.push(QueryRequest::HeadQuery(params.to_vec()));
}

fn query_config_dump(ctx: &mut ParseContext, params: &[u8]) {
    ctx.queries.push(QueryRequest::ConfigDump(params.to_vec()));
}

fn query_model_id(ctx: &mut ParseContext, _params: &[u8]) {
    ctx.queries.push(QueryRequest::ModelId);
}

/// Static command table; rows are added by later tasks as handlers land.
const TABLE: &[CommandSpec] = &[
    CommandSpec { mnemonic: *b"XA", prefix: Prefix::Caret, kind: CmdKind::Format, handler: start_format },
    CommandSpec { mnemonic: *b"XZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: end_format },
    CommandSpec { mnemonic: *b"FS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: end_field },
    CommandSpec { mnemonic: *b"DF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: start_download },
    // Task 7: printer storage. ^DF is the download half of this family (its
    // ^FS ends the capture and stores the bytes); the tilde operations run
    // any time; ^ID/^XG/^XF/^IL/^IM/^IS/^GS act on or inside a format.
    CommandSpec { mnemonic: *b"XF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::recall_format },
    CommandSpec { mnemonic: *b"XG", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::recall_graphic },
    CommandSpec { mnemonic: *b"ID", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::delete_object },
    CommandSpec { mnemonic: *b"IL", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::image_load },
    CommandSpec { mnemonic: *b"IM", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::image_move },
    CommandSpec { mnemonic: *b"IS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::image_save },
    // Task 8: drawing-field command set. ^GS was registered by Task 7 (it
    // is the graphic-symbol *font style*, not a shape); ^GB ^GC ^GD ^GE
    // draw shapes, ^GF carries an inline graphic field.
    CommandSpec { mnemonic: *b"GB", prefix: Prefix::Caret, kind: CmdKind::Format, handler: geom::graphic_box },
    CommandSpec { mnemonic: *b"GC", prefix: Prefix::Caret, kind: CmdKind::Format, handler: geom::graphic_circle },
    CommandSpec { mnemonic: *b"GD", prefix: Prefix::Caret, kind: CmdKind::Format, handler: geom::graphic_diagonal },
    CommandSpec { mnemonic: *b"GE", prefix: Prefix::Caret, kind: CmdKind::Format, handler: geom::graphic_ellipse },
    CommandSpec { mnemonic: *b"GF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: geom::graphic_field },
    CommandSpec { mnemonic: *b"GS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::graphic_symbol },
    // Task 9: barcode field framework. ^BY sets the ^B* defaults (module
    // width, ratio, height); ^BC (Code 128) is the first symbology and the
    // pattern for Task 15's ^B1..^BE.
    CommandSpec { mnemonic: *b"BY", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::bar_code_defaults },
    CommandSpec { mnemonic: *b"BC", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::code_128 },
    // Task 15: the remaining 1D symbologies. ^B1..^B9 scan as the
    // one-letter mnemonic `B ` with the symbology digit as the first
    // parameter segment; the two-letter mnemonics dispatch directly.
    // (^B0 Aztec, ^B4 Code 49, ^B5 Planet, ^B7 PDF417 and ^BB CODABLOCK
    // are 2D/stacked - Task 16 territory; their commands warn and skip.)
    CommandSpec { mnemonic: *b"B ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::barcode_digit },
    CommandSpec { mnemonic: *b"BA", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::code93_field },
    // Task 16: 2D symbologies. ^B0 (Aztec) and ^B7 (PDF417) scan through
    // the digit route; ^BQ (QR) and ^BX (DataMatrix) are two-letter.
    CommandSpec { mnemonic: *b"BQ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::qr_field },
    CommandSpec { mnemonic: *b"BX", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::datamatrix_field },
    CommandSpec { mnemonic: *b"BK", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::codabar_field },
    CommandSpec { mnemonic: *b"BE", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::ean13_field },
    CommandSpec { mnemonic: *b"BU", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::upca_field },
    CommandSpec { mnemonic: *b"BM", prefix: Prefix::Caret, kind: CmdKind::Format, handler: bc::msi_field },
    CommandSpec { mnemonic: *b"DG", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::download_graphic },
    CommandSpec { mnemonic: *b"DY", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::download_object },
    CommandSpec { mnemonic: *b"DS", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::download_scalable_font },
    CommandSpec { mnemonic: *b"DT", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::download_bounded_font },
    CommandSpec { mnemonic: *b"DU", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::download_unbounded_font },
    CommandSpec { mnemonic: *b"EG", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::erase_graphics },
    CommandSpec { mnemonic: *b"DN", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: memory::abort_download },
    // Task 5: text-field command set. The ^A mnemonic is single-letter, so
    // the scanner pads it to [A, space].
    CommandSpec { mnemonic: *b"FO", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_origin },
    CommandSpec { mnemonic: *b"FT", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_typeset },
    CommandSpec { mnemonic: *b"FD", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_data },
    CommandSpec { mnemonic: *b"FV", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_variable },
    CommandSpec { mnemonic: *b"FH", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_hex },
    CommandSpec { mnemonic: *b"FR", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_reverse },
    CommandSpec { mnemonic: *b"FB", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_block },
    CommandSpec { mnemonic: *b"FW", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_orientation },
    CommandSpec { mnemonic: *b"A ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::select_font },
    CommandSpec { mnemonic: *b"CF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::change_default_font },
    CommandSpec { mnemonic: *b"CW", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::assign_font },
    CommandSpec { mnemonic: *b"CI", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::change_international },
    CommandSpec { mnemonic: *b"FC", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::set_clock_indicators },
    // Task 6: label configuration commands. Geometry config is stored in
    // PrinterMode; darkness/media/units/ZPL-mode in PrinterConfig; the rest
    // are parse-and-validate (see config.rs).
    CommandSpec { mnemonic: *b"LH", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::label_home },
    CommandSpec { mnemonic: *b"LL", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::label_length },
    CommandSpec { mnemonic: *b"PW", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::print_width },
    CommandSpec { mnemonic: *b"PO", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::print_orientation },
    CommandSpec { mnemonic: *b"PM", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::mirror_image },
    CommandSpec { mnemonic: *b"LS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::label_shift },
    CommandSpec { mnemonic: *b"LT", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::label_top },
    CommandSpec { mnemonic: *b"MD", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::media_darkness },
    CommandSpec { mnemonic: *b"PQ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::print_quantity },
    CommandSpec { mnemonic: *b"PR", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::print_rate },
    CommandSpec { mnemonic: *b"PH", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::slew_to_home },
    CommandSpec { mnemonic: *b"MN", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::media_tracking },
    CommandSpec { mnemonic: *b"MT", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::media_type },
    CommandSpec { mnemonic: *b"MM", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::print_mode },
    CommandSpec { mnemonic: *b"MU", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::units },
    CommandSpec { mnemonic: *b"MF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::media_feed },
    CommandSpec { mnemonic: *b"MI", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::maintenance_message },
    CommandSpec { mnemonic: *b"ML", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::max_label_length },
    CommandSpec { mnemonic: *b"MP", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::mode_protection },
    CommandSpec { mnemonic: *b"MW", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::head_cold_warning },
    CommandSpec { mnemonic: *b"PA", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::advanced_text },
    CommandSpec { mnemonic: *b"PF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::slew_rows },
    CommandSpec { mnemonic: *b"SC", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::serial_comms },
    CommandSpec { mnemonic: *b"SZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::zpl_mode },
    CommandSpec { mnemonic: *b"ZZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::printer_sleep },
    CommandSpec { mnemonic: *b"XB", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::suppress_backfeed },
    CommandSpec { mnemonic: *b"XS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::dynamic_calibration },
    CommandSpec { mnemonic: *b"PN", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::present_now },
    // Tilde printer operations from the same config family: darkness and
    // slew-to-home have ^- and ~-forms in the guide (^PH ~PH p.317; ~SD p.333
    // is the only darkness form).
    CommandSpec { mnemonic: *b"SD", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: config::set_darkness },
    CommandSpec { mnemonic: *b"PH", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: config::slew_to_home },
    // Task 17: command-character switches (^CC/^CT), serial numbers (^SN),
    // and the accept-only remainder (^SQ ^SR ^SS ^ST ^SX ^TB ^TO ^FX ^HY ^HZ
    // ^HU ^JZ, plus the tilde ops ~WC ~TA ~JR). ^SN/^TB modify the open
    // field; the rest are parsed and validated but have no label-image
    // effect (see config.rs / cmd.rs handler docs).
    CommandSpec { mnemonic: *b"CC", prefix: Prefix::Caret, kind: CmdKind::Format, handler: change_command_char },
    CommandSpec { mnemonic: *b"CC", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: change_command_char },
    CommandSpec { mnemonic: *b"CT", prefix: Prefix::Caret, kind: CmdKind::Format, handler: change_tilde_char },
    CommandSpec { mnemonic: *b"CT", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: change_tilde_char },
    CommandSpec { mnemonic: *b"SN", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::serial_number },
    CommandSpec { mnemonic: *b"TB", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::text_block },
    CommandSpec { mnemonic: *b"SQ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::halt_zebranet_alert },
    CommandSpec { mnemonic: *b"SR", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::set_head_resistance },
    CommandSpec { mnemonic: *b"SS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::set_media_sensors },
    CommandSpec { mnemonic: *b"ST", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::set_clock },
    CommandSpec { mnemonic: *b"SX", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::set_zebranet_alert },
    CommandSpec { mnemonic: *b"TO", prefix: Prefix::Caret, kind: CmdKind::Format, handler: memory::transfer_object },
    CommandSpec { mnemonic: *b"FX", prefix: Prefix::Caret, kind: CmdKind::Format, handler: comment },
    CommandSpec { mnemonic: *b"HY", prefix: Prefix::Caret, kind: CmdKind::Format, handler: host_upload_graphic },
    CommandSpec { mnemonic: *b"HZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: host_description },
    CommandSpec { mnemonic: *b"HU", prefix: Prefix::Caret, kind: CmdKind::Format, handler: host_get_alerts },
    CommandSpec { mnemonic: *b"HU", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: host_get_alerts },
    CommandSpec { mnemonic: *b"JZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: config::reprint_after_error },
    CommandSpec { mnemonic: *b"WC", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: print_config_label },
    CommandSpec { mnemonic: *b"TA", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: config::tear_off_adjust },
    CommandSpec { mnemonic: *b"JR", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: power_on_reset },
    // Task 20: host queries (~HS ~HQ ~WD ~HI) and numbered fields
    // (^FN / ^HV). ~HS is the byte-exact status centerpiece; the rest are
    // the protocol responders. ^FN/^HV work inside formats (^HV may be
    // answered any time - the guide pages 200/239).
    CommandSpec { mnemonic: *b"HS", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: query_host_status },
    CommandSpec { mnemonic: *b"HQ", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: query_head_status },
    CommandSpec { mnemonic: *b"WD", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: query_config_dump },
    CommandSpec { mnemonic: *b"HI", prefix: Prefix::Tilde, kind: CmdKind::PrinterOp, handler: query_model_id },
    CommandSpec { mnemonic: *b"FN", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::field_number },
    CommandSpec { mnemonic: *b"HV", prefix: Prefix::Caret, kind: CmdKind::Format, handler: textfmt::host_value },
];

/// The command table: prefix+mnemonic -> handler. Rows accumulate across tasks.
pub fn command_table() -> &'static [CommandSpec] {
    TABLE
}

/// Look up `cmd` in the table and run its handler; unknown commands become a
/// diagnostic. Handlers receive the raw parameter bytes.
///
/// Prefix matching is against the mode's *current* command/tilde characters,
/// not the literal caret/tilde bytes: after a `^CC/` the scan of `/FO...`
/// must still find the caret rows (the scanner records the raw prefix byte).
pub fn dispatch(ctx: &mut ParseContext, cmd: Cmd) {
    for spec in command_table() {
        let prefix_matches = match spec.prefix {
            Prefix::Caret => cmd.prefix == ctx.mode.cmd_char,
            Prefix::Tilde => cmd.prefix == ctx.mode.tilde_char,
        };
        if prefix_matches && spec.mnemonic == cmd.mnemonic {
            (spec.handler)(ctx, cmd.params);
            return;
        }
    }
    // The command token carries no input offset; diagnostics report 0 until
    // the driver threads scanner positions through.
    ctx.diags
        .push(Diagnostic::unknown_command(cmd.prefix, &cmd.mnemonic[..], 0));
}

/// Drive `scanner` through `dispatch` until the input is exhausted.
///
/// While in a ^DF download, subsequent tokens (commands and data) are
/// captured raw until the terminating ^FS. While a text field is open, `~`
/// is scanned as literal data - the ^FD body may contain it (Task 2's
/// field-data model; `^FD` params absorb tildes until the next `^`).
pub fn parse_stream(scanner: &mut Scanner, ctx: &mut ParseContext) {
    // A stream resumed on a fresh scanner (new format batch, ^XF recall)
    // scans with the prefix characters persisted in the mode.
    scanner.set_chars(ctx.mode.cmd_char, ctx.mode.tilde_char);
    while let Some(token) = scanner.next(matches!(ctx.state, FormatState::Field { .. })) {
        match token {
            Token::Cmd(cmd) => {
                let ends_download = cmd.prefix == ctx.mode.cmd_char && cmd.mnemonic == *b"FS";
                if matches!(ctx.state, FormatState::DefFormat { .. }) && !ends_download {
                    ctx.state.capture(&cmd_bytes(&cmd));
                } else {
                    dispatch(ctx, cmd);
                    // ^CC/^CT handlers may have written new prefix
                    // characters into the mode; re-sync the scanner so the
                    // next token scans with them.
                    scanner.set_chars(ctx.mode.cmd_char, ctx.mode.tilde_char);
                }
            }
            // Outside a download, capture is a no-op.
            Token::Data(data) => ctx.state.capture(data),
        }
    }
}

/// A completed label plus its raw input span: global byte offsets of the
/// `^XA` ... `^XZ` token range (`stream_base` + chunk-local scanner
/// positions), for the server archive's `.zpl` file.
#[derive(Debug)]
pub struct ParsedLabel {
    pub label: Label,
    pub start: usize,
    pub end: usize,
}

/// Like [`parse_stream`], but yields every label closed by a `^XZ` in this
/// input (in order) instead of leaving only the most recent one in
/// `ctx.pending_label`, and each entry carries the label's raw-input span.
/// The TCP session driver (Task 19) calls this per read chunk, so a single
/// chunk containing several formats archives each and stores exact raw
/// bytes; [`parse_stream`] keeps its last-label-wins postcondition for
/// whole-stream parsing.
pub fn parse_stream_collect(
    scanner: &mut Scanner,
    ctx: &mut ParseContext,
    labels: &mut Vec<ParsedLabel>,
) {
    scanner.set_chars(ctx.mode.cmd_char, ctx.mode.tilde_char);
    while let Some(token) = scanner.next(matches!(ctx.state, FormatState::Field { .. })) {
        // This token's global span: token_start carries over from the
        // previous token (or the session's chunk base); token_end is the
        // current scanner position plus the bytes consumed by earlier
        // chunks, so spans survive chunk boundaries.
        ctx.token_start = ctx.token_end;
        ctx.token_end = ctx.stream_base + scanner.pos();
        match token {
            Token::Cmd(cmd) => {
                let ends_download = cmd.prefix == ctx.mode.cmd_char && cmd.mnemonic == *b"FS";
                if matches!(ctx.state, FormatState::DefFormat { .. }) && !ends_download {
                    ctx.state.capture(&cmd_bytes(&cmd));
                } else {
                    dispatch(ctx, cmd);
                    // ^CC/^CT handlers may have written new prefix
                    // characters into the mode; re-sync the scanner so the
                    // next token scans with them.
                    scanner.set_chars(ctx.mode.cmd_char, ctx.mode.tilde_char);
                    if let Some(label) = ctx.pending_label.take() {
                        // token_end is just past this ^XZ token; format_start
                        // was set by the matching ^XA.
                        labels.push(ParsedLabel {
                            label,
                            start: ctx.format_start,
                            end: ctx.token_end,
                        });
                    }
                }
            }
            // Outside a download, capture is a no-op.
            Token::Data(data) => ctx.state.capture(data),
        }
    }
}

/// Reconstruct one command token as its raw input bytes (dropping the
/// scanner's mnemonic padding), for ^DF capture.
fn cmd_bytes(cmd: &Cmd) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + cmd.mnemonic.len() + cmd.params.len());
    bytes.push(cmd.prefix);
    for &b in &cmd.mnemonic {
        if b != b' ' {
            bytes.push(b);
        }
    }
    bytes.extend_from_slice(cmd.params);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Diagnostics, Severity};
    use crate::ir::{Field, OriginMode, Rotation};
    use crate::state::{FieldDraft, Memory, ParseContext};

    #[test]
    fn frames_and_unknowns() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // ^QZ is not a real command; ^ZZ is registered (Task 6: printer sleep).
        parse_stream(&mut Scanner::new(b"^XA^QZ^FS^XZ"), &mut ctx);
        assert_eq!(ctx.diags.items.len(), 1); // ^QZ unknown
        assert!(matches!(ctx.diags.items[0].severity, Severity::UnknownCommand));
        assert!(ctx.pending_label.is_some()); // ^XA..^XZ framed
    }

    #[test]
    fn partial_format_detected() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        parse_stream(&mut Scanner::new(b"^XA^FO10,10"), &mut ctx);
        assert!(ctx.state.is_partial()); // in format, no ^XZ yet
    }

    #[test]
    fn download_captures_until_fs() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        parse_stream(&mut Scanner::new(b"^DFR:F^FO10,10^FS^XZ"), &mut ctx);
        // ^DF entered DefFormat and captured the ^FO body; ^FS ended the
        // download, so the captured commands were never dispatched.
        assert!(matches!(&ctx.state, FormatState::Outside));
        assert_eq!(ctx.diags.items.len(), 0);
        assert!(ctx.pending_label.is_none()); // ^XZ outside a format is a no-op
    }

    #[test]
    fn field_draft_finalizes_into_label() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // An open field as Task 5's ^FO/^FD handlers will leave one.
        ctx.state = FormatState::Field {
            draft: FieldDraft {
                x: 10,
                y: 20,
                origin: OriginMode::Dot,
                rotation: Rotation::R90,
                reverse: true,
                font: *b"A ",
                font_height: 0,
                font_width: 0,
                data: b"hello".to_vec(),
                code_page: 0,
                block: None,
                barcode: None,
                graphic: None,
                inline_data: None,
                geometry: None,
                font_name: None,
                symbol: false,
                hex: None,
                re_eval: false,
                serial: None,
                raw: b"^FDhello".to_vec(),
                field_number: None,
            },
            fields: Vec::new(),
        };
        ctx.mode.pw_dots = 800;
        ctx.mode.ll_dots = 400;
        parse_stream(&mut Scanner::new(b"^FS^XZ"), &mut ctx);
        let label = ctx.pending_label.unwrap();
        assert_eq!(label.width_dots, 800); // dimensions come from mode
        assert_eq!(label.height_dots, 400);
        assert_eq!(label.copies, 1);
        assert_eq!(label.fields.len(), 1);
        match &label.fields[0] {
            Field::Text(t) => {
                assert_eq!(t.x, 10);
                assert_eq!(t.y, 20);
                assert_eq!(t.origin, OriginMode::Dot);
                assert_eq!(t.rotation, Rotation::R90);
                assert!(t.reverse);
                assert_eq!(t.font, *b"A ");
                assert_eq!((t.font_height, t.font_width), (0, 0));
                assert_eq!(t.data, "hello");
                assert!(!t.re_eval);
            }
            other => panic!("expected a text field, got {other:?}"),
        }
    }

    #[test]
    fn query_commands_record_requests_not_warnings() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // ~ queries between formats; ^HV inside a format (the guide's
        // pattern); a tilde embedded in ^FD data must NOT be a query.
        parse_stream(
            &mut Scanner::new(b"~HS~HQOD~WD~HI^XA^FN1^FD~HS^FS^HV1,8,H,T^XZ"),
            &mut ctx,
        );
        use crate::state::QueryRequest;
        let expected = vec![
            QueryRequest::HostStatus,
            QueryRequest::HeadQuery(b"OD".to_vec()),
            QueryRequest::ConfigDump(b"".to_vec()),
            QueryRequest::ModelId,
            QueryRequest::HostValue(crate::state::HostValueRequest {
                field: 1,
                max_bytes: 8,
                header: b"H".to_vec(),
                terminator: b"T".to_vec(),
            }),
        ];
        let queries = core::mem::take(&mut ctx.queries);
        assert_eq!(queries, expected);
        assert!(
            ctx.diags.items.is_empty(),
            "registered queries must not warn as unknown commands"
        );
        assert!(ctx.pending_label.is_some()); // the format still completed

        // ^HV expands h/t through the open field's ^FH indicator and drops
        // the trailing F/L transfer-mode segment (the guide p.239 pattern
        // ^FH_^HV...,L: _40 = '@', so the terminator is ">@").
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        parse_stream(&mut Scanner::new(b"^XA^FH_^HV1,3,<,>_40,L^FS^XZ"), &mut ctx);
        assert_eq!(
            ctx.queries,
            vec![QueryRequest::HostValue(crate::state::HostValueRequest {
                field: 1,
                max_bytes: 3,
                header: b"<".to_vec(),
                terminator: b">@".to_vec(),
            })]
        );
    }

    #[test]
    fn numbered_fields_land_on_the_label() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        parse_stream(
            &mut Scanner::new(b"^XA^FN1^FDHello^FS^FN7^FO10,10^FDWorld^FS^XZ"),
            &mut ctx,
        );
        let label = ctx.pending_label.expect("^XZ closes the format");
        assert_eq!(label.numbered.get(&1).map(String::as_str), Some("Hello"));
        assert_eq!(label.numbered.get(&7).map(String::as_str), Some("World"));
        assert_eq!(label.numbered.len(), 2);
    }

    #[test]
    fn collect_yields_every_label_with_exact_spans() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        let mut labels = Vec::new();
        let src = b"^XA^GB50,50,2,B^XZ^XA^GB20,20,2,B^XZ";
        parse_stream_collect(&mut Scanner::new(src), &mut ctx, &mut labels);
        assert_eq!(labels.len(), 2, "each ^XZ yields one label");
        assert_eq!(&src[labels[0].start..labels[0].end], b"^XA^GB50,50,2,B^XZ");
        assert_eq!(&src[labels[1].start..labels[1].end], b"^XA^GB20,20,2,B^XZ");
        assert!(labels[0].end <= labels[1].start);
        assert!(ctx.pending_label.is_none(), "collect drains pending_label");
    }

    #[test]
    fn collect_spans_cross_chunk_boundaries() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        let mut labels = Vec::new();
        ctx.stream_base = 0;
        parse_stream_collect(&mut Scanner::new(b"^XA^FO10,10"), &mut ctx, &mut labels);
        assert!(labels.is_empty());
        assert!(ctx.state.is_partial()); // 11 bytes in; no ^XZ yet
        // Second chunk: ^XZ completes the format opened in chunk 1. The
        // span must start at chunk 1's ^XA (offset 0), not the chunk base.
        ctx.stream_base = 11;
        parse_stream_collect(&mut Scanner::new(b"^XZ"), &mut ctx, &mut labels);
        assert_eq!(labels.len(), 1);
        // ^XZ is a 3-byte token (prefix + 2-letter mnemonic), so the span
        // ends at 11 + 3.
        assert_eq!((labels[0].start, labels[0].end), (0, 11 + 3));
        assert_eq!(
            &b"^XA^FO10,10^XZ"[labels[0].start..labels[0].end],
            b"^XA^FO10,10^XZ"
        );
    }
}
