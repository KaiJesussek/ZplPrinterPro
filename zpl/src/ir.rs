//! Document IR: the parsed representation of a ZPL II format.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A complete format: everything between `^XA` and `^XZ`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Label {
    pub width_dots: u32,
    pub height_dots: u32,
    pub copies: u32,
    pub fields: Vec<Field>,
    /// `^FN#` numbered fields of this format: number -> decoded field data
    /// (guide p.200). The server answers `^HV#` from the last completed
    /// label's map (Task 20). `#[serde(default)]` keeps pre-T20 archive
    /// rows loadable.
    #[serde(default)]
    pub numbered: BTreeMap<u32, String>,
}

/// One drawn element of a label.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Field {
    Text(TextField),
    Barcode(BarcodeField),
    Box(BoxField),
    Graphic(GraphicField),
    Circle(CircleField),
    Diagonal(DiagonalField),
    Ellipse(EllipseField),
}

/// Field rotation in 90deg steps.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

/// Origin reference of a field: `^FO` is Block, `^FT` is Dot.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum OriginMode {
    Block,
    Dot,
}

/// A text field (`^FO`/`^FT` origin + `^FD` data).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TextField {
    pub x: u32,
    pub y: u32,
    pub origin: OriginMode,
    pub rotation: Rotation,
    pub font: [u8; 2],
    /// `^A` character height in dots; 0 = standard font matrix (guide ^A).
    pub font_height: u32,
    /// `^A` character width in dots; 0 = standard font matrix.
    pub font_width: u32,
    pub reverse: bool,
    pub block: Option<TextBlock>,
    pub data: String,
    /// `^A@`: the stored (downloaded) font this field prints with, or `None`
    /// for the built-in `^CF` default font. Task 11 maps it to
    /// `FontSpec::Downloaded`.
    pub font_name: Option<String>,
    /// `^GS`: the field prints a built-in graphic symbol ((C) (R) (TM) ... picked by
    /// the `^FD` character) instead of a text glyph.
    pub symbol: bool,
    /// `^FV` variable field: re-evaluated per copy.
    pub re_eval: bool,
}

/// `^FB` block formatting of a text field.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TextBlock {
    pub width: u32,
    pub max_lines: u32,
    pub line_spacing: u32,
    pub justification: Justification,
    /// True for a `^FB` block (guide p.186 width rule: "If the value is
    /// less than the font width or not specified, the text does not
    /// print" - the renderer suppresses such fields). `^TB` (p.356) has
    /// no such rule, so its blocks set this false.
    #[serde(default)]
    pub fb: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Justification {
    Left,
    Center,
    Right,
}

/// A 1D/2D barcode field (`^B...` + `^FD` data).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BarcodeField {
    pub x: u32,
    pub y: u32,
    /// `^FO` (Block) = the bar box's top-left sits at the field origin for
    /// every rotation. `^FT` (Dot) = the origin is the rotated "baseline"
    /// corner: R0 bottom-left, R90 top-left, R180 top-right, R270
    /// bottom-right (measured on the reference - four barcodes at one
    /// ^FT origin tile the four quadrants around it, a cross; the
    /// renderer subtracts the anchor from the painted rect origin).
    pub origin: OriginMode,
    /// Symbology mnemonic letter, e.g. `b'C'` for `^BC`.
    pub symbology: u8,
    pub data: String,
    /// `^BY w`: module width in dots (guide default 2).
    pub module: u32,
    /// `^BY r`: wide/narrow ratio in tenths (3.0 -> 30; guide default 3.0).
    /// Code 128 is a fixed-ratio symbology, so the encoder ignores it;
    /// Task 15's wide/narrow symbologies (Code 39, I2of5) consume it.
    pub ratio: u32,
    /// `^BY h`/symbology `h`: barcode height in dots (guide default 10).
    pub height: u32,
    /// `^BC f`: print interpretation line below the code, `b'Y'`/`b'N'`
    /// (guide default Y).
    pub line: u8,
    /// `^BC g`: print interpretation line above the code (guide default N).
    pub interpret: bool,
    /// `^BC e`: UCC check digit - adds the mod-10 UCC check digit; the
    /// mod-103 check is always present (guide default N).
    pub ucc: bool,
    /// `^BC m`: mode letter N/U/A/D (guide default N). U = UCC case mode
    /// (19-digit pad/trim), A = automatic subset selection, D = UCC/EAN
    /// mode (FNC1 prefix, parentheses/spaces stripped).
    pub model: u8,
    /// Per-symbology check-digit option: ^B1 e (1 or 2 digits), ^B3 e
    /// (Mod-43 Y/N letter), ^BM e (A/B/C/D mode letter). Meaningless for
    /// the rest (their check digits are inherent or their options have no
    /// bar effect).
    pub check: u8,
    /// `^BK k`: Codabar start character A-D (guide default A).
    pub start: u8,
    /// `^BK l`: Codabar stop character A-D (guide default A).
    pub stop: u8,
    /// `^BQ m`: QR error-correction level letter H/Q/M/L (guide default Q).
    pub qr_ec: u8,
    /// `^BQ e`: QR mask pattern 0..7 (default 0 - the reference's fixed
    /// pick; measured corpus 11/16, the guide's 7 does not match it).
    pub qr_mask: u8,
    /// `^BX s`: DataMatrix quality level (guide default 0; 0/200 encode
    /// as ECC 200, legacy convolution values 50..140 warn at parse).
    pub dm_quality: u32,
    /// `^BX f`: DataMatrix format ID 1..6 (guide default 6; 1..5 validate
    /// the character set, 6 = full 256 ISO 8-bit set).
    pub dm_format: u8,
    /// `^BX a`: DataMatrix aspect ratio 1 square / 2 rectangular
    /// (guide default 1).
    pub dm_aspect: u8,
    /// `^B0 d`: Aztec error control and symbol size (guide default 0:
    /// automatic layers at 23% EC; 01-99 EC%, 101-104 / 201-232 forced
    /// compact/full layers, 300 = Rune).
    pub az_level: u32,
    /// `^B0 e`: Aztec menu symbol indicator Y/N (guide default N).
    pub az_menu: bool,
    /// `^B0 f`: Aztec structured-append symbol count 1..26 (default 1).
    pub az_append: u8,
    /// `^B7 s`: PDF417 security level 0..8 (guide default 0 = error
    /// detection only, 2 EC codewords; levels 1..8 -> 2^(s+1)).
    pub pdf_security: u8,
    /// `^B7 c`: PDF417 data columns 1..30 (0 = automatic, guide default).
    pub pdf_cols: u32,
    /// `^B7 r`: PDF417 rows 3..90 (0 = automatic, guide default).
    pub pdf_rows: u32,
    /// `^B7 t`: PDF417 truncation Y/N (guide default N): right row
    /// indicators and stop reduced to a single-module bar.
    pub pdf_truncate: bool,
    /// `^A`/`^A@` font request for the interpretation line, captured when
    /// the symbology field was parsed - guide ^BC p.94: "The interpretation
    /// line can be printed in any font by placing the font command before
    /// the barcode command". The reference does NOT honor it: the line
    /// renders in built-in font 0 at ~15% of the bar height, ignoring the
    /// field's `^A` font and sizes (measured, corpus 08/21). Same shape as
    /// `TextField`'s; kept for state completeness.
    pub font: [u8; 2],
    /// `^A` character height in dots; 0 = standard font matrix.
    pub font_height: u32,
    /// `^A` character width in dots; 0 = standard font matrix.
    pub font_width: u32,
    /// `^A@`: stored (downloaded) font name, captured for state
    /// completeness - the reference renders the interpretation line in
    /// built-in font 0 regardless (measured, corpus 08/21), so the
    /// renderer does not consult this for barcode fields.
    pub font_name: Option<String>,
    pub rotation: Rotation,
}

/// A box or line (`^GB`); `thickness` 0 = filled box (renderer, Task 10).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoxField {
    pub x: u32,
    pub y: u32,
    /// `^FT` (Dot) semantics for graphics: the rect hangs ABOVE the
    /// origin (its bottom edge is the "baseline", so the origin sits at
    /// the rect's bottom) - measured on the reference for ^GB/^GC/^GE.
    pub origin: OriginMode,
    pub width: u32,
    pub height: u32,
    pub thickness: u32,
    /// `b'B'` black / `b'W'` white (the guide's `c` parameter letters).
    pub color: u8,
    /// `^FR` reverse print: the field's ink is XORed into the canvas
    /// (white over existing ink, black over blank paper) - the reference
    /// logo's white frame over the black box (measured, case 21).
    pub reverse: bool,
}

/// A circle (`^GC`); thickness extends inward from the outline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CircleField {
    pub x: u32,
    pub y: u32,
    /// `^FT` (Dot): the circle's bbox hangs above the origin (see
    /// [`BoxField::origin`]).
    pub origin: OriginMode,
    pub diameter: u32,
    pub thickness: u32,
    pub color: u8,
    pub reverse: bool,
}

/// A diagonal line (`^GD`) joining opposite corners of its bounding box.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DiagonalField {
    pub x: u32,
    pub y: u32,
    /// `^FT` (Dot): the bbox hangs above the origin (see
    /// [`BoxField::origin`]).
    pub origin: OriginMode,
    pub width: u32,
    pub height: u32,
    /// `b'R'` right-leaning (`/`, the guide's default) / `b'L'` left-leaning (`\`).
    pub direction: u8,
    pub thickness: u32,
    pub color: u8,
    pub reverse: bool,
}

/// An ellipse (`^GE`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EllipseField {
    pub x: u32,
    pub y: u32,
    /// `^FT` (Dot): the bbox hangs above the origin (see
    /// [`BoxField::origin`]).
    pub origin: OriginMode,
    pub width: u32,
    pub height: u32,
    pub thickness: u32,
    pub color: u8,
    pub reverse: bool,
}

/// An inline or stored graphics image (`^GF` / `^XG`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GraphicField {
    pub x: u32,
    pub y: u32,
    /// `^FT` (Dot): the image hangs above the origin (see
    /// [`BoxField::origin`]).
    pub origin: OriginMode,
    pub kind: GraphicKind,
    pub data: Vec<u8>,
    pub bytes_per_row: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum GraphicKind {
    Inline,
    Stored(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_round_trips_every_field_variant() {
        let label = Label {
            width_dots: 400,
            height_dots: 200,
            copies: 2,
            numbered: BTreeMap::new(),
            fields: vec![
                Field::Text(TextField {
                    x: 10,
                    y: 20,
                    origin: OriginMode::Dot,
                    rotation: Rotation::R270,
                    font: *b"A0",
                    font_height: 24,
                    font_width: 20,
                    reverse: true,
                    block: Some(TextBlock {
                        width: 150,
                        max_lines: 3,
                        line_spacing: 4,
                        justification: Justification::Right,
                        fb: true,
                    }),
                    data: "Hello, ZPL".into(),
                    font_name: Some("E:FONTFILE.TTF".into()),
                    symbol: false,
                    re_eval: true,
                }),
                Field::Text(TextField {
                    x: 10,
                    y: 60,
                    origin: OriginMode::Block,
                    rotation: Rotation::R0,
                    font: *b"01",
                    font_height: 0,
                    font_width: 0,
                    reverse: false,
                    block: None,
                    data: "no block".into(),
                    font_name: None,
                    symbol: true, // ^GS symbol font
                    re_eval: false,
                }),
                Field::Barcode(BarcodeField {
                    x: 10,
                    y: 100,
                    origin: OriginMode::Dot,
                    symbology: b'C',
                    data: "123456".into(),
                    module: 2,
                    ratio: 30,
                    height: 50,
                    line: b'Y',
                    interpret: true,
                    ucc: true,
                    model: b'D',
                    check: b'N',
                    start: b'A',
                    stop: b'A',
                    qr_ec: b'Q',
                    qr_mask: 0,
                    dm_quality: 0,
                    dm_format: 6,
                    dm_aspect: 1,
                    az_level: 0,
                    az_menu: false,
                    az_append: 1,
                    pdf_security: 0,
                    pdf_cols: 0,
                    pdf_rows: 0,
                    pdf_truncate: false,
                    font: *b"01",
                    font_height: 30,
                    font_width: 20,
                    font_name: Some("E:HRFONT.TTF".into()),
                    rotation: Rotation::R180,
                }),
                Field::Box(BoxField {
                    origin: OriginMode::Block,
                    x: 5,
                    y: 5,
                    width: 390,
                    height: 190,
                    thickness: 3,
                    color: b'B',
                    reverse: false,
                }),
                Field::Circle(CircleField {
                    origin: OriginMode::Block,
                    x: 30,
                    y: 30,
                    diameter: 40,
                    thickness: 2,
                    color: b'B',
                    reverse: false,
                }),
                Field::Diagonal(DiagonalField {
                    origin: OriginMode::Block,
                    x: 60,
                    y: 60,
                    width: 100,
                    height: 50,
                    direction: b'L',
                    thickness: 4,
                    color: b'W',
                    reverse: false,
                }),
                Field::Ellipse(EllipseField {
                    origin: OriginMode::Block,
                    x: 90,
                    y: 90,
                    width: 120,
                    height: 80,
                    thickness: 3,
                    color: b'B',
                    reverse: false,
                }),
                Field::Graphic(GraphicField {
                    origin: OriginMode::Block,
                    x: 100,
                    y: 100,
                    kind: GraphicKind::Inline,
                    data: vec![0b1010_1010, 0b1111_0000],
                    bytes_per_row: 1,
                }),
                Field::Graphic(GraphicField {
                    origin: OriginMode::Block,
                    x: 200,
                    y: 100,
                    kind: GraphicKind::Stored("logo.grf".into()),
                    data: vec![0xFF, 0x00],
                    bytes_per_row: 2,
                }),
            ],
        };

        let json = serde_json::to_vec(&label).expect("serialize label");
        let decoded: Label = serde_json::from_slice(&json).expect("deserialize label");
        assert_eq!(label, decoded);
    }
}
