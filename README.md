# ZplPrinterPro

A ZPL II parser and renderer for Rust, plus a print server that speaks
the protocol over TCP. It turns plain ZPL II format strings into
1-bit grayscale PNGs - text, barcodes, graphics - with geometry and
host-status responses anchored to the *ZPL II Programming Guide*.

The workspace is two crates:

- **`zpl`** - the library. Parses ZPL II into an intermediate
  representation (`^XA`...`^XZ` formats, stored formats, downloaded
  objects) and rasterizes labels: text fields with a bundled TrueType
  face (metrics anchored to the guide's character matrices), 1D and 2D
  barcodes, boxes/circles/diagonals/shading, graphic fields, `^CI`
  codepages, printer memory, and byte-exact host responses
  (`~HS`, `~HQ`, `~HI`, `~HU`, `~WD`, `^HV`).
- **`zpl-server`** - a print server. Accepts raw ZPL sessions on a TCP
  port, archives every label (raw `.zpl` span + rendered PNG), answers
  host queries on the socket, and exposes an HTTP API with a web UI for
  labels, printer state, and storage.

## Quickstart

```sh
cargo run -p zpl-server
```

This starts the server on the defaults below and prints a banner with
the effective settings. Send a label:

```sh
printf '^XA^FO50,50^A0N,30,30^FDHello, ZPL!^FS^XZ' | nc 127.0.0.1 9100
```

The raw session is parsed, the label is archived (`.zpl` + `.png`),
and the web UI is served at <http://localhost:8080>.

### Environment variables

All optional; defaults shown.

| Variable              | Default            | Meaning                                      |
| --------------------- | ------------------ | -------------------------------------------- |
| `ZPL_TCP_PORT`        | `9100`             | Raw ZPL TCP listen port                      |
| `ZPL_HTTP_PORT`       | `8080`             | HTTP API / web UI port                       |
| `ZPL_ARCHIVE_DIR`     | `./zpl-archive`    | Directory for archived labels (`.zpl`+`.png`)|
| `ZPL_ARCHIVE_KEEP`    | `10000`            | Archived labels retained (newest)            |
| `ZPL_ARCHIVE_MAX_MB`  | `1024`             | Archive size cap before oldest is pruned     |

## Library usage

Parse a format and rasterize it to a PNG:

```rust
use zpl::cmd::parse_stream;
use zpl::diag::Diagnostics;
use zpl::fonts::DefaultFontProvider;
use zpl::render::{render, RenderOptions};
use zpl::scanner::Scanner;
use zpl::state::{Memory, ParseContext};

fn main() {
    let format = b"^XA^FO50,50^A0N,30,30^FDHello, ZPL!^FS^XZ";
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
```

This is exactly the file `zpl/examples/render_label.rs` (run it with
`cargo run -p zpl --example render_label`); the example is compiled by
`cargo test --workspace`, so this snippet is guaranteed to match the
current API. The render options pass the parser's printer state
(`^LH`/`^PO`/`^PM`/`^LL`) straight into the renderer; `RenderOptions::dpi`
rescales text and barcode metrics from the guide's 203 dpi reference
values.

## Command coverage

| Area | Implemented | Not yet |
| --- | --- | --- |
| Formats & fields | `^XA`/`^XZ`, `^FO`/`^FT`/`^FV`/`^FR`/`^FS`, `^FD`/`^FH`/`^FC`/`^FN`, `^FB` block formatting, `^FW` rotation, `^SN` serialization, `^PQ` copies | `^PQ r` replicate-serial re-print (v1 prints the first copy's value on every copy) |
| Text & fonts | `^A0`..`^A@` font selection, `^CF` default font, `^CW` font assignment, bundled DejaVu face with guide-anchored cell/baseline metrics | Full downloaded-font parity: `^A@`/`~DS`/`~DT`/`~DU` faces render at a fixed reference size rescaled to the field, not Zebra's exact hinting |
| Printer geometry & config | `^PW`, `^LL` (incl. auto height), `^LH`, `^LS`, `^LT`, `^PO`, `^PM`, `^MD`/`~SD` darkness, `^MN` media tracking, `^MU` units, `^CC`/`^CT` command chars, `~PH` slew, `~TA` tear-off, `~JR` reset, `~WC` config label | `^PR ^MT ^MM ^MF ^MI ^ML ^MP ^MW ^PA ^PF ^SC ^SZ ^ZZ ^XB ^XS ^PN` are parsed and validated but not stored - media/hardware actions with no effect on the label image |
| Graphics | `^GB` box, `^GC` circle, `^GD` diagonal, `^GE` ellipse, `^GS` shading, `^GF` graphic field (hex compression), `^DY`/`~DG` downloads, `~EG` erase, `^XG` recall | SVG/PDF output backends - the renderer is a 1-bit raster (PNG only) |
| 1D barcodes | Code 11 (`^B1`), Interleaved 2 of 5 (`^B2`), Code 39 (`^B3`), Planet (`^B5`), EAN-8 (`^B8`), UPC-E (`^B9`), Code 93 (`^BA`), Code 128 (`^BC`), EAN-13 (`^BE`), Codabar (`^BK`), MSI (`^BM`), UPC-A (`^BU`); `^BY` module/ratio/height | - |
| 2D barcodes | QR (`^BQ`), Data Matrix (`^BX`), PDF417 (`^B7`), Aztec (`^B0`) | - |
| Codepages | `^CI` 0/27 (1252 default), 1-13 (Zebra 850), 28 (UTF-8), 29/30 (UTF-16), 31/33/34/35/36 (1250/1251/1253/1254/1255) | Per-country `^CI` 1-12 substitution tables (v1 decodes them as plain 850); Asian DBCS sets 14/15/16/24/26 and UCS-2 17 warn and fall back to 1252 |
| Memory | `^DF` stored formats (+ `^XF` recall), `^DG`/`^DY` graphics, `~DS`/`~DT`/`~DU` fonts, `~DN` abort, `^ID` delete, `^IL`/`^IS` load/store, `^XG` graphics recall | - |
| Host protocol | `~HS`, `~HQ`, `~HI`, `~HU`, `~WD`, `^HV`; raw TCP sessions parse per chunk and answer on the socket | LPR/SNMP - raw 9100 TCP only |

## Testing

```sh
cargo test --workspace
```

- **Unit + integration** - parser, renderer, barcode encoders
  (decode-verified: QR is re-decoded with `rqrr`), fonts, codepages,
  geometry, memory, and server suites (`zpl/tests/*`,
  `zpl-server/tests/*`).
- **Protocol suite** - `zpl-server/tests/proto.rs`, `tcp.rs`,
  `queries.rs`, `hs.rs` drive the real TCP session and HTTP layers over
  a local socket and assert byte-exact host-status frames.
- **Fuzz properties** - `zpl/tests/fuzz.rs` runs proptest invariants
  over adversarial byte strings up to the 64 KiB ceiling: the parse
  path never panics on any input, the IR serde round-trip holds, and
  the renderer never panics and stays inside its 32000^2 dot canvas.
  Panics found by fuzzing are pinned into the regression corpus and
  replayed on every run.
- **Exhaustive guide checks** - `parse.rs`, `render.rs`, `barcode.rs`
  pin the guide's tables and byte layouts so a regression against a
  documented value fails loudly.

## Parity runbook

> Repository policy: **nothing reference-derived is ever committed.**
> This section documents a *local* cross-check workflow only - no
> external renderer's images, outputs, corpora, or metrics ever enter
> this repository, and `parity/` (gitignored) is scratch space.

Rendering is verified against an external reference rasterizer of your
choosing - any ZPL-to-image engine you trust: a commercial rasterizer,
an open-source one, or a printer driver that can echo its raster. The
reference's identity lives only in the gitignored `parity/fetch.sh`;
the workflow below is engine-agnostic.

1. **Corpus** - `parity/corpus/*.zpl` holds *our* labels: text, `^FB`
   blocks, 1D and 2D barcodes, graphics, codepages, font probes, and a
   mixed viewer-style sample. The corpus is ours - never the reference
   engine's sample data.
2. **Fetch once, cache by sha1** - `parity/fetch.sh` renders every
   corpus case on the reference engine at 203 dpi into
   `parity/ref/<sha1(zpl)>.png`. Each case is fetched exactly once;
   all iteration happens against the cache (the service budget is
   shared - re-fetching is a last resort).
3. **Render locally** - `parity/local.sh` POSTs the same corpus to the
   server's `POST /api/render?dpi=203` endpoint and caches the results
   under the same sha1 keys in `parity/local/`.
4. **Diff** - `parity/diff.py` compares every case that has both PNGs
   and prints the absolute-error pixel count, the % of differing
   pixels, and the difference bounding box, plus a red-on-white mask
   in `parity/diff/`.
5. **Iterate offline** - change the engine, re-run only `local.sh`
   (cached sha1 keys mean only changed behavior re-renders), diff
   again. No reference round-trip inside the loop.

Measured corrections (an advance table, a module rule, a mode
selection) are facts about the reference's semantics: they land in the
engine as documented behavior with tests, never as reference-derived
"golden data".

### Current standing (203 dpi, cached reference rasters)

| Case | AE | % | Residual |
| --- | ---: | ---: | --- |
| 01 text-basic | 1477 | 0.59% | ink-shape floor |
| 02 text-rotations | 2369 | 0.95% | ink-shape floor |
| 03 text-ft | 2097 | 0.84% | ink-shape floor |
| 04 text-ft-rotated | 2768 | 1.11% | ink-shape floor |
| 05 text-cp850 | 5306 | 2.13% | ink-shape floor |
| 06 text-utf8 | 1494 | 0.60% | ink-shape floor |
| 07 text-fb | 10780 | 4.32% | `^FB` block space placement |
| 08 bc128 | 631 | 0.25% | interpret-line ink |
| 09 bc-uccd | 2867 | 1.15% | interpret-line ink |
| 10 bc39 | 0 | 0 | byte-exact |
| 11 qr | 0 | 0 | byte-exact |
| 12 dm | 0 | 0 | byte-exact |
| 13 gb | 0 | 0 | byte-exact |
| 14 gfa | 672 | 0.27% | graphic decode corner |
| 15 serial | 404 | 0.16% | ink-shape floor |
| 16 kitchen-sink | 8313 | 2.50% | mixed (`^FB` + ink floor) |
| 17 font0-sizes | 4982 | 1.50% | ink-shape floor |
| 18 fontA-sizes | 12577 | 3.78% | ink-shape floor |
| 20-29 font probes | 427-11069 | 0.23-3.17% | ink-shape floor |
| 21 viewer-sample | 65513 | 6.46% | ink-shape floor |
| 30 msi-mod11 | 727 | 0.44% | interpret-line ink |
| 31 planet | 562 | 0.34% | interpret-line ink |

Residual classes:

- **Ink-shape floor.** Cells, advances, baselines, symbol grids and
  module geometry are calibrated to the reference; what remains on
  text and font cases is glyph-rasterization drift between the bundled
  DejaVu-family faces and the printer's fonts - largest on big cells,
  absent where no text is involved (Code 39, QR, DataMatrix and boxes
  are byte-exact).
- **Interpret-line ink.** Barcodes that print their human-readable
  line (font 0 at 15% of the bar height, centered under the symbol)
  carry that same glyph-ink floor on the line; the bars and 2D modules
  themselves are exact.
- **Two no-public-spec areas.** `^FB` justified-line space placement:
  the guide specifies line wrapping but not the reference's
  position-dependent space widths, so those are pinned empirically
  from the reference rasters. Downloaded fonts (`^A@` with `~DY`):
  the reference engine does not process downloads at all - the guide's
  model is implemented and unit-tested, with hardware validation
  pending. Neither can be oracle-pinned like the rest.

Nothing produced by the reference engine - images, dumps, metrics,
notes - is committed. `parity/` is gitignored scratch, and the
committed tests assert our documented values independently of any
external engine.
