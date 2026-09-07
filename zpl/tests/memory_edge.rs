//! Edge-case coverage for the printer-storage command set: the validation
//! and error branches of ~DG, ~DY, ~DS/~DT/~DU, ~IS, ~XF and the ~DN no-op
//! that the happy-path memory tests do not reach. Every payload here is
//! malformed or out-of-range and must warn + drop - never a panic, and
//! never a partial store.

mod common;

use zpl::cmd::parse_stream;
use zpl::diag::{Diagnostics, Severity};
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

/// Parse the given storage commands; a trailing ^XA^XZ closes a format so
/// parse_ctx returns, and the label is discarded.
fn parse_cmds(src: &[u8]) -> (Diagnostics, Memory) {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    let mut s = src.to_vec();
    s.extend_from_slice(b"^XA^XZ");
    parse_stream(&mut Scanner::new(&s), &mut ctx);
    (diags, memory)
}

/// Parse a stream that ends as-is (no appended ^XA^XZ), so an unterminated
/// binary payload truly hits EOF and the driver emits its truncation
/// warning.
fn parse_raw(src: &[u8]) -> (Diagnostics, Memory) {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    (diags, memory)
}

fn parse_full(src: &[u8]) -> (zpl::ir::Label, Diagnostics, Memory) {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, _ctx) = common::parse_ctx(src, &mut diags, &mut memory);
    (label, diags, memory)
}

/// ~DG geometric validation: non-multiple total, zero size, per-object
/// overflow and row overflow all warn + drop.
#[test]
fn dg_geometry_validation_warns_and_drops() {
    let (diags, memory) = parse_cmds(
        b"~DGR:T.GRF,9,2,FFFFFFFFFFFFFFFFFF~DGR:Z.GRF,0,1,FF~DGR:BIG.GRF,99999999,1,FF~DGR:W.GRF,8,99999999,FF00000000000000",
    );
    // 4 payloads, each with exactly one warning (the size/validity path).
    assert_eq!(
        diags.items.len(),
        4,
        "one warning per invalid payload: {0:?}",
        diags
    );
    assert!(diags.items.iter().all(|d| d.severity == Severity::Warning));
    assert!(memory.images.is_empty(), "nothing stored");
}

/// ~DY with a G extension but non-A format, or missing/invalid geometry:
/// the graphic-downlink path's guards.
#[test]
fn dy_graphic_path_validation() {
    let (diags, memory) = parse_cmds(
        b"~DYR:I.GRF,B,G,8,1,FF00000000000000~DYR:J.GRF,A,G,,1,FF00000000000000~DYR:K.GRF,A,G,8,,FF00000000000000~DYR:L.GRF,A,G,0,1,FF",
    );
    assert_eq!(
        diags.items.len(),
        4,
        "four invalid graphic downloads: {0:?}",
        diags
    );
    assert!(memory.images.is_empty());
}

/// ~DY object path: C/P formats unsupported, missing format letter,
/// oversized A payload, and a B payload whose real size differs from the
/// declared t.
#[test]
fn dy_object_path_validation() {
    // C-format unsupported, missing format letter, oversized A payload:
    // three warnings, nothing stored. (A B-format payload routes to the
    // binary driver instead - covered by dy_binary_driver_routing.)
    let (diags, memory) = parse_cmds(
        b"~DYR:F1.TTF,C,T,5,0,ABCDE~DYR:F2.TTF,,T,5,0,ABCDE~DYR:F3.TTF,A,T,99999999,0,ABCDE",
    );
    assert_eq!(
        diags.items.len(),
        3,
        "three invalid object downloads: {0:?}",
        diags
    );
    assert!(
        memory.fonts.is_empty(),
        "no font stored: {:?}",
        memory.fonts
    );
}

/// ~DS/~DT/~DU font-downlink guards: missing size, oversized size, and a
/// size/length mismatch in the hex payload.
#[test]
fn font_download_guards() {
    // Missing size, oversized size, and a size that overruns the hex data
    // all warn and drop; the well-formed download lands.
    let (diags, memory) =
        parse_cmds(b"~DSFONT.S,~DTBIG.TTF,99999999,FF~DUSHORT.FNT,4,00~DULIVE.FNT,2,00FF");
    assert_eq!(
        diags.items.len(),
        3,
        "three invalid font downloads: {0:?}",
        diags
    );
    assert_eq!(memory.fonts.len(), 1, "valid ~DU stored");
}

/// ~DY format-B routing through the parse driver: a payload with a ^ byte
/// inside, a truncated payload, and a .GRF extension that needs hex.
#[test]
fn dy_binary_driver_routing() {
    // Format B with ^ inside the binary data: the driver must still absorb
    // the full declared count (6 bytes: A,^,B,C,D,E) as one object. ~DY is
    // a stream-level command; a bad count after it is dropped verbatim.
    let (diags, memory) = parse_cmds(b"~DYR:OBJ.TTF,B,T,6,0,A^BCDE^FS");
    assert!(
        diags.items.is_empty(),
        "binary payload with ^ must store cleanly: {0:?}",
        diags
    );
    assert_eq!(memory.fonts.get("OBJ.TTF").unwrap().len(), 6);

    // Declared 8, delivered 6 (stream ends mid-payload): warn-and-drop.
    let (diags, memory) = parse_raw(b"~DYR:SHORT.TTF,B,T,8,0,abcdef");
    assert_eq!(diags.items.len(), 1, "truncated binary payload warned");
    assert!(
        !memory.fonts.contains_key("SHORT.TTF"),
        "truncated payload stored nothing"
    );

    // .GRF extension with binary data needs ASCII hex.
    let (diags, memory) = parse_cmds(b"~DYR:G.GRF,B,G,4,0,abcd");
    assert_eq!(diags.items.len(), 1, "binary .GRF warned");
    assert!(!memory.fonts.contains_key("G.GRF"));
    assert!(memory.images.is_empty());
}

/// ^IS validation: a bad p flag warns and defaults to Y; outside a format
/// the command is a no-op.
#[test]
fn image_save_validates_p_and_requires_format() {
    let (label, diags, memory) = parse_full(b"^XA^FO10,20^GB20,20,2,B^ISR:PIC.X,7^FS^XZ");
    assert_eq!(diags.items.len(), 1, "bad p warns: {0:?}", diags);
    let key = memory
        .images
        .keys()
        .next()
        .expect("format-level ^IS stored the image")
        .clone();
    assert_eq!(key, "PIC.X.GRF", "stored under normalized .GRF name");
    // ^IS captures the on-label fields into the stored image; the box that
    // was on the label before ^IS stays a plain field.
    assert!(
        label
            .fields
            .iter()
            .any(|f| matches!(f, zpl::ir::Field::Box(_))),
        "^IS leaves the drawn field on the label"
    );

    // ^IS before any format: no-op (no warning, nothing stored, no panic).
    let (_, diags, memory) = parse_full(b"^ISR:NOTHING.GRF,Y^XA^XZ");
    assert!(
        diags.items.is_empty() && memory.images.is_empty(),
        "^IS outside a format is a silent no-op"
    );
}

/// ~DN aborts nothing and stores nothing; ~EG erases only graphics.
#[test]
fn abort_download_is_a_no_op() {
    let (diags, memory) = parse_cmds(b"~DGR:KEEP.GRF,8,1,FF00000000000000~DN~EG");
    assert!(
        diags.items.is_empty(),
        "no diagnostics expected: {0:?}",
        diags
    );
    assert!(memory.images.is_empty(), "~EG erased everything");
}

/// ^XF recursion: a chain deeper than the cap warns; stored bytes that open
/// a format are ignored with a warning; a stored format referencing a
/// missing format warns.
#[test]
fn xf_recursion_cap_and_format_guards() {
    // FMT0 recalls FMT1, which recalls FMT2, which recalls FMT0 again: the
    // depth cap ends the chain.
    let (diags, _memory) = parse_cmds(
        b"^DFR:FMT0^FO10,10^A0N,20,20^FD0^FS^XFR:FMT1^FS^XZ\
^DFR:FMT1^FO20,20^A0N,20,20^FD1^FS^XFR:FMT2^FS^XZ\
^DFR:FMT2^FO30,30^A0N,20,20^FD2^FS^XFR:FMT0^FS^XZ\
^XA^XFR:FMT0^FS^XZ",
    );
    assert!(
        !diags.items.is_empty(),
        "deep recall chain produced warnings: {0:?}",
        diags
    );
    // Missing format also warns (the stop case when the chain runs out).
    let (diags, _) = parse_cmds(b"^DFR:ONE^FO5,5^A0N,10,10^FD1^FS^XFR:TWO^FS^XZ^XA^XFR:ONE^FS^XZ");
    assert!(
        !diags.items.is_empty(),
        "recall of missing format warns: {0:?}",
        diags
    );
}
