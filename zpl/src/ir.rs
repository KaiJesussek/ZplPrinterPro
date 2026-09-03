//! Document IR: the parsed representation of a ZPL II format.

use serde::{Deserialize, Serialize};

/// A complete format: everything between `^XA` and `^XZ`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Label {
    pub width_dots: u32,
    pub height_dots: u32,
    pub copies: u32,
    pub fields: Vec<Field>,
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
    /// `^A`/`^A@` font request for the interpretation line, captured when
    /// the symbology field was parsed - guide ^BC p.94: "The interpretation
    /// line can be printed in any font by placing the font command before
    /// the barcode command" (the UCC/EAN section's Figure 12 shows the size
    /// parameters, `^A0N,40,30`, applying too). Same shape as `TextField`'s.
    pub font: [u8; 2],
    /// `^A` character height in dots; 0 = standard font matrix.
    pub font_height: u32,
    /// `^A` character width in dots; 0 = standard font matrix.
    pub font_width: u32,
    /// `^A@`: the stored (downloaded) font the interpretation line prints
    /// with, or `None` for a built-in font (draft `^CF` default).
    pub font_name: Option<String>,
    pub rotation: Rotation,
}

/// A box or line (`^GB`); `thickness` 0 = filled box (renderer, Task 10).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoxField {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub thickness: u32,
    /// `b'B'` black / `b'W'` white (the guide's `c` parameter letters).
    pub color: u8,
}

/// A circle (`^GC`); thickness extends inward from the outline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CircleField {
    pub x: u32,
    pub y: u32,
    pub diameter: u32,
    pub thickness: u32,
    pub color: u8,
}

/// A diagonal line (`^GD`) joining opposite corners of its bounding box.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DiagonalField {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// `b'R'` right-leaning (`/`, the guide's default) / `b'L'` left-leaning (`\`).
    pub direction: u8,
    pub thickness: u32,
    pub color: u8,
}

/// An ellipse (`^GE`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EllipseField {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub thickness: u32,
    pub color: u8,
}

/// An inline or stored graphics image (`^GF` / `^XG`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GraphicField {
    pub x: u32,
    pub y: u32,
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
            fields: vec![
                Field::Text(TextField {
                    x: 10,
                    y: 20,
                    origin: OriginMode::Dot,
                    rotation: Rotation::R270,
                    font: [b'A', b'0'],
                    font_height: 24,
                    font_width: 20,
                    reverse: true,
                    block: Some(TextBlock {
                        width: 150,
                        max_lines: 3,
                        line_spacing: 4,
                        justification: Justification::Right,
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
                    font: [b'0', b'1'],
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
                    font: [b'0', b'1'],
                    font_height: 30,
                    font_width: 20,
                    font_name: Some("E:HRFONT.TTF".into()),
                    rotation: Rotation::R180,
                }),
                Field::Box(BoxField {
                    x: 5,
                    y: 5,
                    width: 390,
                    height: 190,
                    thickness: 3,
                    color: b'B',
                }),
                Field::Circle(CircleField {
                    x: 30,
                    y: 30,
                    diameter: 40,
                    thickness: 2,
                    color: b'B',
                }),
                Field::Diagonal(DiagonalField {
                    x: 60,
                    y: 60,
                    width: 100,
                    height: 50,
                    direction: b'L',
                    thickness: 4,
                    color: b'W',
                }),
                Field::Ellipse(EllipseField {
                    x: 90,
                    y: 90,
                    width: 120,
                    height: 80,
                    thickness: 3,
                    color: b'B',
                }),
                Field::Graphic(GraphicField {
                    x: 100,
                    y: 100,
                    kind: GraphicKind::Inline,
                    data: vec![0b1010_1010, 0b1111_0000],
                    bytes_per_row: 1,
                }),
                Field::Graphic(GraphicField {
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
