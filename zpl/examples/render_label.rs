//! Parse a ZPL II format and rasterize it to a PNG.
//!
//! This file mirrors the README's library-usage example exactly; it is
//! compiled by `cargo test --workspace`, so the README snippet is
//! guaranteed to match the real public API.

use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::fonts::DefaultFontProvider;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let zpl = if args.len() > 1 {
        args[1].clone()
    } else {
        "^XA^FO50,50^A0N,30,30^FDHello, ZPL!^FS^XZ".to_string()
    };
    let format = zpl.as_bytes();
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(format), &mut ctx);
    let label = ctx.pending_label.take().expect("^XZ must close a label");
    let fonts = DefaultFontProvider::new();
    let opts = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let png = render(&label, &opts)
        .expect("render is structural")
        .bitmap
        .to_png();
    std::fs::write("label.png", png).unwrap();
}
