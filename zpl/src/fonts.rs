//! Font provider interface, placeholder until Task 11 fills it in.
//!
//! `RenderOptions` (Task 10) carries `fonts: &dyn FontProvider` so the
//! renderer's signature stays stable; this task paints no text, so the
//! spec/data types are empty stubs. Task 11 adds the `Zebra(u8)` /
//! `Downloaded(String)` spec variants, the metrics tables, and the real
//! rasterized face.

/// A font request: which face (and size) the renderer wants. Task 11 fills
/// in the variants; nothing constructs one today.
pub struct FontSpec;

/// A rasterized font the renderer can draw from. Task 11 fills it in.
pub struct FontData;

/// Supplies fonts to the renderer.
pub trait FontProvider {
    /// Look up the font named by `spec`; `None` when it is unavailable
    /// (missing download, unmapped letter - the caller warns and skips).
    fn get(&self, spec: &FontSpec) -> Option<FontData>;
}
