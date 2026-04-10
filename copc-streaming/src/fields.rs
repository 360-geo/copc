//! Field selection for selective LAZ decompression.

use bitflags::bitflags;

bitflags! {
    /// Which point fields to decompress.
    ///
    /// On LAS 1.4 layered point formats (6, 7, 8 — the formats COPC mandates),
    /// LAZ decompression is organized as independent per-field byte layers.
    /// Omitted fields skip arithmetic decoding of their layer entirely, which
    /// is a direct CPU saving proportional to the number of layers dropped.
    ///
    /// The following fields are always decoded regardless of the mask because
    /// they share a single base layer: `x`, `y`, `return_number`,
    /// `number_of_returns`, and `scanner_channel`. They are "free" in the
    /// sense that you cannot skip them. The minimum mask that still gives
    /// full 3D geometry is `Fields::Z` (just the Z layer on top of the
    /// always-on base).
    ///
    /// When a field is not present in the mask, its bytes in the decompressed
    /// chunk remain zero. Calling a column accessor for a skipped field on a
    /// [`Chunk`](crate::Chunk) returns `None`, so downstream code cannot
    /// silently read stale zeros.
    ///
    /// # Composing masks
    ///
    /// Use bitwise `|` to combine flags and [`Fields::empty`] /
    /// [`Fields::ALL`] as the extremes:
    ///
    /// ```
    /// use copc_streaming::Fields;
    ///
    /// // Geometry only.
    /// let geometry = Fields::Z;
    ///
    /// // Geometry + color.
    /// let geometry_rgb = Fields::Z | Fields::RGB;
    ///
    /// // Geometry + gps time.
    /// let geometry_time = Fields::Z | Fields::GPS_TIME;
    ///
    /// // Full decode — required if you want `Chunk::to_points()` or
    /// // `Chunk::points_at()` to succeed.
    /// let everything = Fields::ALL;
    ///
    /// // `Fields::empty()` decodes only the always-on base layer:
    /// // x, y, return_number, number_of_returns, scanner_channel.
    /// let bare = Fields::empty();
    /// ```
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
    pub struct Fields: u32 {
        /// Z coordinate.
        const Z               = 1 << 0;
        /// Intensity (return strength).
        const INTENSITY       = 1 << 1;
        /// Classification code.
        const CLASSIFICATION  = 1 << 2;
        /// Flag bits (synthetic / key-point / withheld / overlap, etc.).
        const FLAGS           = 1 << 3;
        /// Scan angle.
        const SCAN_ANGLE      = 1 << 4;
        /// User data byte.
        const USER_DATA       = 1 << 5;
        /// Point source ID.
        const POINT_SOURCE_ID = 1 << 6;
        /// GPS time.
        const GPS_TIME        = 1 << 7;
        /// RGB color triple.
        const RGB             = 1 << 8;
        /// Near-infrared value.
        const NIR             = 1 << 9;
        /// Full-waveform packet data.
        const WAVEPACKET      = 1 << 10;
        /// Extra bytes region.
        const EXTRA_BYTES     = 1 << 11;

        /// Decode every field. The only mask that lets a chunk be materialized
        /// back into `las::Point` values via [`Chunk::to_points`](crate::Chunk::to_points).
        const ALL = Self::Z.bits()
                  | Self::INTENSITY.bits()
                  | Self::CLASSIFICATION.bits()
                  | Self::FLAGS.bits()
                  | Self::SCAN_ANGLE.bits()
                  | Self::USER_DATA.bits()
                  | Self::POINT_SOURCE_ID.bits()
                  | Self::GPS_TIME.bits()
                  | Self::RGB.bits()
                  | Self::NIR.bits()
                  | Self::WAVEPACKET.bits()
                  | Self::EXTRA_BYTES.bits();
    }
}

impl Fields {
    /// Build a [`laz::DecompressionSelection`] matching this mask.
    ///
    /// Uses the laz builder API (`xy_returns_channel()` + `decompress_*`)
    /// instead of a raw bit-cast, so the public `Fields` layout is not
    /// coupled to laz's internal representation.
    pub(crate) fn to_laz_selection(self) -> laz::DecompressionSelection {
        let mut sel = laz::DecompressionSelection::xy_returns_channel();
        if self.contains(Fields::Z) {
            sel = sel.decompress_z();
        }
        if self.contains(Fields::INTENSITY) {
            sel = sel.decompress_intensity();
        }
        if self.contains(Fields::CLASSIFICATION) {
            sel = sel.decompress_classification();
        }
        if self.contains(Fields::FLAGS) {
            sel = sel.decompress_flags();
        }
        if self.contains(Fields::SCAN_ANGLE) {
            sel = sel.decompress_scan_angle();
        }
        if self.contains(Fields::USER_DATA) {
            sel = sel.decompress_user_data();
        }
        if self.contains(Fields::POINT_SOURCE_ID) {
            sel = sel.decompress_point_source_id();
        }
        if self.contains(Fields::GPS_TIME) {
            sel = sel.decompress_gps_time();
        }
        if self.contains(Fields::RGB) {
            sel = sel.decompress_rgb();
        }
        if self.contains(Fields::NIR) {
            sel = sel.decompress_nir();
        }
        if self.contains(Fields::WAVEPACKET) {
            sel = sel.decompress_wavepacket();
        }
        if self.contains(Fields::EXTRA_BYTES) {
            sel = sel.decompress_extra_bytes();
        }
        sel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_contains_every_layer() {
        assert!(Fields::ALL.contains(Fields::Z));
        assert!(Fields::ALL.contains(Fields::INTENSITY));
        assert!(Fields::ALL.contains(Fields::CLASSIFICATION));
        assert!(Fields::ALL.contains(Fields::FLAGS));
        assert!(Fields::ALL.contains(Fields::SCAN_ANGLE));
        assert!(Fields::ALL.contains(Fields::USER_DATA));
        assert!(Fields::ALL.contains(Fields::POINT_SOURCE_ID));
        assert!(Fields::ALL.contains(Fields::GPS_TIME));
        assert!(Fields::ALL.contains(Fields::RGB));
        assert!(Fields::ALL.contains(Fields::NIR));
        assert!(Fields::ALL.contains(Fields::WAVEPACKET));
        assert!(Fields::ALL.contains(Fields::EXTRA_BYTES));
    }

    #[test]
    fn composition() {
        let f = Fields::Z | Fields::GPS_TIME;
        assert!(f.contains(Fields::Z));
        assert!(f.contains(Fields::GPS_TIME));
        assert!(!f.contains(Fields::RGB));
        assert!(!f.contains(Fields::INTENSITY));
    }

    #[test]
    fn empty_mask_has_no_layers_set() {
        let sel = Fields::empty().to_laz_selection();
        assert!(!sel.should_decompress_z());
        assert!(!sel.should_decompress_classification());
        assert!(!sel.should_decompress_intensity());
        assert!(!sel.should_decompress_gps_time());
        assert!(!sel.should_decompress_rgb());
    }

    #[test]
    fn selective_mask_enables_only_requested_layers() {
        let sel = (Fields::Z | Fields::GPS_TIME).to_laz_selection();
        assert!(sel.should_decompress_z());
        assert!(sel.should_decompress_gps_time());
        assert!(!sel.should_decompress_rgb());
        assert!(!sel.should_decompress_intensity());
        assert!(!sel.should_decompress_classification());
        assert!(!sel.should_decompress_nir());
    }

    #[test]
    fn all_mask_enables_every_layer() {
        let sel = Fields::ALL.to_laz_selection();
        assert!(sel.should_decompress_z());
        assert!(sel.should_decompress_intensity());
        assert!(sel.should_decompress_classification());
        assert!(sel.should_decompress_flags());
        assert!(sel.should_decompress_scan_angle());
        assert!(sel.should_decompress_user_data());
        assert!(sel.should_decompress_point_source_id());
        assert!(sel.should_decompress_gps_time());
        assert!(sel.should_decompress_rgb());
        assert!(sel.should_decompress_nir());
        assert!(sel.should_decompress_wavepacket());
        assert!(sel.should_decompress_extra_bytes());
    }
}
