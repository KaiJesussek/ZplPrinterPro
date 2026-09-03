//! Task 18: fuzz-based parser/renderer invariants (proptest).
//!
//! Four properties over adversarial byte strings, capped at the plan's
//! 64 KiB ceiling. Input is arbitrary bytes (many `^`/`~`, huge
//! parameters, truncated formats, empty fields, binary garbage) plus a
//! ZPL "command soup" (real mnemonics with hostile parameter bytes and
//! closed `^FD` chains) so complete barcode fields - the 2D encoders in
//! particular - and `^SN` serial expansion actually run against
//! adversarial data instead of being starved by random bytes never
//! closing a field.
//!
//! 1. `parse` - the public path the server's driver uses (Scanner +
//!    dispatch + parse_stream + ParseContext) - completes on ANY input:
//!    it returns the closed label or `None` (malformed input degrades to
//!    diagnostics), never panics, terminates.
//! 2. The IR serde round-trip holds on fuzz-produced labels (serialize ->
//!    deserialize -> PartialEq).
//! 3. `render` on any fuzz label never panics and its canvas stays inside
//!    the 32000^2 cap (DefaultFontProvider + empty Memory + default
//!    PrinterMode; height 0 drives the auto-height path).
//! 4. Inputs at the 64 KiB ceiling terminate.
//!
//! Regression corpus: any panic found is persisted into
//! `proptest-regressions/` (see the config below) and replayed on every
//! run; the minimal failing input is additionally pinned as a plain test.

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::fonts::DefaultFontProvider;
use zpl::ir::Label;
use zpl::memory::Memory;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{ParseContext, PrinterMode};

/// The plan's input ceiling; everything below it must terminate.
const MAX_INPUT: usize = 64 * 1024;
/// The renderer's canvas cap (`render::MAX_LABEL_DOTS` is private).
const MAX_DOTS: u32 = 32_000;

/// Every run replays the regression corpus (and PROPTEST_CASES overrides
/// the case count for dedicated hunts).
fn config() -> ProptestConfig {
    ProptestConfig {
        // One shared regression file (Direct treats the string as the
        // file path; parent dirs are created as needed).
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "proptest-regressions/fuzz.txt",
        ))),
        ..ProptestConfig::default()
    }
}

/// Arbitrary bytes, capped at 64 KiB.
fn raw_input() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=MAX_INPUT)
}

/// A ZPL "command soup": real mnemonics with hostile parameter bytes,
/// closed `^FD` bodies, and complete barcode / serial / `^GF` chains - so
/// the encoders (2D included), serial expansion, and graphic decoding
/// actually run against adversarial data. Assembled inside a `^XA`/`^XZ`
/// frame and hard-capped at the 64 KiB input ceiling: pieces that would
/// overflow the cap are dropped whole, never cut mid-command.
///
/// Payload bytes are distribution-aware: pure random bytes close a `^FD`
/// field at the first `^`, so barcode data would never reach the
/// encoders' capacity/oversize paths. A mix of raw bytes (binary garbage
/// realism), printable ASCII without command characters, and digits keeps
/// fields open long enough to exercise the full encoder/serial/graphic
/// machinery while hostile cases still occur.
fn soup_input() -> impl Strategy<Value = Vec<u8>> {
    const MNEMONICS: &[&[u8]] = &[
        b"XA", b"XZ", b"XF", b"XG", b"IL", b"IS", b"ID", b"DF", b"DG", b"DY",
        b"PW", b"LL", b"LH", b"LT", b"LS", b"PO", b"PM", b"PQ", b"FO", b"FT",
        b"FD", b"FS", b"FR", b"FB", b"FH", b"FW", b"A ", b"CI", b"BY", b"BC",
        b"SN", b"GF", b"GB", b"GC", b"GE", b"GD", b"GS", b"B1", b"B2", b"B3",
        b"B5", b"B8", b"B9", b"BA", b"BE", b"BK", b"BM", b"BU",
    ];
    const GEOM: &[&[u8]] = &[
        b"PW", b"LL", b"LH", b"LT", b"LS", b"PO", b"PM", b"PQ", b"FO", b"FT",
        b"FB", b"A ", b"BY", b"CI", b"GB", b"GC", b"GE", b"GD", b"FW",
    ];
    const SYMS: &[&[u8]] = &[
        b"Q", b"X", b"7", b"0", b"C", b"1", b"2", b"3", b"5", b"8", b"9", b"A",
        b"E", b"K", b"M", b"U",
    ];

    // Hostile, mostly non-numeric parameter bytes (may include ^/~).
    let param = prop::collection::vec(any::<u8>(), 0..=128);
    // Digit/comma/- soup: numeric parameters that reach clamping,
    // saturation, and arithmetic (serials, geometry, quantities).
    let numparam = prop::collection::vec(prop_oneof![0x30u8..=0x39, Just(0x2Cu8), Just(0x2Du8)], 0..=64);
    // ^FD payload bytes: raw (may close the field early) or printable
    // ASCII without ^/~.
    let payload_byte = prop_oneof![any::<u8>(), 0x20u8..=0x5D, 0x60u8..=0x7D];
    let serial_data = prop::collection::vec(payload_byte, 0..=512);
    // Barcode bodies: `^` is the only byte that closes a ^FD field, so
    // map it away and keep the full byte space otherwise - hostile
    // content with open-field runs, generated at `any::<u8>()` speed
    // (proptest's per-element union strategy is ~30x slower at 55 K
    // elements). Oversized bodies (relative to the 2D encoders'
    // capacities) are part of the hunt: QR caps near 2.7 KB, DataMatrix
    // near 1.5 KB.
    let barcode_byte = any::<u8>().prop_map(|b| if b == b'^' { b'}' } else { b });
    let barcode_data = prop::collection::vec(barcode_byte, 0..=55_000);

    // Raw garbage run (may contain ^/~ - truncation and re-parse).
    let raw = prop::collection::vec(any::<u8>(), 0..=512);
    // A bare command: ^<mnemonic><hostile params, may include ^/~>.
    let plain = (prop::sample::select(MNEMONICS), param.clone()).prop_map(|(m, p)| {
        let mut c = Vec::with_capacity(3 + p.len());
        c.extend_from_slice(b"^");
        c.extend_from_slice(m);
        c.extend(p);
        c
    });
    // A geometry/numeric command: ^<mnemonic><digit/comma params> -
    // hostile dimensions, quantities, and codepage values.
    let numeric = (prop::sample::select(GEOM), numparam.clone()).prop_map(|(m, p)| {
        let mut c = Vec::with_capacity(3 + p.len());
        c.extend_from_slice(b"^");
        c.extend_from_slice(m);
        c.extend(p);
        c
    });
    // A complete barcode field: ^B<sym><params>^FD<data>^FS.
    let barcode = (prop::sample::select(SYMS), param.clone(), barcode_data).prop_map(|(s, p, d)| {
        let mut c = Vec::with_capacity(8 + p.len() + d.len());
        c.extend_from_slice(b"^B");
        c.extend_from_slice(s);
        c.extend(p);
        c.extend_from_slice(b"^FD");
        c.extend(d);
        c.extend_from_slice(b"^FS");
        c
    });
    // A complete serialized field: ^SN<v,d,c>^FD<data>^FS - digit params
    // drive counter clamping/overflow, digit-rich data drives indexing.
    let serial = (numparam, serial_data).prop_map(|(p, d)| {
        let mut c = Vec::with_capacity(8 + p.len() + d.len());
        c.extend_from_slice(b"^SN");
        c.extend(p);
        c.extend_from_slice(b"^FD");
        c.extend(d);
        c.extend_from_slice(b"^FS");
        c
    });
    // A complete inline-graphic chain: ^GF<params><hex/row-op data>.
    let gfx_byte = prop_oneof![
        0x30u8..=0x39,
        0x41u8..=0x46,
        Just(0x2Cu8),
        Just(0x3Au8),
        Just(0x3Bu8),
        any::<u8>(),
    ];
    let graphic_data = prop::collection::vec(gfx_byte, 0..=2048);
    let graphic = (param, graphic_data).prop_map(|(p, d)| {
        let mut c = Vec::with_capacity(5 + p.len() + d.len());
        c.extend_from_slice(b"^GF");
        c.extend(p);
        c.extend(d);
        c
    });

    prop::collection::vec(
        prop_oneof![raw, plain, numeric, barcode, serial, graphic],
        0..=32,
    )
    .prop_map(|pieces: Vec<Vec<u8>>| {
        let mut out = Vec::new();
        out.extend_from_slice(b"^XA");
        for p in pieces {
            if out.len() + p.len() > MAX_INPUT - 3 {
                break;
            }
            out.extend(p);
        }
        out.extend_from_slice(b"^XZ");
        out
    })
}

/// Parse a complete ZPL stream through the same public path the server
/// uses. Parsing never fails hard - malformed input degrades to
/// diagnostics - so the result is the closed label, or `None` when no
/// `^XZ` closed a format.
fn parse_fuzz(src: &[u8]) -> Option<Label> {
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
    ctx.pending_label.take()
}

/// Render the label with the server's default options. The label's own
/// dims drive the canvas (`height_dots == 0` -> the auto-height path);
/// the canvas must respect the 32000^2 cap and never be zero-sized.
fn render_fuzz(label: &Label) {
    let fonts = DefaultFontProvider::new();
    let memory = Memory::default();
    let mode = PrinterMode::default();
    let opts = RenderOptions { dpi: 203, fonts: &fonts, memory: &memory, mode: &mode };
    let out = render(label, &opts).expect("fuzz label renders (defined RenderError only)");
    let (w, h) = (out.bitmap.width, out.bitmap.height);
    assert!(w >= 1 && h >= 1, "canvas is never zero-sized");
    assert!(w <= MAX_DOTS && h <= MAX_DOTS, "canvas respects the 32000\u{00B2} cap (got {w}\u{00D7}{h})");
}

proptest! {
    #![proptest_config(config())]

    /// Property 1: parse completes on arbitrary bytes up to the 64 KiB cap
    /// - returns a value, never panics, terminates.
    #[test]
    fn parse_arbitrary_bytes_never_panics(src in raw_input()) {
        let _ = parse_fuzz(&src);
    }

    /// Property 2: the IR serde round-trip holds on fuzz-produced labels
    /// (soup inputs so barcode/serial/graphic fields genuinely close).
    #[test]
    fn serde_roundtrip_fuzz_labels(src in soup_input()) {
        let Some(label) = parse_fuzz(&src) else { return Ok(()); };
        let json = serde_json::to_string(&label).expect("label serializes");
        let back: Label = serde_json::from_str(&json).expect("label deserializes");
        prop_assert_eq!(label, back);
    }

    /// Property 3: render on any fuzz label never panics and stays inside
    /// the canvas cap.
    #[test]
    fn render_never_panics(src in soup_input()) {
        let Some(label) = parse_fuzz(&src) else { return Ok(()); };
        render_fuzz(&label);
    }

    /// Property 4: inputs at the 64 KiB ceiling terminate.
    #[test]
    fn near_cap_input_terminates(src in prop::collection::vec(any::<u8>(), (MAX_INPUT - 4096)..=MAX_INPUT)) {
        let _ = parse_fuzz(&src);
    }
}

// --- Regression pins (panics found by the fuzz hunts) ----------------------

/// Code 128's mod-103 check digit accumulated `sum of value*position` in u32,
/// which overflowed (debug-build panic) on barcode bodies beyond ~9 KB of
/// data - the weighted sum reaches ~10^1^1. Fuzzing found the panic; the
/// accumulator is u64 now. 55 KB of data must parse and render cleanly.
#[test]
fn checksum_overflow_large_code128() {
    let mut src = Vec::with_capacity(55_050);
    src.extend_from_slice(b"^XA^BC^FD");
    src.extend(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(2116).as_slice()); // 55,016 B
    src.extend_from_slice(b"^FS^XZ");
    let label = parse_fuzz(&src).expect("format closes");
    render_fuzz(&label);
}

/// Hostile geometry: `^PW`/`^LL` values far past the guide's 32000, a
/// huge box, and an oversized graphic request - all render inside the
/// 32000^2 canvas cap without panicking.
#[test]
fn hostile_geometry_renders() {
    let label = parse_fuzz(b"^XA^PW99999999^LL32000^FO0,0^GB99999,99999,99999^FS^FDover^FS^XZ")
        .expect("format closes");
    render_fuzz(&label);
}
