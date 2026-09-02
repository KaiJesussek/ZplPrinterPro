//! Geometric drawing primitives (Task 10). Every function clamps to the
//! bitmap, never panics, and costs only as much work as the on-canvas
//! intersection - hostile field geometry (a circle of `u32::MAX` dots far
//! off the label) loops over nothing more than the visible rows/columns.
//!
//! Semantics come from the ZPL II guide (11EN): `^GB`/`^GC`/`^GD`/`^GE`
//! line thickness extends **inward from the outline** ("Thickness extends
//! inward from the outline" - ^GC, p.212; the plan generalizes the same
//! rule to ^GB/^GE and the diagonal), so the outer edge is fixed at the
//! declared box boundary and the thickness eats into the interior. `t = 0`
//! on a box means a filled box (the plan's renderer contract; the 11EN
//! table lists t in 1..32000 but the parser records 0 verbatim and the
//! renderer owns the fill rule). A border whose thickness meets or exceeds
//! a box dimension covers the box entirely (the bands merge).
//!
//! Colors: `B` paints black, `W` paints white - i.e. erases (paints 0).
//! The guide's line-color parameter is B/W; on a thermal printer white
//! marks are unburned dots, so W over black clears it (used for white
//! boxes on black backgrounds).

use crate::render::Bitmap;

/// Paint one horizontal span, clamped to the bitmap's bounds.
fn hspan(bm: &mut Bitmap, x: i64, y: i64, len: i64, color: u8) {
    if y < 0 || y >= bm.height as i64 {
        return;
    }
    let x0 = x.max(0);
    let x1 = (x + len).min(bm.width as i64);
    for cx in x0..x1 {
        bm.paint(cx as u32, y as u32, color);
    }
}

/// Paint one vertical span, clamped.
fn vspan(bm: &mut Bitmap, x: i64, y: i64, len: i64, color: u8) {
    if x < 0 || x >= bm.width as i64 {
        return;
    }
    let y0 = y.max(0);
    let y1 = (y + len).min(bm.height as i64);
    for cy in y0..y1 {
        bm.paint(x as u32, cy as u32, color);
    }
}

/// A horizontal line `len` dots long and `thickness` rows tall. The top
/// edge is fixed at `y` and the thickness grows downward - the "inward"
/// side for a box's top border (even thicknesses grow one dot inward, per
/// the plan's Zebra rule).
pub(crate) fn draw_thick_line(bm: &mut Bitmap, x: i64, y: i64, len: i64, thickness: i64, color: u8) {
    let y0 = y.max(0);
    let y1 = (y + thickness).min(bm.height as i64);
    for ry in y0..y1 {
        hspan(bm, x, ry, len, color);
    }
}

/// A vertical line `len` dots tall and `thickness` cols wide; the left edge
/// is fixed at `x` and the thickness grows rightward (inward for a box's
/// left border).
pub(crate) fn draw_thick_vline(bm: &mut Bitmap, x: i64, y: i64, len: i64, thickness: i64, color: u8) {
    let x0 = x.max(0);
    let x1 = (x + thickness).min(bm.width as i64);
    for cx in x0..x1 {
        vspan(bm, cx, y, len, color);
    }
}

/// A solid rectangle, clamped.
fn fill_rect(bm: &mut Bitmap, x: i64, y: i64, w: i64, h: i64, color: u8) {
    let y0 = y.max(0);
    let y1 = (y + h).min(bm.height as i64);
    for ry in y0..y1 {
        hspan(bm, x, ry, w, color);
    }
}

/// A box or line (`^GB`): `thickness` 0 = filled box; otherwise a border
/// `thickness` dots wide growing inward from the outer edge. Top and bottom
/// bands cover the full width, left/right bands the remaining middle rows;
/// a thickness meeting or exceeding the smaller box dimension makes the
/// bands merge into a solid block (this is how `^GB` draws thick lines:
/// the guide's w/h defaults to t, and an explicit small dimension with a
/// large t fills the box).
pub(crate) fn draw_rect(
    bm: &mut Bitmap,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    thickness: u32,
    color: u8,
) {
    let (w64, h64) = (w as i64, h as i64);
    if w64 == 0 || h64 == 0 {
        return;
    }
    if thickness == 0 {
        fill_rect(bm, x, y, w64, h64, color);
        return;
    }
    let t = (thickness as i64).min(w64.min(h64));
    draw_thick_line(bm, x, y, w64, t, color); // top
    draw_thick_line(bm, x, y + h64 - t, w64, t, color); // bottom
    if h64 > 2 * t {
        let mid = h64 - 2 * t;
        draw_thick_vline(bm, x, y + t, mid, t, color); // left
        if w64 > t {
            draw_thick_vline(bm, x + w64 - t, y + t, mid, t, color); // right
        }
    }
}

/// A circle (`^GC`): diameter `d` bounding box at (x, y); thickness grows
/// inward from the outline, so the ring is the outer disc minus the inner
/// disc of diameter `d - 2t`. A thickness reaching half the diameter fills
/// the disc. Midpoint-style raster: a pixel at box offset (c, r) is painted
/// when `(2c-(d-1))^2 + (2r-(d-1))^2 <= d^2` - the exact circle of diameter
/// `d` centered on the box (integer math, i128 so hostile dimensions can
/// never overflow).
pub(crate) fn draw_circle(
    bm: &mut Bitmap,
    x: i64,
    y: i64,
    d: u32,
    thickness: u32,
    color: u8,
) {
    let d64 = d as i64;
    if d64 <= 0 {
        return;
    }
    let t = thickness as i64;
    let inner = (d64 - 2 * t).max(0);
    let bw = bm.width as i64;
    let bh = bm.height as i64;
    let row0 = 0.max(-y);
    let row1 = d64.min(bh - y);
    let col0 = 0.max(-x);
    let col1 = d64.min(bw - x);
    let d2 = (d64 as i128) * (d64 as i128);
    let inner2 = (inner as i128) * (inner as i128);
    for row in row0..row1 {
        let dy: i128 = (2 * row - (d64 - 1)) as i128;
        let dy2 = dy * dy;
        for col in col0..col1 {
            let dx: i128 = (2 * col - (d64 - 1)) as i128;
            // A pixel is inside the disc of diameter `diam` centered on the
            // box when (2c-(d-1))^2 + (2r-(d-1))^2 <= diam^2. The inner disc
            // has the SAME center, so it reuses (dx, dy) with the inner
            // diameter - shifting the offsets by 2t would re-center it on a
            // different grid point and misplace the hole.
            if dx * dx + dy2 <= d2 {
                if inner == 0 || dx * dx + dy2 > inner2 {
                    bm.paint((x + col) as u32, (y + row) as u32, color);
                }
            }
        }
    }
}

/// An ellipse (`^GE`): box `w x h`; thickness inward; the ring is the outer
/// ellipse minus the inner ellipse (`w - 2t` x `h - 2t`). Pixel test:
/// `h^2*(2c-(w-1))^2 + w^2*(2r-(h-1))^2 <= w^2h^2`. u128 throughout - no overflow
/// for any u32 dimensions.
pub(crate) fn draw_ellipse(
    bm: &mut Bitmap,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    thickness: u32,
    color: u8,
) {
    let (w64, h64) = (w as i64, h as i64);
    if w64 == 0 || h64 == 0 {
        return;
    }
    let t = thickness as i64;
    let iw = (w64 - 2 * t).max(0);
    let ih = (h64 - 2 * t).max(0);
    let bw = bm.width as i64;
    let bh = bm.height as i64;
    let row0 = 0.max(-y);
    let row1 = h64.min(bh - y);
    let col0 = 0.max(-x);
    let col1 = w64.min(bw - x);
    // u128: `ww^2hh^2` reaches (2^3^2-1)^4 ~ 3.40e38, past i128::MAX; u128
    // (max ~ 3.40e38) covers every u32 dimension pair.
    let ww = w64 as u128;
    let hh = h64 as u128;
    let outer = ww * ww * hh * hh;
    let iww = iw as u128;
    let ihh = ih as u128;
    let inner = iww * iww * ihh * ihh;
    for row in row0..row1 {
        let dyq: u128 = (2 * row - (h64 - 1)).abs() as u128;
        for col in col0..col1 {
            let dxn: u128 = (2 * col - (w64 - 1)).abs() as u128;
            // `|dxn| <= w` and `|dyq| <= h`, so each factored square is
            // individually <= outer - compare via subtraction, never by
            // adding two near-maximum products.
            let a = hh * dxn;
            let b = ww * dyq;
            if a * a <= outer && b * b <= outer - a * a {
                // Inner ellipse: same center, so the same normalized
                // offsets with the inner diameters in the denominator.
                let ai = ihh * dxn;
                let bi = iww * dyq;
                if iw == 0 || ih == 0 || ai * ai > inner || bi * bi > inner - ai * ai {
                    bm.paint((x + col) as u32, (y + row) as u32, color);
                }
            }
        }
    }
}

/// A diagonal (`^GD`): joins one pair of opposite corners of its `w x h`
/// box - `R` (or `/`) right-leaning: top-left to bottom-right; `L` (or
/// `\`) left-leaning: top-right to bottom-left (guide p.213, default R).
/// The line is a Bresenham-style interpolation along the box's longer axis,
/// thickened to `thickness` dots: odd thicknesses center on the line, even
/// ones grow one extra dot toward the larger coordinate (the plan's "even
/// thickness grows inward" reading; the guide gives no turnaround rule for
/// ^GD, so the +x/+y side is used). Degenerate 1-pixel-wide boxes draw a
/// thick horizontal/vertical line.
pub(crate) fn draw_diagonal(
    bm: &mut Bitmap,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    direction: u8,
    thickness: u32,
    color: u8,
) {
    let (w64, h64) = (w as i64, h as i64);
    if w64 == 0 || h64 == 0 {
        return;
    }
    let t = (thickness as i64).max(1);
    if h64 == 1 {
        draw_thick_line(bm, x, y, w64, t, color);
        return;
    }
    if w64 == 1 {
        draw_thick_vline(bm, x, y, h64, t, color);
        return;
    }
    let bw = bm.width as i64;
    let bh = bm.height as i64;
    let left = direction == b'L';
    let run_start = (t - 1) / 2; // odd centered; even +1 toward +x/+y
    if w64 >= h64 {
        // Iterate rows (the longer axis); the diagonal's column at row r
        // of the box (right-leaning) is x + r*(w-1)/(h-1) (rounded).
        let row0 = 0.max(-y);
        let row1 = h64.min(bh - y);
        let slope_num = (w64 - 1) as i128;
        let slope_den = 2 * (h64 - 1) as i128;
        for row in row0..row1 {
            let r = row as i128;
            let along = ((2 * r * slope_num + (h64 - 1) as i128) / slope_den) as i64;
            let cx = if left { x + (w64 - 1) - along } else { x + along };
            hspan(bm, cx - run_start, y + row, t, color);
        }
    } else {
        let col0 = 0.max(-x);
        let col1 = w64.min(bw - x);
        let slope_num = (h64 - 1) as i128;
        let slope_den = 2 * (w64 - 1) as i128;
        for col in col0..col1 {
            let c = col as i128;
            let along = ((2 * c * slope_num + (w64 - 1) as i128) / slope_den) as i64;
            let cy = if left { y + (h64 - 1) - along } else { y + along };
            vspan(bm, x + col, cy - run_start, t, color);
        }
    }
}

/// Copy a packed 1bpp source (row-major, MSB-first, `src_w` dots wide) into
/// the bitmap at (x, y), both colors copied verbatim (white source dots
/// erase - the guide's image-recall model writes pixels, it does not OR
/// them). Clipped at the bitmap edges; `src` bytes are read defensively, so
/// a truncated payload paints as white and never panics.
pub(crate) fn blit(dst: &mut Bitmap, src_w: u32, src_h: u32, src: &[u8], x: i64, y: i64) {
    if src_w == 0 || src_h == 0 {
        return;
    }
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    if x0 >= dst.width || y0 >= dst.height {
        return;
    }
    let vw = (dst.width - x0).min(src_w);
    let vh = (dst.height - y0).min(src_h);
    let src_stride = ((src_w as usize) + 7) / 8;
    for sy in 0..vh {
        for sx in 0..vw {
            let b = (sy as usize) * src_stride + (sx as usize >> 3);
            if let Some(&byte) = src.get(b) {
                if byte & (0x80 >> (sx & 7)) != 0 {
                    dst.set(x0 + sx, y0 + sy, true);
                } else {
                    dst.set(x0 + sx, y0 + sy, false);
                }
            } else {
                // Truncated row: unread dots count as white, not as nothing.
                dst.set(x0 + sx, y0 + sy, false);
            }
        }
    }
}
