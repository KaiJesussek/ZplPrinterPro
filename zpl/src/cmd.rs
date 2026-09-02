//! Command table and dispatch for ZPL II.

use crate::bc;
use crate::config;
use crate::diag::Diagnostic;
use crate::geom;
use crate::memory;
use crate::scanner::{Cmd, Scanner, Token};
use crate::state::{FormatState, ParseContext};
use crate::textfmt;

/// Command prefix: `^` (format commands) or `~` (printer operations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Caret,
    Tilde,
}

impl Prefix {
    fn matches(self, prefix: u8) -> bool {
        matches!((self, prefix), (Self::Caret, b'^') | (Self::Tilde, b'~'))
    }
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
    warn_empty_barcode(ctx);
    if let Some(label) = ctx.state.end_format(&ctx.mode) {
        ctx.pending_label = Some(label);
    }
}

fn end_field(ctx: &mut ParseContext, _params: &[u8]) {
    // An open field closes into its format; a ^DF download hands its
    // captured bytes over to Memory.formats.
    warn_empty_barcode(ctx);
    if let Some((name, bytes)) = ctx.state.end_field() {
        ctx.memory.formats.insert(memory::format_name(name.as_bytes()), bytes);
    }
}

/// A barcode field closed with no ^FD data prints nothing on a real
/// printer; warn so callers see the format is incomplete (never panic).
fn warn_empty_barcode(ctx: &mut ParseContext) {
    if let FormatState::Field { draft, .. } = &ctx.state {
        if draft.barcode.is_some() && draft.data.is_empty() {
            ctx.diags.push(Diagnostic::warning(
                "BC",
                "barcode field has no data; nothing to encode",
            ));
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
];

/// The command table: prefix+mnemonic -> handler. Rows accumulate across tasks.
pub fn command_table() -> &'static [CommandSpec] {
    TABLE
}

/// Look up `cmd` in the table and run its handler; unknown commands become a
/// diagnostic. Handlers receive the raw parameter bytes.
pub fn dispatch(ctx: &mut ParseContext, cmd: Cmd) {
    for spec in command_table() {
        if spec.prefix.matches(cmd.prefix) && spec.mnemonic == cmd.mnemonic {
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
    while let Some(token) = scanner.next(matches!(ctx.state, FormatState::Field { .. })) {
        match token {
            Token::Cmd(cmd) => {
                let ends_download = cmd.prefix == b'^' && cmd.mnemonic == *b"FS";
                if matches!(ctx.state, FormatState::DefFormat { .. }) && !ends_download {
                    ctx.state.capture(&cmd_bytes(&cmd));
                } else {
                    dispatch(ctx, cmd);
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
                block: None,
                barcode: None,
                graphic: None,
                inline_data: None,
                geometry: None,
                font_name: None,
                symbol: false,
                hex: None,
                re_eval: false,
                raw: b"^FDhello".to_vec(),
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
}
