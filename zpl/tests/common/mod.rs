//! Shared integration-test helpers (used by Tasks 5-9).

use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::ir::Label;
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

/// Parse a complete ZPL format and return the finished label plus any
/// diagnostics collected along the way. Panics if no `^XZ` closed a format.
pub fn parse_one(src: &[u8]) -> (Label, Diagnostics) {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    let label = ctx.pending_label.expect("format never closed by ^XZ");
    (label, diags)
}
