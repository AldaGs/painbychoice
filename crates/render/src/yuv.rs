//! Decoded YCbCr planes and their conversion to display RGB, HDR included.
//!
//! One place for the colour maths, so the preview and the export can't drift:
//! the preview's shader (`live::video`) mirrors [`YuvFrame::to_rgba`] and is
//! tested against it, and the export sidecar converts HDR footage through
//! `to_rgba` itself. Pure Rust, so it builds without the `libav` feature.

/// Which YCbCr→RGB matrix a frame was encoded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// How a frame's RGB is encoded. Everything that isn't HDR draws as-is, the
/// way every SDR clip always has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transfer {
    #[default]
    Sdr,
    /// Hybrid log-gamma (ARIB STD-B67): phones, broadcast HDR.
    Hlg,
    /// Perceptual quantizer (SMPTE ST 2084): HDR10, Dolby Vision base layers.
    Pq,
}

impl Transfer {
    /// The value the shader switches on.
    pub fn code(self) -> f32 {
        match self {
            Transfer::Sdr => 0.0,
            Transfer::Hlg => 1.0,
            Transfer::Pq => 2.0,
        }
    }
}

/// The HDR light level that becomes SDR white (before the roll-off).
///
/// Twice HDR reference white (203 nits, BT.2408), which puts reference white
/// at 75% of the SDR signal — where HLG's own signal puts it, by design, so
/// that HLG shown untouched on an SDR screen looks about right. A converted
/// clip is then as bright as that, with its colours corrected. Mapping 203
/// nits straight to SDR white (BT.2408's literal advice) ran a stop hot on
/// phone footage: floors and walls went to white.
// ponytail: one fixed exposure for all footage; make it a per-asset setting
// if a grade needs to push it.
pub const SDR_WHITE_NITS: f32 = 406.0;
/// The display HLG is rendered for (BT.2100's nominal peak).
pub const HLG_PEAK_NITS: f32 = 1000.0;
/// Below this (in SDR white = 1) the picture is untouched; above it highlights
/// roll off smoothly, reaching 1 at the 1000-nit peak instead of clipping.
pub const KNEE: f32 = 0.8;
/// Where the roll-off reaches SDR white: a 1000-nit display's peak. PQ
/// mastered brighter than that clips above it.
// ponytail: fixed rather than read from the file's mastering metadata;
// read MaxCLL when bright PQ grades need their top end.
pub const PEAK: f32 = HLG_PEAK_NITS / SDR_WHITE_NITS;
/// How far a highlight is pulled towards grey by the time it reaches SDR
/// white. Rolling off on the brightest channel keeps hue but also keeps full
/// saturation, so bright skin and lit plastic glowed orange and red; a
/// display does lose saturation as it runs out of range, and this mimics it.
/// Ramps in from the knee, so midtones are untouched.
// ponytail: a fixed amount tuned on one phone clip; a calibration knob.
pub const HIGHLIGHT_DESAT: f32 = 0.5;

/// BT.2020 to BT.709 primaries, linear light.
pub const BT2020_TO_BT709: [[f32; 3]; 3] = [
    [1.6605, -0.5876, -0.0728],
    [-0.1246, 1.1329, -0.0083],
    [-0.0182, -0.1006, 1.1187],
];

/// One HDR pixel, `rgb` the BT.2020 non-linear signal in 0..1, to SDR BT.709
/// non-linear in 0..1. The shader in `live::video` is this function in WGSL.
pub fn hdr_to_sdr(rgb: [f32; 3], transfer: Transfer) -> [f32; 3] {
    // To display light in nits.
    let nits = match transfer {
        Transfer::Sdr => return rgb,
        Transfer::Hlg => {
            let (a, b, c) = (0.178_832_77f32, 0.284_668_92f32, 0.559_910_7f32);
            let e = rgb.map(|v| {
                let v = v.clamp(0.0, 1.0);
                if v <= 0.5 { v * v / 3.0 } else { (((v - c) / a).exp() + b) / 12.0 }
            });
            // The OOTF: HLG is scene light, and the display's system gamma
            // (1.2 at 1000 nits) is part of how it is meant to look.
            let ys = 0.2627 * e[0] + 0.6780 * e[1] + 0.0593 * e[2];
            let gain = HLG_PEAK_NITS * ys.max(1e-6).powf(0.2);
            e.map(|v| v * gain)
        }
        Transfer::Pq => rgb.map(|v| {
            let (m1, m2, c1, c2, c3) = (0.159_301_76f32, 78.84375f32, 0.8359375f32, 18.851_563f32, 18.6875f32);
            let p = v.clamp(0.0, 1.0).powf(1.0 / m2);
            10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
        }),
    };
    let x = nits.map(|v| v / SDR_WHITE_NITS);
    let m = BT2020_TO_BT709;
    let lin = [0, 1, 2].map(|i| (m[i][0] * x[0] + m[i][1] * x[1] + m[i][2] * x[2]).max(0.0));
    // Roll off on the brightest channel and scale all three by the same
    // amount, so a highlight keeps its hue instead of bleaching per channel.
    let peak = lin[0].max(lin[1]).max(lin[2]);
    // Extended Reinhard above the knee: slope 1 where it joins, exactly 1 at
    // `PEAK`.
    let scale = if peak > KNEE {
        let (t, top) = ((peak - KNEE) / (1.0 - KNEE), (PEAK - KNEE) / (1.0 - KNEE));
        (KNEE + (1.0 - KNEE) * (t * (1.0 + t / (top * top)) / (1.0 + t)).min(1.0)) / peak
    } else {
        1.0
    };
    let lin = lin.map(|v| v * scale);
    let grey = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
    let desat = HIGHLIGHT_DESAT * ((peak - KNEE) / (1.0 - KNEE)).clamp(0.0, 1.0);
    // Encoded for a BT.1886 (gamma 2.4) display, like every SDR video.
    lin.map(|v| (v + (grey - v) * desat).min(1.0).powf(1.0 / 2.4))
}

/// One decoded frame as 8-bit NV12: a full-resolution luma plane and one
/// half-resolution plane of interleaved Cb,Cr. Rows are tightly packed (no
/// stride padding), so a plane uploads to a texture as-is.
#[derive(Clone, Debug)]
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes.
    pub y: Vec<u8>,
    /// `chroma_width() * chroma_height() * 2` bytes.
    pub uv: Vec<u8>,
    pub matrix: Matrix,
    /// Full ("JPEG") range rather than the usual 16–235 video range.
    pub full_range: bool,
    pub transfer: Transfer,
}

impl YuvFrame {
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
    pub fn bytes(&self) -> usize {
        self.y.len() + self.uv.len()
    }

    /// The offsets and 3x3 matrix that take `(y, cb, cr)` in 0..1 to RGB:
    /// `rgb = m * (yuv - offset)`. Shared by the CPU reference below and the
    /// preview's shader, so the two cannot drift.
    pub fn coefficients(&self) -> ([f32; 3], [[f32; 3]; 3]) {
        let (kr, kb) = match self.matrix {
            Matrix::Bt601 => (0.299f32, 0.114f32),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = if self.full_range {
            (0.0, 1.0, 1.0)
        } else {
            (16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0)
        };
        let rv = 2.0 * (1.0 - kr) * c_scale;
        let bu = 2.0 * (1.0 - kb) * c_scale;
        let gu = -2.0 * (1.0 - kb) * kb / kg * c_scale;
        let gv = -2.0 * (1.0 - kr) * kr / kg * c_scale;
        // Rows are R, G, B; columns are Y, Cb, Cr.
        ([y_off, 0.5, 0.5], [[y_scale, 0.0, rv], [y_scale, gu, gv], [y_scale, bu, 0.0]])
    }

    /// CPU conversion to straight RGBA: the reference the preview's shader is
    /// tested against, and how the export converts HDR footage. Rows are split
    /// across cores: an HDR pixel costs a few `exp`/`pow`s, which is over a
    /// second per 4K frame on one.
    // ponytail: still ~120ms per 4K HDR frame (7 fps streamed export); lookup
    // tables for the transfer and encode curves are the upgrade.
    pub fn to_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![255u8; w * h * 4];
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let band = h.div_ceil(threads).max(1);
        std::thread::scope(|s| {
            for (i, chunk) in out.chunks_mut(band * w * 4).enumerate() {
                s.spawn(move || self.rows_into(i * band, chunk));
            }
        });
        out
    }

    /// Convert the rows starting at `first` into `out`, whole rows of RGBA.
    fn rows_into(&self, first: usize, out: &mut [u8]) {
        let (off, m) = self.coefficients();
        let (w, cw) = (self.width as usize, self.chroma_width() as usize);
        for (r, line) in out.chunks_mut(w * 4).enumerate() {
            let row = first + r;
            for col in 0..w {
                let c = ((row / 2) * cw + col / 2) * 2;
                let yuv = [
                    self.y[row * w + col] as f32 / 255.0 - off[0],
                    self.uv[c] as f32 / 255.0 - off[1],
                    self.uv[c + 1] as f32 / 255.0 - off[2],
                ];
                let rgb = m.map(|r| r[0] * yuv[0] + r[1] * yuv[1] + r[2] * yuv[2]);
                let rgb = hdr_to_sdr(rgb, self.transfer);
                for (ch, v) in line[col * 4..][..3].iter_mut().zip(rgb) {
                    *ch = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anchors of the mapping: black stays black, HDR reference white
    /// (75% HLG, 58% PQ) lands at 75% SDR — where HLG itself puts it — and
    /// the brightest signal rolls off to 1 instead of clipping early.
    #[test]
    fn hdr_reference_white_lands_where_hlg_puts_it() {
        for t in [Transfer::Hlg, Transfer::Pq] {
            assert_eq!(hdr_to_sdr([0.0; 3], t), [0.0; 3], "{t:?} black");
            let white = if t == Transfer::Hlg { 0.75 } else { 0.5806 };
            let w = hdr_to_sdr([white; 3], t);
            assert!(w.iter().all(|v| (0.72..=0.78).contains(v)), "{t:?} reference white -> {w:?}");
            let top = hdr_to_sdr([1.0; 3], t);
            assert!(top.iter().all(|v| *v > w[0] && *v <= 1.0), "{t:?} peak -> {top:?}");
        }
        assert_eq!(hdr_to_sdr([0.3, 0.5, 0.7], Transfer::Sdr), [0.3, 0.5, 0.7]);
    }

    /// Brighter in is never darker out: a ramp stays a ramp.
    #[test]
    fn the_mapping_is_monotonic() {
        for t in [Transfer::Hlg, Transfer::Pq] {
            let mut last = -1.0;
            for i in 0..=1000 {
                let v = hdr_to_sdr([i as f32 / 1000.0; 3], t)[1];
                assert!(v >= last - 1e-6, "{t:?} dips at {i}");
                last = v;
            }
        }
    }
}
