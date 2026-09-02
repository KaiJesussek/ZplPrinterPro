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
}

/// Field rotation in 90deg steps.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

/// Origin reference of a field: `^FO` is Block, `^FT` is Dot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
    pub reverse: bool,
    pub block: Option<TextBlock>,
    pub data: String,
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
    pub module: u32,
    pub height: u32,
    pub line: u8,
    pub rotation: Rotation,
}

/// A box or line (`^GB`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BoxField {
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
