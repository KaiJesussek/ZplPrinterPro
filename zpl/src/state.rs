//! Parser state: printer mode, memory stub, and the format state machine.

use std::collections::BTreeMap;

use crate::diag::Diagnostics;
use crate::ir::{BarcodeField, Field, Label, OriginMode, Rotation, TextBlock, TextField};

/// Placeholder for stored images/fonts/formats memory; the real model lands
/// in Task 7.
// TODO(T7): replace with BTreeMap-backed Memory (images/fonts/formats).
#[derive(Default)]
pub struct Memory {}

/// Printer-wide configuration (label geometry, orientation, shifts, text
/// defaults).
///
/// Geometry values start at their defaults until the ^PW/^LL/^LH/... handlers
/// land in Task 8; all-zero defaults mean "unset" for now. The text defaults
/// (^CF font, ^FW rotation, ^CI codepage, ^FC clock indicators) are live from
/// Task 5 onward; their power-up values follow the guide (footnotes in the
/// field docs).
#[derive(Clone, Debug)]
pub struct PrinterMode {
    pub pw_dots: u32,
    pub ll_dots: u32,
    pub lh_x: u32,
    pub lh_y: u32,
    pub orientation: u8,
    pub label_top: u32,
    pub left_shift: u32,
    pub mirror: bool,
    /// `^CF` default font as `[canonical, raw]` (power-up: A).
    pub default_font: [u8; 2],
    /// `^CF` default character height in dots (power-up: 9).
    pub default_font_h: u32,
    /// `^CF` default character width in dots (power-up: 5).
    pub default_font_w: u32,
    /// `^FW` default field rotation (power-up: N).
    pub fw_rotation: Rotation,
    /// `^CI` international character set (power-up: 0, U.S.A. 1).
    pub code_page: u8,
    /// `^CW` font assignments: character -> stored font name.
    pub font_assignments: BTreeMap<u8, String>,
    /// `^FC` Real-Time Clock indicator characters (primary power-up: `%`).
    pub clock_indicators: [u8; 3],
}

impl Default for PrinterMode {
    fn default() -> Self {
        PrinterMode {
            pw_dots: 0,
            ll_dots: 0,
            lh_x: 0,
            lh_y: 0,
            orientation: 0,
            label_top: 0,
            left_shift: 0,
            mirror: false,
            default_font: [b'A', b' '],
            default_font_h: 9,
            default_font_w: 5,
            fw_rotation: Rotation::R0,
            code_page: 0,
            font_assignments: BTreeMap::new(),
            clock_indicators: [b'%', 0, 0],
        }
    }
}

/// An in-progress field, accumulated by the ^FO/^FT/^FD/^A handlers (Task 5).
#[derive(Debug, Clone)]
pub struct FieldDraft {
    pub x: u32,
    pub y: u32,
    pub origin: OriginMode,
    pub rotation: Rotation,
    pub reverse: bool,
    pub font: [u8; 2],
    /// `^A` character height in dots; 0 = standard font matrix.
    pub font_height: u32,
    /// `^A` character width in dots; 0 = standard font matrix.
    pub font_width: u32,
    pub data: Vec<u8>,
    pub block: Option<TextBlock>,
    pub symbology: Option<u8>,
    /// `^FH` hex indicator while escapes are enabled for this field.
    pub hex: Option<u8>,
    /// `^FV` variable field: re-evaluated per copy.
    pub re_eval: bool,
    /// Raw command bytes of the field, for stored-format re-emission later.
    pub raw: Vec<u8>,
}

/// Where the parser is in the input stream.
#[derive(Debug, Default)]
pub enum FormatState {
    /// Not inside a format.
    #[default]
    Outside,
    /// Between ^XA and ^XZ; `fields` holds the drafts closed by ^FS.
    InFormat { fields: Vec<FieldDraft> },
    /// A field is open (between its origin/^FD and its ^FS). `fields` carries
    /// the drafts already closed in this format.
    Field { draft: FieldDraft, fields: Vec<FieldDraft> },
    /// ^DF stored-format download: capture raw bytes until ^FS.
    DefFormat { name: String, buf: Vec<u8> },
}

impl FormatState {
    /// Whether a print format is open and not yet closed by ^XZ.
    pub fn is_partial(&self) -> bool {
        matches!(self, Self::InFormat { .. } | Self::Field { .. })
    }

    /// ^XA: (re)start a format, discarding any partial one.
    pub(crate) fn start_format(&mut self) {
        *self = Self::InFormat {
            fields: Vec::new(),
        };
    }

    /// ^XZ: finalize the open format into a `Label`, or `None` if no format
    /// is open (stray ^XZ outside a format is ignored).
    pub(crate) fn end_format(&mut self, mode: &PrinterMode) -> Option<Label> {
        let drafts = match core::mem::take(self) {
            Self::InFormat { fields } => fields,
            Self::Field { draft, mut fields } => {
                // An open field at ^XZ is finalized with the rest.
                fields.push(draft);
                fields
            }
            _ => return None,
        };
        let fields = drafts.into_iter().map(FieldDraft::into_field).collect();
        Some(Label {
            width_dots: mode.pw_dots,
            height_dots: mode.ll_dots,
            copies: 1,
            fields,
        })
    }

    /// ^FS: close an open field into the format's draft list, or finish a
    /// ^DF download. No-op anywhere else.
    pub(crate) fn end_field(&mut self) {
        let next = match core::mem::take(self) {
            Self::Field { draft, mut fields } => {
                fields.push(draft);
                Self::InFormat { fields }
            }
            // ponytail: captured bytes are dropped until Task 7's Memory
            // model gives them a home (formats map).
            Self::DefFormat { .. } => Self::Outside,
            other => other,
        };
        *self = next;
    }

    /// ^DF (params = stored-format name): enter download capture.
    pub(crate) fn start_download(&mut self, name: &[u8]) {
        *self = Self::DefFormat {
            name: String::from_utf8_lossy(name).into_owned(),
            buf: Vec::new(),
        };
    }

    /// Append raw bytes to a ^DF download buffer; no-op outside DefFormat.
    pub(crate) fn capture(&mut self, bytes: &[u8]) {
        // ponytail: unbounded buffer; cap once the server (Task 20) sizes ^DF.
        if let Self::DefFormat { buf, .. } = self {
            buf.extend_from_slice(bytes);
        }
    }
}

impl FieldDraft {
    /// A fresh field at the current printer defaults (^CF font, ^FW rotation).
    pub(crate) fn open(mode: &PrinterMode) -> FieldDraft {
        FieldDraft {
            x: 0,
            y: 0,
            origin: OriginMode::Block,
            rotation: mode.fw_rotation,
            reverse: false,
            font: mode.default_font,
            font_height: mode.default_font_h,
            font_width: mode.default_font_w,
            data: Vec::new(),
            block: None,
            symbology: None,
            hex: None,
            re_eval: false,
            raw: Vec::new(),
        }
    }

    /// Convert a closed draft into its IR field.
    ///
    /// Text fields carry all their geometry on the draft. Barcode geometry
    /// (module/height/line) has no draft counterparts yet, so it falls back
    /// to the ^BY defaults until Task 9 wires ^BY onto the draft.
    fn into_field(self) -> Field {
        // Latin-1 until Task 12 applies the ^CI code page; bytes were
        // accumulated verbatim with ^FH escapes already expanded.
        let data: String = self.data.iter().map(|&b| b as char).collect();
        match self.symbology {
            Some(symbology) => Field::Barcode(BarcodeField {
                x: self.x,
                y: self.y,
                symbology,
                data,
                // ponytail: ^BY defaults (module 2, height 10, line 1) until Task 9.
                module: 2,
                height: 10,
                line: 1,
                rotation: self.rotation,
            }),
            None => Field::Text(TextField {
                x: self.x,
                y: self.y,
                origin: self.origin,
                rotation: self.rotation,
                font: self.font,
                font_height: self.font_height,
                font_width: self.font_width,
                reverse: self.reverse,
                block: self.block,
                data,
                re_eval: self.re_eval,
            }),
        }
    }
}

/// Per-format parsing context handed to command handlers.
pub struct ParseContext<'a> {
    pub state: FormatState,
    pub diags: &'a mut Diagnostics,
    pub memory: &'a mut Memory,
    pub mode: PrinterMode,
    /// The most recently completed label (^XZ), for the protocol layer.
    pub pending_label: Option<Label>,
}

impl<'a> ParseContext<'a> {
    pub fn new(diags: &'a mut Diagnostics, memory: &'a mut Memory) -> Self {
        ParseContext {
            state: FormatState::default(),
            diags,
            memory,
            mode: PrinterMode::default(),
            pending_label: None,
        }
    }
}
