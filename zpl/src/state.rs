//! Parser state: printer mode, storage memory, and the format state
//! machine.

use std::collections::BTreeMap;

use crate::diag::Diagnostics;
use crate::ir::{
    BarcodeField, Field, GraphicField, GraphicKind, Label, OriginMode, Rotation, TextBlock, TextField,
};

pub use crate::memory::{Image, Memory};

/// Printer-wide geometry and text configuration, all defaults per the ZPL II
/// guide's power-up tables (Task 6 verified them against the guide; see the
/// task report for the divergences from the plan's sketches).
///
/// Geometry commands land in `PrinterMode` because the renderer consumes
/// them; non-geometry settings with downstream consumers live in
/// [`PrinterConfig`] on the parse context.
#[derive(Clone, Debug)]
pub struct PrinterMode {
    /// `^PW` print width in dots (power-up: the printer width - 832 dots for
    /// a 203-dpi 4-inch printer, the guide's configuration-label example).
    pub pw_dots: u32,
    /// `^LL` label length in dots; 0 = AUTO - ^LL omitted, the renderer
    /// sizes the label to its printed content (the guide's continuous-media
    /// rule; an explicit ^LL value of 0 is impossible, so 0 is unambiguous).
    pub ll_dots: u32,
    /// `^LH` label home offset (0..32000 each, power-up 0), added to every
    /// field origin at render time.
    pub lh_x: u32,
    pub lh_y: u32,
    /// `^PO` print orientation: 0 = N (normal), 1 = I (inverted 180deg). The
    /// guide knows only these two values.
    pub orientation: u8,
    /// `^LT` label top offset (-120..120, power-up 0): shifts the whole
    /// format up (negative) or down from the label's top edge.
    pub label_top: i32,
    /// `^LS` left-shift (-9999..9999, power-up 0), Z-130 compatibility.
    pub left_shift: i32,
    /// `^PM` mirror image of the whole label (Y), left-right flip.
    pub mirror: bool,
    /// `^PQ` print quantity (default 1); `Label::copies` at ^XZ, and the
    /// server's labels-remaining tracking.
    pub copies: u32,
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
            pw_dots: 832,
            ll_dots: 0, // auto: ^LL omitted
            lh_x: 0,
            lh_y: 0,
            orientation: 0,
            label_top: 0,
            left_shift: 0,
            mirror: false,
            copies: 1,
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

/// Non-geometry printer settings that have real downstream consumers.
///
/// Other accept-only commands (`^PR ^MT ^MM ^MF ^MI ^ML ^MP ^MW ^PA ^PF ^SC
/// ^ZZ ^XB ^XS ^PN`) are parsed and validated by their handlers but not
/// stored: the guide describes media/hardware actions with no effect on the
/// label image, so a bitmap renderer has nowhere to put them (see the Task 6
/// report).
#[derive(Clone, Debug)]
pub struct PrinterConfig {
    /// `~SD`/`^MD` darkness in tenths of the guide's 0-30 scale (8.3 -> 83;
    /// the XiIIIPlus-class 0.1 steps survive this way). `~SD` sets the
    /// absolute value, `^MD` adds its signed offset. Power-up 0.
    pub darkness: i32,
    /// `^MN` media-tracking letter (N/Y/W/M/A/V); 0 = unset (the guide
    /// ignores a missing value). V (continuous, variable length) drives Task
    /// 10's auto label height.
    pub media_tracking: u8,
    /// `^MU` units letter: D dots / I inches / M millimeters (power-up D).
    /// Field-by-field meaning; Task 13 scales field coordinates by it.
    pub units: u8,
    /// `^SZ` ZPL mode: 1 = ZPL, 2 = ZPL II (power-up 2). Mode 1 selects the
    /// legacy ZPL language, which this project deliberately does not
    /// implement (the plan drops ZPL I); the value is recorded anyway.
    pub zpl_mode: u8,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        PrinterConfig {
            darkness: 0,
            media_tracking: 0,
            units: b'D',
            zpl_mode: 2,
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
    /// `^XG`/`^IM`/`^IL`: the field recalls a stored graphic instead of
    /// carrying text/barcode data.
    pub graphic: Option<GraphicKind>,
    /// `^A@`: the selected stored (downloaded) font name, or `None` for the
    /// built-in `^CF` default. Task 11 maps it to `FontSpec::Downloaded`.
    pub font_name: Option<String>,
    /// `^GS`: the field prints a built-in graphic symbol ((C) (R) (TM) ...) from
    /// its `^FD` character instead of a text glyph.
    pub symbol: bool,
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
            copies: mode.copies,
            fields,
        })
    }

    /// ^FS: close an open field into the format's draft list, or finish a
    /// ^DF download - returning the captured bytes so the caller (cmd.rs)
    /// can store them in `Memory.formats`. No-op anywhere else.
    pub(crate) fn end_field(&mut self) -> Option<(String, Vec<u8>)> {
        let next = match core::mem::take(self) {
            Self::Field { draft, mut fields } => {
                fields.push(draft);
                Self::InFormat { fields }
            }
            Self::DefFormat { name, buf } => {
                *self = Self::Outside;
                return Some((name, buf));
            }
            other => other,
        };
        *self = next;
        None
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
            graphic: None,
            font_name: None,
            symbol: false,
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
        match self.graphic {
            // Stored-image recall (^XG/^IM/^IL): the renderer (Task 10)
            // resolves the bytes from Memory.images by name.
            Some(kind) => Field::Graphic(GraphicField {
                x: self.x,
                y: self.y,
                kind,
                data: Vec::new(),
                bytes_per_row: 0,
            }),
            None => match self.symbology {
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
                    font_name: self.font_name,
                    symbol: self.symbol,
                    re_eval: self.re_eval,
                }),
            },
        }
    }
}

/// Per-format parsing context handed to command handlers.
pub struct ParseContext<'a> {
    pub state: FormatState,
    pub diags: &'a mut Diagnostics,
    pub memory: &'a mut Memory,
    pub mode: PrinterMode,
    /// Non-geometry printer settings (^MD/~SD darkness, ^MN tracking, ...).
    pub config: PrinterConfig,
    /// The most recently completed label (^XZ), for the protocol layer.
    pub pending_label: Option<Label>,
    /// Active ^XF stored-format recall depth (recursion cap 4; see
    /// memory.rs `recall_format`).
    pub recall_depth: u32,
}

impl<'a> ParseContext<'a> {
    pub fn new(diags: &'a mut Diagnostics, memory: &'a mut Memory) -> Self {
        ParseContext {
            state: FormatState::default(),
            diags,
            memory,
            mode: PrinterMode::default(),
            config: PrinterConfig::default(),
            pending_label: None,
            recall_depth: 0,
        }
    }
}
