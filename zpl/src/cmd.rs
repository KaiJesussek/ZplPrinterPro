//! Command table and dispatch for ZPL II.

use crate::bc;
use crate::bc1d;
use crate::config;
use crate::diag::Diagnostic;
use crate::geom;
use crate::ir::Label;
use crate::memory;
use crate::scanner::{Cmd, Scanner, Token};
use crate::state::{FieldDraft, FormatState, ParseContext, PendingBinary, QueryRequest};
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

/// A binary command's declared payload framing: `(declared byte count,
/// bytes of the payload the token's params run already absorbed)` - see
/// [`CommandSpec::binary_len`].
pub type BinaryFraming = (usize, usize);
/// Binary payload framing decoder - see [`CommandSpec::binary_len`].
pub type BinaryLen = Option<fn(&[u8]) -> Option<BinaryFraming>>;
/// Raw-payload receiver - see [`CommandSpec::binary_handler`].
pub type BinaryHandler = Option<fn(&mut ParseContext, &[u8], &[u8])>;

/// One row of the static command table.
pub struct CommandSpec {
    /// Two-byte mnemonic as scanned, e.g. `[b'X', b'A']` for ^XA;
    /// single-letter mnemonics pad the second byte with a space.
    pub mnemonic: [u8; 2],
    pub prefix: Prefix,
    pub kind: CmdKind,
    pub handler: fn(&mut ParseContext, &[u8]),
    /// Raw-byte framing for binary payloads (`^GF` B/C, `~DY` B - guide
    /// p.215/p.181: binary data's "control prefixes are ignored until the
    /// total number of bytes needed ... is sent", so the payload may
    /// legally contain `^`/`~`/`^FS` bytes that the scanner must never
    /// tokenize). Given the command's scanned `params`, returns
    /// `(declared, already)`: the byte count the host declared and how
    /// much of the payload the params run already absorbed (the scanner
    /// stops it at the first command character, which may sit inside the
    /// payload). The parse driver raw-consumes the remaining
    /// `declared - already` bytes from the stream before the next token
    /// and hands them to `binary_handler`. `None` on every other command.
    pub binary_len: BinaryLen,
    /// Receiver for a framed binary payload. Runs *instead of* `handler`
    /// with `(ctx, params, tail)`: `tail` is the raw bytes taken from the
    /// stream (the `declared - already` remainder); the payload itself is
    /// the data segments of `params` plus `tail`. Used with `binary_len`
    /// by exactly the rows above.
    pub binary_handler: BinaryHandler,
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
    // A ^DF download still open when ^XA arrives is aborted here:
    // start_format() replaces the capture state, dropping the stored
    // bytes. Warn so the operator sees the crossover.
    if matches!(ctx.state, FormatState::DefFormat { .. }) {
        ctx.diags.push(Diagnostic::warning(
            "XA",
            "^XA while a format download is open; download aborted",
        ));
    }
    // A previous format's closed label must not leak into this one:
    // pending_label is only Some between a ^XZ and the next ^XA/drain.
    ctx.pending_label = None;
    // Record where this ^XA sits in the global input stream (driver-set
    // token offsets + stream_base); the archive (Task 19) slices the raw
    // .zpl span [format_start, ^XZ end) for the completed label. The
    // diag-index window opens here too: a chunk holding several formats
    // must not charge every diagnostic to the first label.
    ctx.format_start = ctx.token_start;
    ctx.diag_start = ctx.diags.items.len();
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
    check_malformed_utf8(ctx);
    expand_serials(ctx);
    if let Some(label) = ctx.state.end_format(&ctx.mode) {
        // A ^IS pending from this format (guide p.249: save the format as
        // a graphic image): render exactly the fields that were closed at
        // the ^IS (serial expansion already applied) and store the bitmap.
        if let Some(save) = ctx.image_save.take() {
            memory::fulfill_image_save(ctx, &label, save);
        }
        ctx.pending_label = Some(label);
    }
}

/// Expand `^SN` serialized fields at `^XZ` (plan Task 17 ruling: expansion
/// at parse time; counters live on [`ParseContext`] so the server can
/// persist them, Task 21). Each serialized draft's data gets its counter's
/// current value substituted for its indexed portion (the rightmost <=12
/// integers), then the counter advances by step x the serials this batch
/// prints - v1 renders the first copy's value
/// on every copy (`Label.copies` carries multiplicity) while keeping the
/// counter trajectory of a real printer: one advance per printed label,
/// or per r+1 labels under ^PQ r replicates (guide p.341: incrementing
/// takes place "when all replicates for each serial number have been
/// printed"). Counters are keyed by the field's ordinal among serialized
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
        // One counter advance per serial printed: with ^PQ r replicates,
        // each serial prints r+1 times (guide p.324/p.341), so the number
        // of serials over this batch is copies / (r+1). Integer division
        // leaves a batch ending mid-replicate unadvanced - the guide's
        // "when all replicates ... have been printed" has no rule for the
        // partial case (documented v1 choice).
        let serials = ctx
            .mode
            .copies
            .saturating_div(ctx.mode.replicates.saturating_add(1));
        let advance = step.saturating_mul(serials as i64);
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
    check_malformed_utf8(ctx);
    if let Some((name, bytes)) = ctx.state.end_field() {
        let key = memory::format_name(name.as_bytes());
        if !memory::fits_total(ctx.memory, &key, bytes.len()) {
            ctx.diags.push(Diagnostic::warning(
                "DF",
                format!("{key}: printer memory full; object dropped"),
            ));
            return;
        }
        ctx.memory.formats.insert(key, bytes);
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

/// A `^CI28` field whose bytes are malformed UTF-8 decodes lossily; warn
/// so the parser's "malformed -> warning + lossy, never panic" contract is
/// observable. Mirrors `check_malformed_utf16`.
fn check_malformed_utf8(ctx: &mut ParseContext) {
    if let FormatState::Field { draft, .. } = &ctx.state {
        if draft.code_page == 28 && crate::codepage::is_malformed_utf8(&draft.data) {
            ctx.diags.push(Diagnostic::warning(
                "CI",
                "malformed UTF-8 data; decoding lossily",
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
            if let Some(msg) = crate::bc2d::size_error(
                bc.symbology,
                bc.qr_ec,
                bc.qr_mask,
                bc.pdf_security,
                draft.data.len(),
            ) {
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
    // ^DF while a format is open destroys the open format (capture state
    // replaces it); warn, but still enter the download - the guide's own
    // pattern embeds ^DF right after ^XA.
    if ctx.state.is_partial() {
        ctx.diags.push(Diagnostic::warning(
            "DF",
            "format download while a format is open; download started",
        ));
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
    CommandSpec {
        mnemonic: *b"XA",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: start_format,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"XZ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: end_format,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: end_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: start_download,
        binary_len: None,
        binary_handler: None,
    },
    // Task 7: printer storage. ^DF is the download half of this family (its
    // ^FS ends the capture and stores the bytes); the tilde operations run
    // any time; ^ID/^XG/^XF/^IL/^IM/^IS/^GS act on or inside a format.
    CommandSpec {
        mnemonic: *b"XF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::recall_format,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"XG",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::recall_graphic,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"ID",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::delete_object,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"IL",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::image_load,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"IM",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::image_move,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"IS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::image_save,
        binary_len: None,
        binary_handler: None,
    },
    // Task 8: drawing-field command set. ^GS was registered by Task 7 (it
    // is the graphic-symbol *font style*, not a shape); ^GB ^GC ^GD ^GE
    // draw shapes, ^GF carries an inline graphic field.
    CommandSpec {
        mnemonic: *b"GB",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: geom::graphic_box,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"GC",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: geom::graphic_circle,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"GD",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: geom::graphic_diagonal,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"GE",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: geom::graphic_ellipse,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"GF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: geom::graphic_field,
        binary_len: Some(geom::gf_binary_len),
        binary_handler: Some(geom::gf_binary_handler),
    },
    CommandSpec {
        mnemonic: *b"GS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::graphic_symbol,
        binary_len: None,
        binary_handler: None,
    },
    // Task 9: barcode field framework. ^BY sets the ^B* defaults (module
    // width, ratio, height); ^BC (Code 128) is the first symbology and the
    // pattern for Task 15's ^B1..^BE.
    CommandSpec {
        mnemonic: *b"BY",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::bar_code_defaults,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BC",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::code_128,
        binary_len: None,
        binary_handler: None,
    },
    // Task 15: the remaining 1D symbologies. ^B1..^B9 scan as the
    // one-letter mnemonic `B ` with the symbology digit as the first
    // parameter segment; the two-letter mnemonics dispatch directly.
    // (^B0 Aztec, ^B4 Code 49, ^B5 Planet, ^B7 PDF417 and ^BB CODABLOCK
    // are 2D/stacked - Task 16 territory; their commands warn and skip.)
    CommandSpec {
        mnemonic: *b"B ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::barcode_digit,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BA",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::code93_field,
        binary_len: None,
        binary_handler: None,
    },
    // Task 16: 2D symbologies. ^B0 (Aztec) and ^B7 (PDF417) scan through
    // the digit route; ^BQ (QR) and ^BX (DataMatrix) are two-letter.
    CommandSpec {
        mnemonic: *b"BQ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::qr_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BX",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::datamatrix_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BK",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::codabar_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BE",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::ean13_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BU",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::upca_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"BM",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: bc::msi_field,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DG",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::download_graphic,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DY",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::download_object,
        binary_len: Some(memory::dy_binary_len),
        binary_handler: Some(memory::dy_binary_handler),
    },
    CommandSpec {
        mnemonic: *b"DS",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::download_scalable_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DT",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::download_bounded_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DU",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::download_unbounded_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"EG",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::erase_graphics,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"DN",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: memory::abort_download,
        binary_len: None,
        binary_handler: None,
    },
    // Task 5: text-field command set. The ^A mnemonic is single-letter, so
    // the scanner pads it to [A, space].
    CommandSpec {
        mnemonic: *b"FO",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_origin,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FT",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_typeset,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FD",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_data,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FV",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_variable,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FH",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_hex,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FR",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_reverse,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FB",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_block,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FW",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_orientation,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"A ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::select_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::change_default_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CW",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::assign_font,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CI",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::change_international,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FC",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::set_clock_indicators,
        binary_len: None,
        binary_handler: None,
    },
    // Task 6: label configuration commands. Geometry config is stored in
    // PrinterMode; darkness/media/units/ZPL-mode in PrinterConfig; the rest
    // are parse-and-validate (see config.rs).
    CommandSpec {
        mnemonic: *b"LH",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::label_home,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"LL",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::label_length,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PW",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::print_width,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PO",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::print_orientation,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PM",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::mirror_image,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"LS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::label_shift,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"LT",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::label_top,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MD",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::media_darkness,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PQ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::print_quantity,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PR",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::print_rate,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PH",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::slew_to_home,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MN",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::media_tracking,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MT",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::media_type,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MM",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::print_mode,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MU",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::units,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::media_feed,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MI",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::maintenance_message,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"ML",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::max_label_length,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MP",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::mode_protection,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"MW",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::head_cold_warning,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PA",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::advanced_text,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PF",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::slew_rows,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SC",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::serial_comms,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SZ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::zpl_mode,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"ZZ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::printer_sleep,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"XB",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::suppress_backfeed,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"XS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::dynamic_calibration,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PN",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::present_now,
        binary_len: None,
        binary_handler: None,
    },
    // Tilde printer operations from the same config family: darkness and
    // slew-to-home have ^- and ~-forms in the guide (^PH ~PH p.317; ~SD p.333
    // is the only darkness form).
    CommandSpec {
        mnemonic: *b"SD",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: config::set_darkness,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"PH",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: config::slew_to_home,
        binary_len: None,
        binary_handler: None,
    },
    // Task 17: command-character switches (^CC/^CT), serial numbers (^SN),
    // and the accept-only remainder (^SQ ^SR ^SS ^ST ^SX ^TB ^TO ^FX ^HY ^HZ
    // ^HU ^JZ, plus the tilde ops ~WC ~TA ~JR). ^SN/^TB modify the open
    // field; the rest are parsed and validated but have no label-image
    // effect (see config.rs / cmd.rs handler docs).
    CommandSpec {
        mnemonic: *b"CC",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: change_command_char,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CC",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: change_command_char,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CT",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: change_tilde_char,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"CT",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: change_tilde_char,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SN",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::serial_number,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"TB",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::text_block,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SQ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::halt_zebranet_alert,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SR",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::set_head_resistance,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SS",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::set_media_sensors,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"ST",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::set_clock,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"SX",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::set_zebranet_alert,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"TO",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: memory::transfer_object,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FX",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: comment,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HY",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: host_upload_graphic,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HZ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: host_description,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HU",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: host_get_alerts,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HU",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: host_get_alerts,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"JZ",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: config::reprint_after_error,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"WC",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: print_config_label,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"TA",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: config::tear_off_adjust,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"JR",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: power_on_reset,
        binary_len: None,
        binary_handler: None,
    },
    // Task 20: host queries (~HS ~HQ ~WD ~HI) and numbered fields
    // (^FN / ^HV). ~HS is the byte-exact status centerpiece; the rest are
    // the protocol responders. ^FN/^HV work inside formats (^HV may be
    // answered any time - the guide pages 200/239).
    CommandSpec {
        mnemonic: *b"HS",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: query_host_status,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HQ",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: query_head_status,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"WD",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: query_config_dump,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HI",
        prefix: Prefix::Tilde,
        kind: CmdKind::PrinterOp,
        handler: query_model_id,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"FN",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::field_number,
        binary_len: None,
        binary_handler: None,
    },
    CommandSpec {
        mnemonic: *b"HV",
        prefix: Prefix::Caret,
        kind: CmdKind::Format,
        handler: textfmt::host_value,
        binary_len: None,
        binary_handler: None,
    },
];

/// The command table: prefix+mnemonic -> handler. Rows accumulate across tasks.
pub fn command_table() -> &'static [CommandSpec] {
    TABLE
}

/// The table row for a scanned command token (prefix matched against the
/// mode's *current* command/tilde characters under `^CC`/`^CT`, not the
/// literal caret/tilde bytes), shared by [`dispatch`] and the parse
/// driver's binary framing so the two cannot pick different rows.
fn spec_for(ctx: &ParseContext, cmd: &Cmd) -> Option<&'static CommandSpec> {
    command_table().iter().find(|spec| {
        let prefix_matches = match spec.prefix {
            Prefix::Caret => cmd.prefix == ctx.mode.cmd_char,
            Prefix::Tilde => cmd.prefix == ctx.mode.tilde_char,
        };
        prefix_matches && spec.mnemonic == cmd.mnemonic
    })
}

/// Look up `cmd` in the table and run its handler; unknown commands become a
/// diagnostic. Handlers receive the raw parameter bytes.
pub fn dispatch(ctx: &mut ParseContext, cmd: Cmd) {
    if let Some(spec) = spec_for(ctx, &cmd) {
        (spec.handler)(ctx, cmd.params);
        return;
    }
    // The command token carries no input offset; diagnostics report 0 until
    // the driver threads scanner positions through.
    ctx.diags.push(Diagnostic::unknown_command(
        cmd.prefix,
        &cmd.mnemonic[..],
        0,
    ));
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
                // Not collecting: the label closed by ^XZ stays in
                // ctx.pending_label (last-label-wins postcondition).
                run_cmd_token(scanner, ctx, cmd, false);
            }
            // Outside a download, capture is a no-op.
            Token::Data(data) => ctx.state.capture(data),
        }
    }
    // Whole-stream parse has no later chunk to supply the payload a
    // stashed download still owes, so a pending binary here is genuine
    // truncation: warn and drop. The input after the truncation point was
    // never consumed into the payload, so it parsed normally above (the
    // trailing commands stay parseable). The chunked driver
    // ([`parse_stream_collect`]) deliberately has NO such check - a
    // payload merely spanning chunks is valid - the TCP session's EOF
    // branch warns instead.
    if let Some(pending) = ctx.pending_binary.take() {
        ctx.diags.push(Diagnostic::warning(
            core::str::from_utf8(&pending.mnemonic[..]).unwrap_or("??"),
            format!(
                "truncated binary data: expected {} bytes, got {}",
                pending.declared,
                pending.already + pending.buf.len()
            ),
        ));
    }
}

/// Continue an interrupted binary download ([`ParseContext::pending_binary`])
/// against the scanner's remaining bytes: consume the raw payload the
/// declared count still owes from the stream (payload bytes may legally be
/// `^`/`~`/`^FS` - "all control prefixes are ignored until the total number
/// of bytes needed ... is sent", guide p.215/p.181 - so they must never
/// tokenize), then run the stored handler with the header's params plus the
/// accumulated tail once the payload completes. Runs at the top of the
/// chunked driver's loop, before `scanner.next()` can see the payload. A
/// chunk that ends mid-payload leaves the stash in place for the next parse
/// call; only a stream that truly ends there warns (the session's EOF
/// branch). Valid input that merely spans chunks stays silent.
fn continue_pending_binary(scanner: &mut Scanner, ctx: &mut ParseContext) {
    if scanner.remaining() == 0 {
        return;
    }
    let Some(mut pending) = ctx.pending_binary.take() else {
        return;
    };
    let need = pending
        .declared
        .saturating_sub(pending.already)
        .saturating_sub(pending.buf.len());
    pending
        .buf
        .extend_from_slice(scanner.raw_take(need.min(scanner.remaining())));
    if pending.buf.len() < need {
        ctx.pending_binary = Some(pending); // chunk ended mid-payload
        return;
    }
    let handler = pending.handler;
    handler(ctx, &pending.params, &pending.buf);
}

/// Drive one scanned command token - shared by [`parse_stream`] and
/// [`parse_stream_collect`] so the two drivers cannot drift. While a ^DF
/// download is open the token is captured raw (a `^XA` aborts the download
/// and dispatches instead, so the ^XA handler can warn); otherwise it
/// dispatches - after any raw binary payload (`^GF` B/C, `~DY` B) is
/// consumed from the scanner. Returns the label closed by this token for
/// the collect driver's archive.
fn run_cmd_token(
    scanner: &mut Scanner,
    ctx: &mut ParseContext,
    cmd: Cmd,
    collect: bool,
) -> Option<ParsedLabel> {
    let ends_download = cmd.prefix == ctx.mode.cmd_char && cmd.mnemonic == *b"FS";
    let aborts_download = cmd.prefix == ctx.mode.cmd_char && cmd.mnemonic == *b"XA";
    // `(declared byte count, payload bytes the token's params run already
    // absorbed)` - None for non-binary commands (see CommandSpec).
    let framed = spec_for(ctx, &cmd)
        .and_then(|s| s.binary_len)
        .and_then(|f| f(cmd.params));
    if matches!(ctx.state, FormatState::DefFormat { .. }) && !ends_download && !aborts_download {
        // Captured verbatim - cmd_bytes already covers the payload bytes
        // inside the token; the tail after it rides along so the stored
        // format replays the same binary framing when recalled.
        let mut bytes = cmd_bytes(&cmd);
        if let Some((declared, already)) = framed {
            bytes.extend_from_slice(scanner.raw_take(declared.saturating_sub(already)));
        }
        ctx.state.capture(&bytes);
        return None;
    }
    if let (Some((declared, already)), Some(handler)) =
        (framed, spec_for(ctx, &cmd).and_then(|s| s.binary_handler))
    {
        let want = declared.saturating_sub(already);
        if want > scanner.remaining() {
            // The payload straddles a parse-chunk boundary (the TCP
            // session reads ~8 KiB at a time; large graphics are the
            // common case, not an edge). The header's params borrow this
            // chunk's buffer, which the caller drains, so stash a copy;
            // the next parse call's loop keeps raw-consuming the payload
            // (see [`continue_pending_binary`]). This is NOT an error: a
            // stream that truly ends mid-payload warns at EOF
            // (`parse_stream`'s tail check; the session's EOF branch).
            ctx.pending_binary = Some(PendingBinary {
                mnemonic: cmd.mnemonic,
                params: cmd.params.to_vec(),
                already,
                declared,
                buf: Vec::new(),
                handler,
            });
            return None;
        }
        // The payload's tail: the `^`/`~`/`^FS` bytes the declared count
        // still owes, taken raw so they never tokenize (guide p.215/p.181:
        // "all control prefixes are ignored until the total number of bytes
        // needed ... is sent"). Span accounting must cover them, or the
        // archive's raw .zpl slice would not re-parse to the same payload.
        let tail = scanner.raw_take(want);
        ctx.token_end = ctx.stream_base + scanner.pos();
        handler(ctx, cmd.params, tail);
        return None;
    }
    let closes_format = cmd.prefix == ctx.mode.cmd_char && cmd.mnemonic == *b"XZ";
    dispatch(ctx, cmd);
    // ^CC/^CT handlers may have written new prefix characters into the
    // mode; re-sync the scanner so the next token scans with them.
    scanner.set_chars(ctx.mode.cmd_char, ctx.mode.tilde_char);
    if collect && closes_format {
        if let Some(label) = ctx.pending_label.take() {
            // token_end is just past this ^XZ token; format_start and
            // diag_start were set by the matching ^XA. The window closes
            // after the ^XZ dispatch so anything raised at ^XZ
            // (barcode/UTF-16/serial checks) stays attributed to this
            // label.
            let diag_end = ctx.diags.items.len();
            return Some(ParsedLabel {
                label,
                start: ctx.format_start,
                end: ctx.token_end,
                diag_start: ctx.diag_start,
                diag_end,
            });
        }
    }
    None
}

/// A completed label plus its raw input span: global byte offsets of the
/// `^XA` ... `^XZ` token range (`stream_base` + chunk-local scanner
/// positions), for the server archive's `.zpl` file - and its diagnostic
/// window `[diag_start, diag_end)` into the session's `Diagnostics`
/// buffer, so a chunk containing several labels can attribute each
/// diagnostic to the format that raised it.
#[derive(Debug)]
pub struct ParsedLabel {
    pub label: Label,
    pub start: usize,
    pub end: usize,
    /// Index of the first diagnostic raised after this label's `^XA`.
    pub diag_start: usize,
    /// Index just past the last diagnostic raised before this label's `^XZ`.
    pub diag_end: usize,
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
    loop {
        // Finish any binary download the previous chunk started BEFORE
        // the scanner can tokenize its payload: the bytes are raw data,
        // and the span accounting picks them up through scanner.pos().
        continue_pending_binary(scanner, ctx);
        let Some(token) = scanner.next(matches!(ctx.state, FormatState::Field { .. })) else {
            break;
        };
        // This token's global span: token_start carries over from the
        // previous token (or the session's chunk base); token_end is the
        // current scanner position plus the bytes consumed by earlier
        // chunks, so spans survive chunk boundaries. A binary payload
        // consumed inside the token - or by the continuation above -
        // moves scanner.pos() past the raw bytes, so the next token's
        // start lands after them and the archived raw slice re-parses to
        // the same decode.
        ctx.token_start = ctx.token_end;
        ctx.token_end = ctx.stream_base + scanner.pos();
        match token {
            Token::Cmd(cmd) => {
                if let Some(parsed) = run_cmd_token(scanner, ctx, cmd, true) {
                    labels.push(parsed);
                }
            }
            // Outside a download, capture is a no-op.
            Token::Data(data) => ctx.state.capture(data),
        }
    }
    // NO truncation check here: chunk-end != EOF-on-purpose. A payload
    // that merely spans chunks stays in ctx.pending_binary for the next
    // parse call; the TCP session's EOF branch warns when the connection
    // truly ends mid-payload.
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
        assert!(matches!(
            ctx.diags.items[0].severity,
            Severity::UnknownCommand
        ));
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

    #[test]
    fn df_during_format_warns_and_download_starts() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // ^DF while a format is open: exactly one crossover warning, and
        // the download still enters capture (body stored on ^FS; ^XZ then
        // finds no open format).
        parse_stream(&mut Scanner::new(b"^XA^DFR:F^FO10,10^FS^XZ"), &mut ctx);
        assert_eq!(
            ctx.diags.items.len(),
            1,
            "only the crossover warning: {diags:?}"
        );
        assert_eq!(ctx.diags.items[0].command, "DF");
        assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
        assert!(ctx.memory.formats.contains_key("F.ZPL"), "download stored");
        assert!(ctx.pending_label.is_none());
    }

    #[test]
    fn xa_aborts_open_download() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // ^XA while a ^DF download is open: exactly one crossover
        // warning, the captured bytes are dropped, and the new format
        // parses normally.
        parse_stream(&mut Scanner::new(b"^DFR:F^FDx^XA^FO1^FDy^FS^XZ"), &mut ctx);
        assert_eq!(
            ctx.diags.items.len(),
            1,
            "only the crossover warning: {diags:?}"
        );
        assert_eq!(ctx.diags.items[0].command, "XA");
        assert!(matches!(ctx.diags.items[0].severity, Severity::Warning));
        assert!(
            !ctx.memory.formats.contains_key("F.ZPL"),
            "download aborted: captured bytes dropped"
        );
        let label = ctx.pending_label.as_ref().expect("the new format completed");
        assert_eq!(label.fields.len(), 1);
    }

    #[test]
    fn plain_flows_stay_silent() {
        // "No new warnings on valid input": a plain format, a plain
        // download, and ^XA outside any download must not warn.
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        parse_stream(
            &mut Scanner::new(b"^XA^FDx^FS^XZ^DFR:F^FO10,10^FS^XA^GB20,20,2,B^XZ"),
            &mut ctx,
        );
        assert!(ctx.diags.items.is_empty(), "unexpected: {diags:?}");
        assert!(ctx.pending_label.is_some());
    }

    #[test]
    fn stale_pending_label_dropped_at_xa() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        // A closed format leaves its label pending; a new ^XA must drop
        // the stale one even when the new format never closes (EOF).
        // Before the fix, label 1 leaked into the partial format 2.
        parse_stream(&mut Scanner::new(b"^XA^GB10,10,2,B^XZ^XA^FO10,10"), &mut ctx);
        assert!(ctx.pending_label.is_none(), "stale label dropped at ^XA");
        assert!(ctx.state.is_partial(), "the second format is open");
    }

    #[test]
    fn binary_payload_survives_archive_round_trip() {
        let mut diags = Diagnostics::default();
        let mut mem = Memory::default();
        let mut ctx = ParseContext::new(&mut diags, &mut mem);
        let mut labels = Vec::new();
        let payload = [0x5Eu8, 0x58, 0x5A, 0x7E, 0x41, 0x42]; // "^XZ~AB"
        let mut src = b"^XA^GFB,6,6,2,".to_vec();
        src.extend_from_slice(&payload);
        src.extend_from_slice(b"^FS^XZ");
        parse_stream_collect(&mut Scanner::new(&src), &mut ctx, &mut labels);
        assert_eq!(labels.len(), 1);
        assert!(
            src[labels[0].start..labels[0].end] == src[..],
            "the raw span covers the format incl. the binary payload"
        );
        match &labels[0].label.fields[0] {
            Field::Graphic(g) => {
                assert_eq!(g.data, payload.to_vec());
                assert_eq!(g.bytes_per_row, 2);
            }
            other => panic!("expected graphic field, got {other:?}"),
        }
        // Re-parse the archived raw span with a fresh context: the binary
        // framing and decode must be identical.
        let mut diags2 = Diagnostics::default();
        let mut mem2 = Memory::default();
        let mut ctx2 = ParseContext::new(&mut diags2, &mut mem2);
        parse_stream(&mut Scanner::new(&src[labels[0].start..labels[0].end]), &mut ctx2);
        assert!(ctx2.diags.items.is_empty(), "no diagnostics on re-parse");
        let label2 = ctx2.pending_label.expect("re-parsed format closes");
        match &label2.fields[0] {
            Field::Graphic(g) => {
                assert_eq!(g.data, payload.to_vec());
                assert_eq!(g.bytes_per_row, 2);
            }
            other => panic!("expected graphic field, got {other:?}"),
        }
    }
}
