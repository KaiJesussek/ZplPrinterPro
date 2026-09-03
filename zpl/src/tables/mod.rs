//! Standard tables transcribed from the ISO/IEC barcode standards
//! (as carried by the reference implementation zint, BSD-3-Clause).
//! Task 16: wired into `bc2d` by the encoder continuation; kept
//! compile-checked here.

#[allow(dead_code)]
pub(crate) mod aztec_sizes_table;
#[allow(dead_code)]
pub(crate) mod aztec_table;
#[allow(dead_code)]
pub(crate) mod dm_table;
#[allow(dead_code)]
pub(crate) mod pdf_table;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shapes() {
        assert_eq!(dm_table::SIZE_H.len(), 48);
        assert_eq!(pdf_table::PDF_BITPATTERN.len(), 2787);
        assert_eq!(aztec_table::COMPACT_MAP.len(), 27 * 27);
        assert_eq!(aztec_table::CORE_MAP.len(), 225);
        assert_eq!(aztec_sizes_table::AZTEC_SIZES.len(), 32);
        // 144x144 square holds 1558 data codewords; 8x18 rect 5.
        assert_eq!(dm_table::SIZE_BYTES[47], 1558);
        assert_eq!(dm_table::SIZE_BYTES[2], 5);
    }
}
