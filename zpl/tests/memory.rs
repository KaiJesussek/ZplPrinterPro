//! Integration tests for the Task 7 printer-storage command set:
//! ~DG ^DF ^XF ^XG ^ID ^IL ^IM ^IS ~DY ~DS ~DT ~DU ~EG ~DN ^A@ ^GS.
//!
//! The stored-image count (`Memory.images.len()`, `~HS` string-2 `www`) must
//! stay live and truthful through stores and deletions.

mod common;

use common::parse_ctx;
use zpl::cmd::parse_stream;
use zpl::diag::{Diagnostics, Severity};
use zpl::fonts::DefaultFontProvider;
use zpl::ir::{Field, GraphicKind, Rotation};
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

fn text_field(label: &zpl::ir::Label, i: usize) -> &zpl::ir::TextField {
    match &label.fields[i] {
        Field::Text(t) => t,
        other => panic!("expected text field, got {other:?}"),
    }
}

/// Parse a definition-style stream (storage commands that deliberately
/// produce no label - ^DF captures, downloads, deletions) into the memory
/// store, discarding any unfinished-format state.
fn define(src: &[u8], diags: &mut Diagnostics, memory: &mut Memory) {
    let mut ctx = ParseContext::new(diags, memory);
    parse_stream(&mut Scanner::new(src), &mut ctx);
}

#[test]
fn dg_stores_and_xg_recalls() {
    // ~DG stores an 8x8 image (8 bytes, 1 byte per row: one black row, then
    // white). ^XG recalls it by name into a GraphicField::Stored; the
    // image count (`www`) is live from memory.images.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DGR:SAMPLE.GRF,8,1,FF00000000000000^XA^FO10,20^XGR:SAMPLE.GRF,1,1^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.memory.images.len(), 1, "www must count stored images");
    let img = ctx
        .memory
        .images
        .get("SAMPLE.GRF")
        .expect("~DG stored the image under its normalized name");
    assert_eq!(img.width, 8, "dot width = bytes per row * 8");
    assert_eq!(img.height, 8, "height rows = t / w");
    assert_eq!(img.data, vec![0xFF, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(label.fields.len(), 1);
    match &label.fields[0] {
        Field::Graphic(g) => {
            assert_eq!((g.x, g.y), (10, 20));
            match &g.kind {
                GraphicKind::Stored(name) => assert_eq!(name, "SAMPLE.GRF"),
                other => panic!("expected stored graphic, got {other:?}"),
            }
        }
        other => panic!("expected graphic field, got {other:?}"),
    }

    // A bare name (no device, no extension) normalizes to NAME.GRF on both
    // store and recall.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DGLOGO,8,1,FF00000000000000^XA^XGLOGO^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.memory.images.len(), 1);
    assert!(ctx.memory.images.contains_key("LOGO.GRF"));
    assert!(matches!(&label.fields[0],
        Field::Graphic(g) if matches!(&g.kind, GraphicKind::Stored(n) if n == "LOGO.GRF")));
}

#[test]
fn id_and_eg_prune() {
    // ^ID with a '*' glob deletes matching names; ~EG erases all graphics.
    // `www` (images.len()) is truthful after each step.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    {
        let (_, ctx) = parse_ctx(
            b"~DGR:OTHER.GRF,8,1,FF00000000000000~DGR:LOGO.GRF,8,1,FF00000000000000^XA^IDR:OTHER.*^XZ",
            &mut diags,
            &mut memory,
        );
        assert_eq!(
            ctx.diags.items.len(),
            0,
            "unexpected diagnostics: {0:?}",
            ctx.diags
        );
        assert_eq!(ctx.memory.images.len(), 1, "^ID deleted one of two (www=1)");
        assert!(ctx.memory.images.contains_key("LOGO.GRF"));
        assert!(!ctx.memory.images.contains_key("OTHER.GRF"));
    }
    let (_, ctx) = parse_ctx(b"~EG^XA^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.memory.images.len(), 0, "~EG erased the store (www=0)");
}

#[test]
fn dn_aborts_download_without_touching_images() {
    // The guide defines ~DN as "Abort Download Graphic": it must NOT delete
    // stored images (deletions go through ^ID / ~EG). `www` stays truthful.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:LOGO.GRF,8,1,FF00000000000000~DNR:SAMPLE.GRF^XA^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.memory.images.len(), 1, "~DN leaves stored images alone");
    assert!(ctx.memory.images.contains_key("LOGO.GRF"));
}

#[test]
fn id_globs_across_stores() {
    // ^ID matches against images, formats and fonts by key; `*.*` erases
    // everything at once.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:LOGO.GRF,8,1,FF00000000000000^DFR:FMT.ZPL^FO10,10^FS~DYE:ARIAL.TTF,A,T,4,0,00FF00FF^XA^IDR:*.GRF^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.memory.images.len(), 0, "*.GRF deleted the image");
    assert!(
        ctx.memory.formats.contains_key("FMT.ZPL"),
        "formats survive *.GRF"
    );
    assert!(
        ctx.memory.fonts.contains_key("ARIAL.TTF"),
        "fonts survive *.GRF"
    );

    let (_, ctx) = parse_ctx(b"^XA^IDR:FMT.ZPL^XZ~EG", &mut diags, &mut memory);
    assert!(
        !ctx.memory.formats.contains_key("FMT.ZPL"),
        "exact ^ID deletes the format"
    );
    // ~EG erases graphics only (the ~HS www count), not fonts/formats.
    assert!(ctx.memory.fonts.contains_key("ARIAL.TTF"));
    assert_eq!(ctx.memory.images.len(), 0);
}

#[test]
fn bare_id_defaults_to_unknown() {
    // A name-less ^ID targets the UNKNOWN default object (guide p.245):
    // stored names normalize to UNKNOWN.GRF / UNKNOWN.ZPL (and fonts keep
    // the bare UNKNOWN), so the default ^ID pattern must glob `UNKNOWN*`
    // rather than match the literal name - otherwise the deletion misses
    // every default-stored object (the T7 deferred finding). Accepted
    // silently.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:UNKNOWN.GRF,8,1,FF00000000000000^XA^ID^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.diags.items.len(), 0, "bare ^ID must not warn");
    assert_eq!(
        ctx.memory.images.len(),
        0,
        "bare ^ID (defaulting to UNKNOWN*) deletes the stored UNKNOWN.GRF"
    );

    // All stores are swept: a format and a font stored under the UNKNOWN
    // defaults go too.
    let (_, ctx) = parse_ctx(
        b"^DF^FO10,10^FS~DS,2,0000^XA^ID^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(ctx.diags.items.len(), 0, "bare ^ID must not warn");
    assert!(memory.formats.is_empty(), "UNKNOWN.ZPL deleted");
    assert!(memory.fonts.is_empty(), "UNKNOWN.FNT deleted");

    // An explicit UNKNOWN.* glob still reaches whatever survived.
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:UNKNOWN.GRF,8,1,FF00000000000000^XA^IDR:UNKNOWN.*^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.memory.images.len(),
        0,
        "UNKNOWN.* glob reaches the default image"
    );
}

#[test]
fn df_xf_roundtrip() {
    // ^DF captures the format bytes until ^FS and stores them; a later
    // ^XA^XF recalls and re-executes them into the label - same fields.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // ^DF captures everything until ^FS and stores it; a definition stream
    // (guide: ^DF right after ^XA, content saved, ^FS) produces no label.
    define(
        b"^DFR:FMT.ZPL^FO10,10^A0,30,30^FDHello^FS",
        &mut diags,
        &mut memory,
    );
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    let stored = memory
        .formats
        .get("FMT.ZPL")
        .expect("^DF stored the format");
    assert_eq!(stored, b"^FO10,10^A0,30,30^FDHello");
    let (label, ctx) = parse_ctx(b"^XA^XFR:FMT.ZPL^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.fields.len(), 1);
    let t = text_field(&label, 0);
    assert_eq!((t.x, t.y), (10, 10));
    assert_eq!(t.font, [b'A', b'0']);
    assert_eq!((t.font_height, t.font_width), (30, 30));
    assert_eq!(t.data, "Hello");
}

#[test]
fn xf_depth_and_missing_format() {
    // A missing format warns and the parse continues without panicking.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(b"^XA^XFR:NOPE.ZPL^XZ", &mut diags, &mut memory);
    assert!(ctx
        .diags
        .items
        .iter()
        .any(|d| d.severity == Severity::Warning));
    assert!(ctx
        .diags
        .items
        .iter()
        .any(|d| d.message.contains("NOPE.ZPL")));

    // A stored format that recalls itself hits the depth cap: the recall
    // chain is bounded (never infinite), the parse completes, and the
    // format's own fields still land in the label.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    define(
        b"^DFR:F1.ZPL^A0,10,10^FDone^FS^DFR:F3.ZPL^XFR:F3.ZPL^XFR:F3.ZPL^FS",
        &mut diags,
        &mut memory,
    );
    let (label, ctx) = parse_ctx(b"^XA^XFR:F1.ZPL^XFR:F3.ZPL^FS^XZ", &mut diags, &mut memory);
    // F1's text field is kept even though the self-referencing F3 chain was
    // cut at the depth cap.
    assert_eq!(label.fields.len(), 1);
    assert_eq!(text_field(&label, 0).data, "one");
    let warns = ctx
        .diags
        .items
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    assert!(
        warns >= 2,
        "expected depth-cap + missing-recursion warnings, got {0:?}",
        ctx.diags.items
    );
}

#[test]
fn aa_selects_downloaded_font() {
    // ~DY downloads font bytes (hex form); ^A@ names the downloaded font on
    // the field and applies orientation/size.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DYE:FONTFILE.TTF,A,T,4,0,00FF00FF^XA^FO10,10^A@N,50,50,E:FONTFILE.TTF^FDHi^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    let font = ctx
        .memory
        .fonts
        .get("FONTFILE.TTF")
        .expect("~DY stored the font");
    assert_eq!(font, &vec![0x00, 0xFF, 0x00, 0xFF]);
    let t = text_field(&label, 0);
    assert_eq!(t.font_name.as_deref(), Some("FONTFILE.TTF"));
    assert_eq!(t.rotation, Rotation::R0);
    assert_eq!((t.font_height, t.font_width), (50, 50));

    // Carry-over (guide ^A@, main example): a name-less ^A@ keeps the last
    // named font and applies its own orientation/size (R here).
    let (label, ctx) = parse_ctx(
        b"~DYE:FONTFILE.TTF,A,T,4,0,00FF00FF^XA^FO10,10^A@N,50,50,E:FONTFILE.TTF^FDHi^FS^FO10,60^A@R,40,40^FDx^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.fields.len(), 2);
    let carried = text_field(&label, 1);
    assert_eq!(
        carried.font_name.as_deref(),
        Some("FONTFILE.TTF"),
        "^A@ carries the named font"
    );
    assert_eq!(carried.rotation, Rotation::R90);
    assert_eq!((carried.font_height, carried.font_width), (40, 40));

    // Before any name is given, a name-less ^A@ keeps the ^CF default.
    let (label, ctx) = parse_ctx(b"^XA^FO10,10^A@R,40,40^FDx^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    let t = text_field(&label, 0);
    assert_eq!(t.rotation, Rotation::R90);
    assert_eq!((t.font_height, t.font_width), (40, 40));
    assert_eq!(t.font_name, None);
}

#[test]
fn eg_erases_graphics_ds_downloads_font() {
    // ~EG erases all stored graphics (the ^ID erase family); ~DS downloads a
    // scalable font as hex bytes into fonts.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:A.GRF,8,1,FF00000000000000~DGR:B.GRF,8,1,FF00000000000000~EG^XA^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(ctx.memory.images.len(), 0, "~EG erased all graphics");

    let (_, ctx) = parse_ctx(
        b"~DSB:CGTIMES.FNT,4,00FF00FF^XA^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(
        ctx.memory.fonts.get("CGTIMES.FNT"),
        Some(&vec![0x00, 0xFF, 0x00, 0xFF])
    );
}

#[test]
fn il_im_recall_placement_and_is_stores() {
    // ^IM inside an open field behaves like ^XG without sizing; ^IL places
    // the stored image at 0,0 without an ^FS. ^IS (guide p.249) saves the
    // format-so-far as a stored graphic image - stored at ^XZ, recallable
    // by ^IL/^XG. A bare ^IS in an empty format stores the blank label.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DGR:LOGO.GRF,8,1,FF00000000000000^XA^ILR:LOGO.GRF^FO100,100^IMR:LOGO.GRF^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert_eq!(label.fields.len(), 2);
    assert!(matches!(&label.fields[0],
        Field::Graphic(g) if g.x == 0 && g.y == 0
            && matches!(&g.kind, GraphicKind::Stored(n) if n == "LOGO.GRF")));
    assert!(matches!(&label.fields[1],
        Field::Graphic(g) if g.x == 100 && g.y == 100
            && matches!(&g.kind, GraphicKind::Stored(n) if n == "LOGO.GRF")));

    let (_, ctx) = parse_ctx(
        b"^XA^PW128^LL40^FO0,0^A0N,20,20^FDX^FS^ISR:SNAP.GRF^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "^IS on valid input warns nothing: {0:?}",
        ctx.diags
    );
    let img = ctx
        .memory
        .images
        .get("SNAP.GRF")
        .expect("^IS stored the rendered label image");
    assert!(
        img.data.iter().any(|&b| b != 0),
        "the saved raster carries the X glyph"
    );
}

#[test]
fn is_round_trips_identical_pixels() {
    // The ^IS stored bitmap is EXACTLY the rendered format-so-far, and a
    // recall of it (^IL - the guide pairs them, p.247: "The graphic was
    // stored using the ^IS command") paints the same pixels. Fields after
    // the ^IS are not part of the saved image.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"^XA^PW128^LL40^FO0,0^A0N,20,20^FDX^FS^ISR:SNAP.GRF^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let fonts = DefaultFontProvider::new();
    let opts = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let out = render(&label, &opts).expect("format renders");
    let img = &ctx.memory.images["SNAP.GRF"];
    assert_eq!(
        (img.width, img.height),
        (out.bitmap.width, out.bitmap.height),
        "stored image is the rendered label's size"
    );
    assert_eq!(
        img.data, out.bitmap.bits,
        "stored image bytes are the render"
    );

    // The ^IS came after the X field only: a later field must not be part
    // of the image.
    let (label2, ctx2) = parse_ctx(
        b"^XA^PW128^LL40^FO0,0^A0N,20,20^FDX^FS^ISR:EARLY.GRF^FS^FO30,0^A0N,20,20^FDY^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let mut sub = label2.clone();
    sub.fields.truncate(1);
    let opts2 = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx2.memory,
        mode: &ctx2.mode,
    };
    let out2 = render(&sub, &opts2).expect("early fields render");
    assert_eq!(
        ctx2.memory.images["EARLY.GRF"].data, out2.bitmap.bits,
        "fields after ^IS are excluded from the saved image"
    );

    // Recall: ^IL of the saved image paints the identical pixels.
    let (label3, ctx3) = parse_ctx(b"^XA^PW128^LL40^ILR:SNAP.GRF^XZ", &mut diags, &mut memory);
    let opts3 = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx3.memory,
        mode: &ctx3.mode,
    };
    let out3 = render(&label3, &opts3).expect("recalled image renders");
    assert_eq!(
        (out3.bitmap.width, out3.bitmap.height),
        (out.bitmap.width, out.bitmap.height)
    );
    assert_eq!(
        out3.bitmap.bits, out.bitmap.bits,
        "recall paints the same pixels"
    );
}

#[test]
fn png_named_images_store_and_recall() {
    // Guide ^IL/^IM (p.247/248): the stored-image extension is "Fixed
    // Value: .GRF, .PNG". A PNG-named image stores and recalls under the
    // SAME key - the old normalization appended .GRF to it ("LOGO.PNG.GRF",
    // an object name with a period outside the guide's "1 to 8
    // alphanumeric characters" rule). ~DG (p.175) still appends .GRF to
    // any other/absent extension.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DGR:LOGO.PNG,8,1,FF00000000000000^XA^PW128^LL28^FO10,20^XGR:LOGO.PNG,1,1^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    let img = ctx
        .memory
        .images
        .get("LOGO.PNG")
        .expect("stored under its .PNG name, not LOGO.PNG.GRF");
    assert_eq!(img.data, vec![0xFF, 0, 0, 0, 0, 0, 0, 0]);
    assert!(matches!(&label.fields[0],
        Field::Graphic(g) if matches!(&g.kind, GraphicKind::Stored(n) if n == "LOGO.PNG")));

    // Recall by rendering the stored recall paints the identical pixels
    // as the inline blit of the same bytes at the same origin.
    let fonts = DefaultFontProvider::new();
    let opts = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx.memory,
        mode: &ctx.mode,
    };
    let recalled = render(&label, &opts).expect("recalled graphic renders");
    let (label_gfa, ctx_gfa) = parse_ctx(
        b"^XA^PW128^LL28^FO10,20^GFA,8,8,1,FF00000000000000^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let opts_gfa = RenderOptions {
        dpi: 203,
        fonts: &fonts,
        memory: &*ctx_gfa.memory,
        mode: &ctx_gfa.mode,
    };
    let inline = render(&label_gfa, &opts_gfa).expect("inline blit renders");
    assert_eq!(
        (recalled.bitmap.width, recalled.bitmap.height),
        (inline.bitmap.width, inline.bitmap.height)
    );
    assert_eq!(
        recalled.bitmap.bits, inline.bitmap.bits,
        "recalling the PNG-named image paints the same pixels as its data"
    );
    // A plain name still normalizes to .GRF for storage and recall.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGLOGO,8,1,FF00000000000000^XA^XGLOGO^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert!(ctx.memory.images.contains_key("LOGO.GRF"));
    // An unknown extension still gets .GRF appended (~DG's literal rule).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DGR:FOO.BMP,8,1,FF00000000000000^XA^XGR:FOO.BMP^FS^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    assert!(ctx.memory.images.contains_key("FOO.BMP.GRF"));
}

#[test]
fn dy_graphic_download_counts_as_image() {
    // ~DY with extension G downloads a raw .GRF bitmap: it lands in the
    // images store, so `www` counts it like a ~DG download.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (_, ctx) = parse_ctx(
        b"~DYR:LOGO.GRF,A,G,8,1,FF00000000000000^XA^XZ",
        &mut diags,
        &mut memory,
    );
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    let img = ctx
        .memory
        .images
        .get("LOGO.GRF")
        .expect("~DY G stored the image");
    assert_eq!((img.width, img.height), (8, 8));
    assert_eq!(img.data, vec![0xFF, 0, 0, 0, 0, 0, 0, 0]);
    assert!(ctx.memory.fonts.is_empty(), "graphics are not fonts");
}

#[test]
fn gs_symbol_field() {
    // ^GS selects the built-in graphic-symbol set for the field (guide: a
    // font-style command - characters A-E in ^FD pick the symbol).
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(b"^XA^FO10,10^GSN,20,20^FD A^FS^XZ", &mut diags, &mut memory);
    assert_eq!(
        ctx.diags.items.len(),
        0,
        "unexpected diagnostics: {0:?}",
        ctx.diags
    );
    let t = text_field(&label, 0);
    assert!(t.symbol);
    assert_eq!((t.font_height, t.font_width), (20, 20));
    assert_eq!(t.rotation, Rotation::R0);
}

#[test]
fn malformed_storage_payloads_warn_and_drop() {
    // Malformed ~DG/~DY payloads: warning + the payload is dropped, parsing
    // continues, never a panic. Nothing lands in the store.
    let src = b"~DGR:JUNK.GRF,8,1,this is not hex!~DGR:HALF.GRF,3,1,FFFF\
~DGR:MISMATCH.GRF,9,1,FF00000000000000~DGR:ROW0.GRF,8,0,FF00000000000000\
~DGR:BIG.GRF,999999999999,1,FF~DGR:ODD.GRF,1,1,M6~DYR:FONT.TTF,A,T,5,0,00FF^XA^XZ";
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(src, &mut diags, &mut memory);
    // Every malformed payload produced a warning; none panicked.
    assert!(
        ctx.diags.items.len() >= 6,
        "expected >= 6 warnings, got {0:?}",
        ctx.diags
    );
    assert!(ctx
        .diags
        .items
        .iter()
        .all(|d| d.severity == Severity::Warning));
    assert!(
        ctx.memory.images.is_empty(),
        "malformed images were dropped"
    );
    assert!(ctx.memory.fonts.is_empty(), "malformed font was dropped");
    assert_eq!(label.fields.len(), 0);
}

#[test]
fn total_memory_cap_warns_and_drops() {
    // The 64 MiB SUM cap (not just per-object limits): hostile accumulation
    // must eventually warn+drop instead of growing RAM / memory.json
    // unboundedly, and `www` (images.len()) stays bounded.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    // 8 MiB images via ~DG (the '!' row op fills each 4096-byte row with
    // 0xFF, so the payload is a few KB); 8 of them fill the cap exactly.
    let rows = 8 * 1024 * 1024 / 4096;
    let fill = "!".repeat(rows);
    for i in 0..8 {
        define(
            format!("~DGR:IMG{i}.GRF,{},{},{}", 8 * 1024 * 1024, 4096, fill).as_bytes(),
            &mut diags,
            &mut memory,
        );
        assert!(
            diags.items.is_empty(),
            "store {i} is within the cap: {:?}",
            diags.items
        );
    }
    let stored: usize = memory.images.values().map(|i| i.data.len()).sum();
    assert_eq!(stored, 64 * 1024 * 1024, "the cap is exactly full");
    assert_eq!(memory.images.len(), 8, "www reflects the 8 stored images");

    // The 9th image must warn and drop; www stays at 8.
    define(b"~DGR:IMG8.GRF,1,1,00", &mut diags, &mut memory);
    assert_eq!(diags.items.len(), 1, "memory-full warns once");
    assert!(
        diags.items[0].message.contains("memory full"),
        "warning text: {}",
        diags.items[0].message
    );
    assert_eq!(memory.images.len(), 8, "www stays limited");

    // Other store sites are capped the same way: a ~DS font and a ~DY
    // binary object over the total limit are dropped; fonts stays empty.
    define(b"~DS,2,0000", &mut diags, &mut memory);
    define(b"~DYR:OBJ.BIN,B,BIN,,X", &mut diags, &mut memory);
    assert_eq!(diags.items.len(), 3, "each over-cap store warns");
    assert!(memory.fonts.is_empty(), "over-cap fonts dropped");

    // A same-key overwrite frees the old bytes, so re-downloading IMG0
    // small still fits.
    define(b"~DGR:IMG0.GRF,1,1,00", &mut diags, &mut memory);
    assert_eq!(
        memory.images["IMG0.GRF"].data,
        vec![0x00],
        "overwrite landed"
    );
    assert_eq!(diags.items.len(), 3, "the shrink fit, no new warning");
    assert_eq!(memory.images.len(), 8, "www unchanged by the overwrite");
}

#[test]
fn dy_b_binary_font_keeps_caret_bytes() {
    // ~DY format B carries raw binary (guide p.181: "all control prefixes
    // and flow control characters are ignored until the total number of
    // bytes needed ... is received"): ^ and ^XZ bytes inside the payload
    // are data - stored verbatim, closing nothing, raising nothing.
    //
    // The guide's two-stream form (`~DY...,SIZE,,` + payload; the data
    // may even arrive in a separate transmission) drops the payload right
    // after the empty w field:
    let payload = [0x70u8, 0x5E, 0x58, 0x5A, 0x7E, 0x71]; // "p^XZ~q"
    let mut src = b"~DYE:FONTFILE.BIN,B,T,6,,".to_vec();
    src.extend_from_slice(&payload);
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    define(&src, &mut diags, &mut memory);
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(
        memory.fonts.get("FONTFILE.BIN").map(Vec::as_slice),
        Some(payload.as_slice()),
        "font bytes stored verbatim"
    );

    // The full six-field form (w present) frames the same way.
    let mut src = b"~DYE:F.BIN,B,T,6,0,".to_vec();
    src.extend_from_slice(&payload);
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    define(&src, &mut diags, &mut memory);
    assert_eq!(diags.items.len(), 0, "unexpected diagnostics: {diags:?}");
    assert_eq!(
        memory.fonts.get("F.BIN").map(Vec::as_slice),
        Some(payload.as_slice())
    );
}

#[test]
fn dy_b_truncated_warns_and_stores_nothing() {
    // Declared byte count beyond the stream: warn, store nothing, and
    // keep parsing - never a panic.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let (label, ctx) = parse_ctx(
        b"~DYE:F.BIN,B,T,99,AB^XA^FO10,10^FDx^FS^XZ",
        &mut diags,
        &mut memory,
    );
    let dy_warns = ctx.diags.items.iter().filter(|d| d.command == "DY").count();
    assert_eq!(dy_warns, 1, "one truncation warning: {diags:?}");
    assert!(
        ctx.diags
            .items
            .iter()
            .all(|d| d.severity == Severity::Warning),
        "{diags:?}"
    );
    assert!(ctx.diags.items[0].message.contains("99"), "{diags:?}");
    assert!(ctx.memory.fonts.is_empty(), "no partial store");
    // The input after the truncation point still parses.
    assert_eq!(label.fields.len(), 1);
    match &label.fields[0] {
        Field::Text(t) => assert_eq!(t.data, "x"),
        other => panic!("expected text field, got {other:?}"),
    }
}

#[test]
fn dy_b_with_grf_extension_warns_hex_required() {
    // ext = G routes ~DY to the stored-image path, which needs ASCII-hex
    // data even for format B (a raw-binary .GRF is not a supported store)
    // - warn and drop, never store into fonts or images.
    let mut diags = Diagnostics::default();
    let mut memory = Memory::default();
    let mut ctx = ParseContext::new(&mut diags, &mut memory);
    parse_stream(&mut Scanner::new(b"~DYE:LOGO.GRF,B,G,6,1,^XZ~AB"), &mut ctx);
    assert!(ctx.diags.items.iter().any(|d| d.command == "DY"));
    assert!(ctx.memory.fonts.is_empty());
    assert!(ctx.memory.images.is_empty());
}
