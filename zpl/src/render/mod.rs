//! Renderer core: the packed 1bpp framebuffer, PNG output, and the paint
//! loop that rasterizes a parsed `Label` (Task 10).
//!
//! `render` walks `label.fields` in order and paints each drawing field
//! (`^GB` rect, `^GC` circle, `^GD` diagonal, `^GE` ellipse) and graphic
//! blit (`^GF` inline, `^XG`/`^IM`/`^IL` stored recall) into a
//! [`Bitmap`]. Text and barcode fields are painted by Tasks 13/14 and are
//! silently skipped here. The parser already decodes inline `^GF` payloads
//! into packed bytes (`geom::graphic_field`), so the renderer only blits.
//!
//! The Task 6 mode arrives via [`RenderOptions::mode`] (a deliberate
//! addition to the plan's struct: `^LH`/`^LS`/`^LT`/`^PO`/`^PM` are
//! render-time per the T6 ruling, and the label IR deliberately keeps field
//! coordinates local - there is no other channel). `^LH` offsets every
//! field origin, `^LS` shifts the format left, `^LT` down, then `^POI`
//! rotates the whole label 180deg and `^PMY` mirrors it (guide p.319/322).
//!
//! Everything clamps to the bitmap; the renderer never panics and never
//! hangs on hostile field geometry (all loops range over the on-canvas
//! intersection only, and all width/height arithmetic is checkpointed
//! against u32/i64/u128 overflow).

mod draw;

use crate::fonts::FontProvider;
use crate::ir::{Field, GraphicKind, Label};
use crate::memory::Memory;
use crate::state::PrinterMode;
use draw::{blit, draw_circle, draw_diagonal, draw_ellipse, draw_rect};

/// A packed 1bpp framebuffer: row-major, MSB-first, black = 1
/// (bit 7 of the first byte is the leftmost dot of row 0).
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bits: Vec<u8>,
}

impl Bitmap {
    /// A new all-white bitmap. Width/height clamp to at least 1 so every
    /// `Bitmap` is encodable (PNG has no 0-dimension images).
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let row = ((width as usize) + 7) / 8;
        Bitmap {
            width,
            height,
            bits: vec![0; row * height as usize],
        }
    }

    /// Paint one dot; coordinates outside the bitmap are ignored.
    pub fn set(&mut self, x: u32, y: u32, black: bool) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y as usize * ((self.width as usize + 7) / 8) + (x as usize >> 3);
        let mask = 0x80 >> (x & 7);
        if black {
            self.bits[idx] |= mask;
        } else {
            self.bits[idx] &= !mask;
        }
    }

    /// Read one dot; coordinates outside the bitmap read as white.
    pub fn get(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx = y as usize * ((self.width as usize + 7) / 8) + (x as usize >> 3);
        self.bits[idx] & (0x80 >> (x & 7)) != 0
    }

    /// Paint one dot with a field color: `B` sets black, `W` erases
    /// (paints white = 0) - the guide's line-color semantics (the `W`
    /// color is an eraser, used for white boxes over black).
    fn paint(&mut self, x: u32, y: u32, color: u8) {
        match color {
            b'W' | b'w' => self.set(x, y, false),
            _ => self.set(x, y, true),
        }
    }

    /// Encode as a 1-bit grayscale PNG (the plan's output format; the png
    /// crate supports `ColorType::Grayscale` + `BitDepth::One`).
    ///
    /// PNG grayscale samples are 0 = black / 1 = white, so the bits are
    /// inverted on write - writing them verbatim would produce a
    /// photographically inverted label. Rows are byte-packed, which is
    /// exactly the bitmap's row layout. Encodes into memory, which cannot
    /// fail once the bitmap is non-degenerate (guaranteed by [`Bitmap::new`]).
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.width, self.height);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::One);
            let mut writer = enc.write_header().expect("PNG header into memory cannot fail");
            let inverted: Vec<u8> = self.bits.iter().map(|b| !b).collect();
            writer
                .write_image_data(&inverted)
                .expect("PNG data into memory cannot fail");
            writer.finish().expect("PNG finish into memory cannot fail");
        }
        out
    }
}

impl core::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Bitmap {{ width: {}, height: {}, bits: {} bytes }}",
            self.width,
            self.height,
            self.bits.len()
        )
    }
}

/// Renderer inputs: resolution, font service, printer storage, and the
/// Task 6 printer mode (`^LH`/`^LS`/`^LT`/`^PO`/`^PM`/`^LL` are consumed
/// at render time; the label IR keeps local field coordinates).
pub struct RenderOptions<'a> {
    /// Printer resolution in dots per inch; scales text/barcode metrics
    /// (Tasks 11/13/14). Geometry is already in dots, so T10 ignores it.
    pub dpi: u32,
    /// Font lookup for text fields (placeholder trait until Task 11).
    pub fonts: &'a dyn FontProvider,
    /// Printer storage (stored fonts/fonts/formats; `^XG` recalls here).
    pub memory: &'a Memory,
    /// The printer geometry state the label was parsed under. The plan's
    /// `RenderOptions` sketch lacked this channel; the T6 ruling
    /// (render-time `^LH`/`^PO`/`^PM`) requires it, so it was added.
    pub mode: &'a PrinterMode,
}

/// The product of a render: the raster plus non-fatal warnings (a missing
/// stored image is a warning, not an error - the label still renders).
#[derive(Debug)]
pub struct RenderOutcome {
    pub bitmap: Bitmap,
    pub warnings: Vec<String>,
}

/// Canvas dimension ceiling, in dots. Real ZPL geometry never exceeds it
/// (the guide's `^LL`/`^LH`/`^FO` ranges all top out at 32000, and the
/// memory row cap is 32000 dots - `memory::MAX_ROW_BYTES`), but the parser
/// accepts `^PW` values up to u32::MAX ("the printer clamps"; T6 stores as
/// given) and hand-built IR can carry anything. The renderer clamps the
/// canvas just like hardware print areas do, so a hostile width can never
/// turn into a multi-gigabyte allocation.
const MAX_LABEL_DOTS: u32 = 32_000;

/// Structural render failures. The parser already validates everything it
/// feeds the IR (malformed `^GF` payloads warn and skip at parse time), so
/// these are only reachable from hand-built IR.
#[derive(Debug)]
pub enum RenderError {
    /// An inline graphic field whose byte geometry is inconsistent:
    /// `bytes_per_row` is 0 or the data length is not a multiple of it.
    InvalidGraphic(String),
}

/// Rasterize a label at the options' geometry. Never panics and never
/// hangs: every primitive clamps to the bitmap and loops only over the
/// on-canvas intersection.
pub fn render(label: &Label, opts: &RenderOptions) -> Result<RenderOutcome, RenderError> {
    let w = label.width_dots.clamp(1, MAX_LABEL_DOTS) as i64;
    let h = if label.height_dots == 0 {
        auto_height(label, opts).min(MAX_LABEL_DOTS) as i64
    } else {
        label.height_dots.clamp(1, MAX_LABEL_DOTS) as i64
    };
    let mut bmp = Bitmap::new(w as u32, h as u32);
    let mut warnings = Vec::new();

    for field in &label.fields {
        match field {
            // Tasks 13 (text) / 14 (barcodes): nothing painted yet.
            Field::Text(_) | Field::Barcode(_) => {}
            Field::Box(b) => {
                let (fx, fy) = place(opts.mode, w, h, b.x, b.y, b.width, b.height);
                draw_rect(&mut bmp, fx, fy, b.width, b.height, b.thickness, b.color);
            }
            Field::Circle(c) => {
                let (fx, fy) = place(opts.mode, w, h, c.x, c.y, c.diameter, c.diameter);
                draw_circle(&mut bmp, fx, fy, c.diameter, c.thickness, c.color);
            }
            Field::Diagonal(d) => {
                let (fx, fy) = place(opts.mode, w, h, d.x, d.y, d.width, d.height);
                draw_diagonal(&mut bmp, fx, fy, d.width, d.height, d.direction, d.thickness, d.color);
            }
            Field::Ellipse(e) => {
                let (fx, fy) = place(opts.mode, w, h, e.x, e.y, e.width, e.height);
                draw_ellipse(&mut bmp, fx, fy, e.width, e.height, e.thickness, e.color);
            }
            Field::Graphic(g) => match &g.kind {
                GraphicKind::Inline => {
                    if g.bytes_per_row == 0 || g.data.len() % g.bytes_per_row as usize != 0 {
                        return Err(RenderError::InvalidGraphic(format!(
                            "inline graphic at ({}, {}): {} bytes is not a multiple of {} bytes per row",
                            g.x,
                            g.y,
                            g.data.len(),
                            g.bytes_per_row
                        )));
                    }
                    // u64 math: `bytes_per_row * 8` overflows u32 for
                    // bytes_per_row >= 2^29 (hand-built IR can carry that;
                    // the parser caps ^GF at 99999). Width/height are
                    // clamped to u32 and used only for extents.
                    let iw = (g.bytes_per_row as u64 * 8).min(u32::MAX as u64) as u32;
                    let ih = (g.data.len() as u64 / g.bytes_per_row as u64).min(u32::MAX as u64) as u32;
                    let (fx, fy) = place(opts.mode, w, h, g.x, g.y, iw, ih);
                    blit(&mut bmp, iw, ih, &g.data, fx, fy);
                }
                GraphicKind::Stored(name) => match opts.memory.images.get(name) {
                    Some(img) => {
                        let (fx, fy) = place(opts.mode, w, h, g.x, g.y, img.width, img.height);
                        blit(&mut bmp, img.width, img.height, &img.data, fx, fy);
                    }
                    None => warnings.push(format!(
                        "missing stored image '{name}' (^XG/^IM/^IL recall); field skipped"
                    )),
                },
            },
        }
    }

    Ok(RenderOutcome { bitmap: bmp, warnings })
}

/// Auto label height (`^LL` omitted, `Label.height_dots == 0`, guide
/// p.294/307 continuous media): the label extends to contain the printed
/// content - the maximum field bottom (`y + height` in label space) plus
/// the `^LH` home offset, minimum 1 dot. `^LS`/`^LT` are fine-tuning shifts
/// of the format on fixed media and do not grow the label's content extent.
/// The result is capped by [`MAX_LABEL_DOTS`] (see its comment).
fn auto_height(label: &Label, opts: &RenderOptions) -> u32 {
    let mut max_bottom: u64 = 0;
    for field in &label.fields {
        let (y, fh) = footprint(field, opts);
        if let Some((y, fh)) = y.zip(fh) {
            max_bottom = max_bottom.max(y as u64 + fh as u64 + opts.mode.lh_y as u64);
        }
    }
    max_bottom.max(1).min(MAX_LABEL_DOTS as u64) as u32
}

/// A field's (y, height) in label space - the extent it contributes to the
/// auto height. Text/barcode fields are not painted yet (Tasks 13/14) and
/// contribute nothing; a missing stored image contributes nothing either.
fn footprint(field: &Field, opts: &RenderOptions) -> (Option<u32>, Option<u32>) {
    match field {
        Field::Box(b) => (Some(b.y), Some(b.height)),
        Field::Circle(c) => (Some(c.y), Some(c.diameter)),
        Field::Diagonal(d) => (Some(d.y), Some(d.height)),
        Field::Ellipse(e) => (Some(e.y), Some(e.height)),
        Field::Graphic(g) => match &g.kind {
            GraphicKind::Inline
                if g.bytes_per_row > 0 && g.data.len() % g.bytes_per_row as usize == 0 =>
            {
                let ih = (g.data.len() as u64 / g.bytes_per_row as u64).min(u32::MAX as u64) as u32;
                (Some(g.y), Some(ih))
            }
            GraphicKind::Stored(name) => match opts.memory.images.get(name) {
                Some(img) => (Some(g.y), Some(img.height)),
                None => (None, None),
            },
            _ => (None, None),
        },
        Field::Text(_) | Field::Barcode(_) => (None, None),
    }
}

/// Map a field's label-space origin through the Task 6 mode: `^LH` adds the
/// home offset, `^LS` shifts left, `^LT` shifts down, `^POI` rotates the
/// whole label 180deg and `^PMY` mirrors it horizontally (guide p.296-297,
/// 319, 322). `fw`/`fh` are the field's footprint, needed because rotation
/// and mirror reposition the field's far corner, not its origin. The
/// transforms are applied to the final label image; 180deg + horizontal
/// mirror compose into a vertical flip, so the two flips commute.
fn place(
    mode: &PrinterMode,
    w: i64,
    h: i64,
    x: u32,
    y: u32,
    fw: u32,
    fh: u32,
) -> (i64, i64) {
    let mut fx = x as i64 + mode.lh_x as i64 - mode.left_shift as i64;
    let mut fy = y as i64 + mode.lh_y as i64 + mode.label_top as i64;
    if mode.orientation == 1 {
        fx = w - fx - fw as i64;
        fy = h - fy - fh as i64;
    }
    if mode.mirror {
        fx = w - fx - fw as i64;
    }
    (fx, fy)
}
