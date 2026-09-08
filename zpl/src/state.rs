//! Parser state: printer mode, storage memory, and the format state
//! machine.

use std::collections::BTreeMap;

use crate::diag::Diagnostics;
use crate::ir::{
    BarcodeField, BoxField, CircleField, DiagonalField, EllipseField, Field, GraphicField,
    GraphicKind, Label, OriginMode, Rotation, TextBlock, TextField,
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
    /// `^PQ` r replicates of each serial number (guide p.324: 0..99,999,999,
    /// default 0). Each distinct serial prints r+1 times, so serialized
    /// fields' counters advance once per r+1 printed copies; the total
    /// quantity to print stays `q` (the guide's example ^PQ50,10,1 prints
    /// "a total of 50 labels with one replicate of each serial number").
    pub replicates: u32,
    /// `^CF` default font as `[canonical, raw]` (power-up: A).
    pub default_font: [u8; 2],
    /// `^CF` default character height in dots (power-up: 9).
    pub default_font_h: u32,
    /// `^CF` default character width in dots (power-up: 5).
    pub default_font_w: u32,
    /// `^FW` default field rotation (power-up: N).
    pub fw_rotation: Rotation,
    /// `^BY w` default barcode module width in dots (guide p.148:
    /// 1..10, power-up 2). Snapshot onto the field draft when the `^B*`
    /// command is parsed.
    pub by_module: u32,
    /// `^BY r` wide/narrow ratio in tenths (3.0 -> 30; guide p.148:
    /// 2.0..3.0 in 0.1 steps, power-up 3.0).
    pub by_ratio: u32,
    /// `^BY h` default barcode height in dots (guide p.148: power-up 10).
    pub by_height: u32,
    /// `^CI` international character set (power-up: 0, U.S.A. 1).
    pub code_page: u8,
    /// `^CW` font assignments: character -> stored font name.
    pub font_assignments: BTreeMap<u8, String>,
    /// `^A@` last named downloaded font; a later name-less `^A@` carries it
    /// over (guide ^A@: "The font named carries over on all subsequent ^A@
    /// commands without a font name").
    pub downloaded_font: Option<String>,
    /// `^FC` Real-Time Clock indicator characters (primary power-up: `%`).
    pub clock_indicators: [u8; 3],
    /// `^CC` format-command prefix character (power-up `^`). The handler
    /// records the change here; the parse driver applies it to the scanner
    /// (`Scanner::set_chars`) after each dispatch.
    pub cmd_char: u8,
    /// `^CT` control-command (tilde) prefix character (power-up `~`).
    pub tilde_char: u8,
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
            replicates: 0,
            default_font: *b"A ",
            default_font_h: 9,
            default_font_w: 5,
            fw_rotation: Rotation::R0,
            by_module: 2,
            by_ratio: 30,
            by_height: 10,
            code_page: 0,
            font_assignments: BTreeMap::new(),
            downloaded_font: None,
            clock_indicators: [b'%', 0, 0],
            cmd_char: b'^',
            tilde_char: b'~',
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

/// `^B*` barcode parameters on an open field draft: the `^BY` defaults
/// snapshotted when the symbology command was parsed, overridden by the
/// symbology's own parameters (^BC `o,h,f,g,e,m`; Task 15's symbologies
/// have their own param tables).
#[derive(Debug, Clone)]
pub struct BarcodeDraft {
    /// Symbology mnemonic letter, e.g. `b'C'` for `^BC`.
    pub symbology: u8,
    /// `^BY w`: module width in dots (power-up 2).
    pub module: u32,
    /// `^BY r`: wide/narrow ratio in tenths (3.0 -> 30; power-up 30).
    pub ratio: u32,
    /// `^BY h`, overridden by the symbology command's height parameter.
    pub height: u32,
    /// `^BC f`: print interpretation line below, `b'Y'`/`b'N'` (default Y).
    pub line: u8,
    /// `^BC g`: print interpretation line above the code (default N).
    pub interpret: bool,
    /// `^BC e`: UCC check digit (mod 10; default N).
    pub ucc: bool,
    /// `^BC m`: mode letter N/U/A/D (default N).
    pub model: u8,
    /// Per-symbology check-digit option: ^B1 e (1 or 2 digits), ^B3 e
    /// (Mod-43 Y/N), ^BM e (A/B/C/D). Meaningless for other symbologies.
    pub check: u8,
    /// ^BK k: Codabar start character (A-D, default A).
    pub start: u8,
    /// ^BK l: Codabar stop character (A-D, default A).
    pub stop: u8,
    /// ^BQ m: QR error-correction level letter H/Q/M/L (guide default Q).
    pub qr_ec: u8,
    /// ^BQ e: QR mask pattern 0..7 (default 0 - the reference's fixed
    /// pick; measured corpus 11/16).
    pub qr_mask: u8,
    /// ^BX s: DataMatrix quality level (0/50/80/100/140/200; guide default 0).
    pub dm_quality: u32,
    /// ^BX f: DataMatrix format ID 1..6 (guide default 6).
    pub dm_format: u8,
    /// ^BX a: DataMatrix aspect ratio 1 square / 2 rectangular (default 1).
    pub dm_aspect: u8,
    /// ^B0 d: Aztec error control / symbol size (0, 01-99, 101-104, 201-232,
    /// 300 = Rune; guide default 0).
    pub az_level: u32,
    /// ^B0 e: Aztec menu symbol indicator (Y/N; default N).
    pub az_menu: bool,
    /// ^B0 f: Aztec structured-append symbol count 1..26 (default 1).
    pub az_append: u8,
    /// ^B7 s: PDF417 security level 0..8 (guide default 0).
    pub pdf_security: u8,
    /// ^B7 c: PDF417 data columns 1..30 (0 = automatic).
    pub pdf_cols: u32,
    /// ^B7 r: PDF417 rows 3..90 (0 = automatic).
    pub pdf_rows: u32,
    /// ^B7 t: PDF417 truncation Y/N (default N).
    pub pdf_truncate: bool,
}

/// The shape a drawing-field command (`^GB ^GC ^GD ^GE`) deposits on the
/// open draft; converted into the matching IR variant at ^FS/^XZ.
#[derive(Debug, Clone)]
pub enum Geometry {
    Box {
        width: u32,
        height: u32,
        thickness: u32,
        color: u8,
    },
    Circle {
        diameter: u32,
        thickness: u32,
        color: u8,
    },
    Diagonal {
        width: u32,
        height: u32,
        thickness: u32,
        color: u8,
        /// `b'R'` right-leaning / `b'L'` left-leaning.
        direction: u8,
    },
    Ellipse {
        width: u32,
        height: u32,
        thickness: u32,
        color: u8,
    },
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
    /// `^CI` character set at data-accumulation time (snapshotted from the
    /// mode when the draft opened, updated by later `^CI` commands);
    /// `into_field` decodes `data` through it (Task 12).
    pub code_page: u8,
    pub block: Option<TextBlock>,
    /// `^B*` barcode parameters; `None` for text/graphic fields. Set by the
    /// symbology command (^BC here, ^B1..^BE in Task 15), which snapshots
    /// the ^BY defaults and applies its own parameters.
    pub barcode: Option<BarcodeDraft>,
    /// `^XG`/`^IM`/`^IL`: the field recalls a stored graphic instead of
    /// carrying text/barcode data.
    pub graphic: Option<GraphicKind>,
    /// `^GF`: the inline bitmap's (bytes, bytes-per-row) when the open
    /// field carries one; paired with `graphic: Some(Inline)`.
    pub inline_data: Option<(Vec<u8>, u32)>,
    /// `^GB`/`^GC`/`^GD`/`^GE`: the drawing-field shape on this draft.
    pub geometry: Option<Geometry>,
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
    /// `^SN` serialization setting; the data string's rightmost integer run
    /// is expanded at ^XZ and the counter tracked on [`ParseContext`]
    /// (Task 17).
    pub serial: Option<SerialSetting>,
    /// Raw command bytes of the field, for stored-format re-emission later.
    pub raw: Vec<u8>,
    /// `^FN#`: the field-number tag assigned to this field (guide p.200).
    /// At ^XZ the draft's decoded data lands in `Label::numbered` under
    /// this key, where the server's `^HV` responder reads it (Task 20).
    pub field_number: Option<u32>,
}

/// `^SN` serialization parameters (guide p.341) carried on a field draft.
/// The draft's `data` is the `v` string; its rightmost integer run is the
/// indexed portion (`step` = the signed `n` increment/decrement, `zeros` =
/// the `z` leading-zeros flag). `cmd.rs::expand_serials` applies the
/// counter's current value at ^XZ.
#[derive(Debug, Clone)]
pub struct SerialSetting {
    pub step: i64,
    pub zeros: bool,
}

/// A pending `^IS` image-save request, recorded by `memory::image_save` and
/// fulfilled at ^XZ (`cmd::end_format`): the format as it stands at the
/// `^IS` is rendered and stored in `Memory.images` under the normalized
/// name. `fields` is the count of drafts already closed when `^IS` was
/// parsed, so fields that follow it are not part of the saved image.
#[derive(Debug, Clone)]
pub(crate) struct ImageSave {
    pub name: String,
    pub fields: usize,
}

/// Where the parser is in the input stream.
// clippy: the Field variant's draft dwarfs the other variants; boxing it
// would touch a dozen destructure sites in cmd/textfmt for a one-time
// stack-layout saving in ParseContext - accepted as-is.
#[derive(Debug, Default)]
#[allow(clippy::large_enum_variant)]
pub enum FormatState {
    /// Not inside a format.
    #[default]
    Outside,
    /// Between ^XA and ^XZ; `fields` holds the drafts closed by ^FS.
    InFormat { fields: Vec<FieldDraft> },
    /// A field is open (between its origin/^FD and its ^FS). `fields` carries
    /// the drafts already closed in this format.
    Field {
        draft: FieldDraft,
        fields: Vec<FieldDraft>,
    },
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
        *self = Self::InFormat { fields: Vec::new() };
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
        // `^FN#` numbered fields: the decoded data of every tagged draft,
        // keyed by its number for the server's `^HV` responder. Decoded
        // through the same code page as `into_field`, so ^HV answers match
        // what the label prints. The guide allows several fields to share
        // one number (last one wins in the map).
        let mut numbered = BTreeMap::new();
        for d in &drafts {
            if let Some(n) = d.field_number {
                numbered.insert(n, crate::codepage::decode_text(&d.data, d.code_page));
            }
        }
        let fields = drafts.into_iter().map(FieldDraft::into_field).collect();
        Some(Label {
            width_dots: mode.pw_dots,
            height_dots: mode.ll_dots,
            copies: mode.copies,
            numbered,
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
            code_page: mode.code_page,
            block: None,
            barcode: None,
            graphic: None,
            inline_data: None,
            geometry: None,
            font_name: None,
            symbol: false,
            hex: None,
            re_eval: false,
            serial: None,
            raw: Vec::new(),
            field_number: None,
        }
    }

    /// Convert a closed draft into its IR field.
    fn into_field(self) -> Field {
        // Drawing fields (^GB ^GC ^GD ^GE) carry their shape on the draft;
        // the field origin comes from this draft's ^FO/^FT (or 0,0).
        if let Some(shape) = self.geometry {
            return match shape {
                Geometry::Box {
                    width,
                    height,
                    thickness,
                    color,
                } => Field::Box(BoxField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    width,
                    height,
                    thickness,
                    color,
                    reverse: self.reverse,
                }),
                Geometry::Circle {
                    diameter,
                    thickness,
                    color,
                } => Field::Circle(CircleField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    diameter,
                    thickness,
                    color,
                    reverse: self.reverse,
                }),
                Geometry::Diagonal {
                    width,
                    height,
                    thickness,
                    color,
                    direction,
                } => Field::Diagonal(DiagonalField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    width,
                    height,
                    thickness,
                    color,
                    direction,
                    reverse: self.reverse,
                }),
                Geometry::Ellipse {
                    width,
                    height,
                    thickness,
                    color,
                } => Field::Ellipse(EllipseField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    width,
                    height,
                    thickness,
                    color,
                    reverse: self.reverse,
                }),
            };
        }
        // Single decode point for text and barcode fields: bytes were
        // accumulated verbatim with ^FH escapes already expanded, then
        // decoded once through the field's ^CI character set.
        let data: String = crate::codepage::decode_text(&self.data, self.code_page);
        match self.graphic {
            // Inline ^GF carries its bitmap on the draft; stored-image
            // recall (^XG/^IM/^IL) leaves the bytes to the renderer, which
            // resolves them from Memory.images by name (Task 10).
            Some(kind) => {
                let (data, bytes_per_row) = self.inline_data.unwrap_or_default();
                Field::Graphic(GraphicField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    kind,
                    data,
                    bytes_per_row,
                })
            }
            None => match self.barcode {
                Some(b) => Field::Barcode(BarcodeField {
                    x: self.x,
                    y: self.y,
                    origin: self.origin,
                    symbology: b.symbology,
                    data,
                    module: b.module,
                    ratio: b.ratio,
                    height: b.height,
                    line: b.line,
                    interpret: b.interpret,
                    ucc: b.ucc,
                    model: b.model,
                    check: b.check,
                    start: b.start,
                    stop: b.stop,
                    qr_ec: b.qr_ec,
                    qr_mask: b.qr_mask,
                    dm_quality: b.dm_quality,
                    dm_format: b.dm_format,
                    dm_aspect: b.dm_aspect,
                    az_level: b.az_level,
                    az_menu: b.az_menu,
                    az_append: b.az_append,
                    pdf_security: b.pdf_security,
                    pdf_cols: b.pdf_cols,
                    pdf_rows: b.pdf_rows,
                    pdf_truncate: b.pdf_truncate,
                    // The field's ^A font is captured at ^FS like text,
                    // but the reference renders the interpretation line in
                    // built-in font 0 at ~15% of the bar height, ignoring
                    // it (measured, corpus 08/21) - see render::barcode.
                    font: self.font,
                    font_height: self.font_height,
                    font_width: self.font_width,
                    font_name: self.font_name,
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

/// A host-query request recorded by a parse handler and answered by the TCP
/// session (Task 20). Handlers cannot respond (there is no socket on the
/// parser side), so parsing merely records the request; the session drains
/// `ParseContext::queries` after each parse window and writes the response.
///
/// Detection goes through the scanner like every other command, which is
/// what makes ^FD bodies safe: inside field data `~` is literal text, so a
/// `~HS` embedded in a field never reaches a handler (the old byte-scan
/// false-positive regression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryRequest {
    /// `~HS` - host status: three STX-framed strings as registered.
    HostStatus,
    /// `~HQ` - head query; `params` is the query-type (OD, SN, PH, ...).
    HeadQuery(Vec<u8>),
    /// `~WD` - printer configuration dump.
    ConfigDump(Vec<u8>),
    /// `~HI` - host identification (model, firmware, resolution, memory).
    ModelId,
    /// `^HV` - return a numbered field's data to the host.
    HostValue(HostValueRequest),
}

/// Parsed `^HV#,n,h,t,a` parameters (guide p.239).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostValueRequest {
    /// `#`: the `^FN` field number whose data is returned (default 0).
    pub field: u32,
    /// `n`: number of data bytes to return (default 64; guide range 1..256).
    pub max_bytes: usize,
    /// `h`: header bytes sent before the field data (^FH-expanded).
    pub header: Vec<u8>,
    /// `t`: termination bytes sent after the field data (^FH-expanded).
    pub terminator: Vec<u8>,
}

/// An interrupted binary download (`^GF` B/C, `~DY` B) whose declared
/// payload straddles a parse-chunk boundary. The header's `params` borrow
/// the chunk buffer, which the caller drains after parsing, so the driver
/// copies them; `buf` accumulates the raw tail consumed from later chunks
/// (the remaining `declared - already` bytes, taken raw so `^`/`~` bytes
/// inside the payload never tokenize). The parse loop completes the
/// download in the chunk that supplies the last owed byte and runs
/// `handler` with the original header params plus the accumulated tail -
/// exactly what the single-chunk path receives. A stream that truly ends
/// mid-payload warns at EOF (whole-stream `parse_stream`'s tail check;
/// the TCP session's EOF branch); a payload merely spanning chunks is
/// valid input and stays silent.
#[derive(Debug)]
pub struct PendingBinary {
    /// Two-byte command mnemonic, for the truncation warning's command.
    pub mnemonic: [u8; 2],
    /// The header's parameter bytes as scanned (header segments plus any
    /// payload bytes the params run absorbed before the first command
    /// character), copied out of the chunk buffer.
    pub params: Vec<u8>,
    /// Payload bytes the header's params run already absorbed.
    pub already: usize,
    /// The byte count the command declared.
    pub declared: usize,
    /// Raw payload bytes accumulated from later chunks.
    pub buf: Vec<u8>,
    /// The command's payload receiver (`CommandSpec::binary_handler`).
    pub handler: fn(&mut ParseContext, &[u8], &[u8]),
}

/// Per-format parsing context handed to command handlers.
pub struct ParseContext<'a> {
    pub state: FormatState,
    pub diags: &'a mut Diagnostics,
    pub memory: &'a mut Memory,
    pub mode: PrinterMode,
    /// `^IS` image-save request recorded while this parse window ran; the
    /// ^XZ handler renders the format-so-far and stores it under the
    /// normalized name (guide p.249).
    pub(crate) image_save: Option<ImageSave>,
    /// Non-geometry printer settings (^MD/~SD darkness, ^MN tracking, ...).
    pub config: PrinterConfig,
    /// The most recently completed label (^XZ), for the protocol layer.
    pub pending_label: Option<Label>,
    /// Active ^XF stored-format recall depth (recursion cap 4; see
    /// memory.rs `recall_format`).
    pub recall_depth: u32,
    /// `^SN` serial counters, keyed by the field's ordinal among serialized
    /// fields in its format, in the same order as the drafts at ^XZ. The
    /// server persists these across sessions (Task 21).
    pub serial_counters: BTreeMap<u8, i64>,
    /// Host queries (`~HS`/`~HQ`/`~WD`/`~HI`/`^HV`) recorded by handlers
    /// while this parse window ran, in stream order. The TCP session drains
    /// them after each chunk and answers on the socket (Task 20).
    pub queries: Vec<QueryRequest>,
    /// Global byte offset of the token currently being dispatched
    /// (`stream_base` + chunk-local scanner position). Maintained by the
    /// parse driver (Task 19) so the ^XA/^XZ handlers can record a label's
    /// raw-input span for the archive.
    pub token_start: usize,
    /// Global byte offset just past the token currently being dispatched.
    pub token_end: usize,
    /// Bytes of input consumed by earlier chunks of this stream. The TCP
    /// session sets this before each parse call so token offsets survive
    /// chunk boundaries.
    pub stream_base: usize,
    /// Global offset of the open format's `^XA` (set from `token_start` by
    /// the ^XA handler); the start of the raw `.zpl` span archived with the
    /// label closed by `^XZ`.
    pub format_start: usize,
    /// `ctx.diags.items.len()` at the open format's `^XA` (set by the ^XA
    /// handler); the start of the label's diagnostic-index window, which
    /// the ^XZ push completes so the server can attribute diagnostics to
    /// the right label when one chunk contains several formats.
    pub diag_start: usize,
    /// A binary download (`^GF` B/C, `~DY` B) interrupted by a chunk
    /// boundary; the next parse call's loop keeps raw-consuming it. `None`
    /// once the payload completed or the stream ended (warned there).
    pub pending_binary: Option<PendingBinary>,
}

impl<'a> ParseContext<'a> {
    pub fn new(diags: &'a mut Diagnostics, memory: &'a mut Memory) -> Self {
        ParseContext {
            state: FormatState::default(),
            diags,
            memory,
            mode: PrinterMode::default(),
            image_save: None,
            config: PrinterConfig::default(),
            pending_label: None,
            recall_depth: 0,
            serial_counters: BTreeMap::new(),
            queries: Vec::new(),
            token_start: 0,
            token_end: 0,
            stream_base: 0,
            format_start: 0,
            diag_start: 0,
            pending_binary: None,
        }
    }
}
