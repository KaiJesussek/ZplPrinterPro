//! Command table and dispatch for ZPL II.

use crate::diag::Diagnostic;
use crate::scanner::{Cmd, Scanner, Token};
use crate::state::{FormatState, ParseContext};

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
    ctx.state.start_format();
}

fn end_format(ctx: &mut ParseContext, _params: &[u8]) {
    if let Some(label) = ctx.state.end_format(&ctx.mode) {
        ctx.pending_label = Some(label);
    }
}

fn end_field(ctx: &mut ParseContext, _params: &[u8]) {
    ctx.state.end_field();
}

fn start_download(ctx: &mut ParseContext, params: &[u8]) {
    ctx.state.start_download(params);
}

/// Static command table; rows are added by later tasks as handlers land.
const TABLE: &[CommandSpec] = &[
    CommandSpec { mnemonic: *b"XA", prefix: Prefix::Caret, kind: CmdKind::Format, handler: start_format },
    CommandSpec { mnemonic: *b"XZ", prefix: Prefix::Caret, kind: CmdKind::Format, handler: end_format },
    CommandSpec { mnemonic: *b"FS", prefix: Prefix::Caret, kind: CmdKind::Format, handler: end_field },
    CommandSpec { mnemonic: *b"DF", prefix: Prefix::Caret, kind: CmdKind::Format, handler: start_download },
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
/// captured raw until the terminating ^FS.
// TODO(T5): pass in-field-data (^FD) awareness to Scanner::next.
pub fn parse_stream(scanner: &mut Scanner, ctx: &mut ParseContext) {
    while let Some(token) = scanner.next(false) {
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
        parse_stream(&mut Scanner::new(b"^XA^ZZ^FS^XZ"), &mut ctx);
        assert_eq!(ctx.diags.items.len(), 1); // ^ZZ unknown
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
                rotation: 90,
                reverse: true,
                font: *b"A ",
                data: b"hello".to_vec(),
                block: None,
                symbology: None,
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
                assert_eq!(t.data, "hello");
            }
            other => panic!("expected a text field, got {other:?}"),
        }
    }
}
