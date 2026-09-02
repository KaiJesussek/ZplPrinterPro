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
                    reverse: true,
                    block: Some(TextBlock {
                        width: 150,
                        max_lines: 3,
                        line_spacing: 4,
                        justification: Justification::Right,
                    }),
                    data: "Hello, ZPL".into(),
                }),
                Field::Text(TextField {
                    x: 10,
                    y: 60,
                    origin: OriginMode::Block,
                    rotation: Rotation::R0,
                    font: [b'0', b'1'],
                    reverse: false,
                    block: None,
                    data: "no block".into(),
                }),
                Field::Barcode(BarcodeField {
                    x: 10,
                    y: 100,
                    symbology: b'C',
                    data: "123456".into(),
                    module: 2,
                    height: 50,
                    line: 0,
                    rotation: Rotation::R180,
                }),
                Field::Box(BoxField {
                    x: 5,
                    y: 5,
                    width: 390,
                    height: 190,
                    thickness: 3,
                    color: 7,
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
