// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Chroma upsampling, colour conversion, and EXIF orientation to RGBA8.

/// One component's samples at its own resolution.
pub(crate) struct Plane {
    pub(crate) samples: Vec<u8>,
    /// Row stride in samples (a whole number of blocks).
    pub(crate) stride: usize,
    /// The component's real extent (T.81 A.1.1).
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// Output pixels per sample, across and down (`Hmax / H`, `Vmax / V`) when whole.
    pub(crate) factor: Option<(usize, usize)>,
    /// The sampling factors themselves, for the replication fallback.
    pub(crate) sampling: (usize, usize),
}

impl Plane {
    #[inline]
    fn row(&self, y: usize) -> &[u8] {
        let y = y.min(self.height.saturating_sub(1));
        let start = y * self.stride;
        self.samples
            .get(start..start + self.width)
            .unwrap_or_default()
    }

    /// Output row `y` of this component, `width` samples long, upsampled to the image's resolution.
    pub(crate) fn upsampled_row(
        &self,
        y: usize,
        width: usize,
        max: (usize, usize),
        out: &mut [u8],
    ) {
        let out = &mut out[..width];
        match self.factor {
            Some((1, 1)) => copy_row(self.row(y), out),
            Some((2, 1)) => h2_fancy(self.row(y), out),
            Some((1, 2)) => {
                let (near, far) = self.vertical_pair(y);
                for (x, slot) in out.iter_mut().enumerate() {
                    let near = near.get(x).copied().unwrap_or(0);
                    let far = far.get(x).copied().unwrap_or(near);
                    *slot = ((3 * u16::from(near) + u16::from(far) + 2) >> 2) as u8;
                }
            }
            Some((2, 2)) => {
                let (near, far) = self.vertical_pair(y);
                h2v2_fancy(near, far, out);
            }
            _ => {
                let source_y = y * self.sampling.1 / max.1;
                let row = self.row(source_y);
                for (x, slot) in out.iter_mut().enumerate() {
                    let source_x = (x * self.sampling.0 / max.0).min(row.len().saturating_sub(1));
                    *slot = row.get(source_x).copied().unwrap_or(0);
                }
            }
        }
    }

    /// For a component halved vertically: the sample row nearest output row
    /// `y` and the one on the far side of it (the edge row repeats).
    #[inline]
    fn vertical_pair(&self, y: usize) -> (&[u8], &[u8]) {
        let near = y / 2;
        let far = if y.is_multiple_of(2) {
            near.saturating_sub(1)
        } else {
            (near + 1).min(self.height.saturating_sub(1))
        };
        (self.row(near), self.row(far))
    }
}

#[inline]
fn copy_row(row: &[u8], out: &mut [u8]) {
    for (x, slot) in out.iter_mut().enumerate() {
        *slot = row.get(x).or_else(|| row.last()).copied().unwrap_or(0);
    }
}

/// Double a row's width with the triangle filter.
#[inline]
fn h2_fancy(row: &[u8], out: &mut [u8]) {
    let n = row.len();
    if n == 0 {
        out.fill(0);
        return;
    }
    let at = |i: usize| u16::from(row[i.min(n - 1)]);
    for (x, slot) in out.iter_mut().enumerate() {
        let i = (x / 2).min(n - 1);
        let neighbour = if x % 2 == 0 {
            at(i.saturating_sub(1))
        } else {
            at(i + 1)
        };
        let bias = if x % 2 == 0 { 1 } else { 2 };
        *slot = ((3 * at(i) + neighbour + bias) >> 2) as u8;
    }
}

/// [`h2_fancy`] after the same filter down the column.
#[inline]
fn h2v2_fancy(near: &[u8], far: &[u8], out: &mut [u8]) {
    let n = near.len();
    if n == 0 {
        out.fill(0);
        return;
    }
    let column = |i: usize| {
        let i = i.min(n - 1);
        3 * u16::from(near[i]) + u16::from(far.get(i).copied().unwrap_or(near[i]))
    };
    for (x, slot) in out.iter_mut().enumerate() {
        let i = (x / 2).min(n - 1);
        let neighbour = if x % 2 == 0 {
            column(i.saturating_sub(1))
        } else {
            column(i + 1)
        };
        let bias = if x % 2 == 0 { 8 } else { 7 };
        *slot = ((3 * column(i) + neighbour + bias) >> 4) as u8;
    }
}

/// What the components mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorModel {
    Gray,
    YCbCr,
    Rgb,
    /// Adobe CMYK, stored inverted (as every Adobe writer stores it).
    Cmyk,
    /// Adobe YCCK: YCbCr for the inverted CMY, then K.
    Ycck,
}

/// One pixel of JFIF YCbCr as sRGB (T.871 section 7), in 16-bit fixed point.
#[inline]
fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = (i32::from(y) << 16) + (1 << 15);
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let clamp = |value: i32| (value >> 16).clamp(0, 255) as u8;
    [
        clamp(y + 91_881 * cr),
        clamp(y - 22_554 * cb - 46_802 * cr),
        clamp(y + 116_130 * cb),
    ]
}

/// Convert one row of components (`rows[c][x]`) to RGBA8.
pub(crate) fn convert_row(model: ColorModel, rows: &[Vec<u8>], width: usize, out: &mut [u8]) {
    let sample = |c: usize, x: usize| rows.get(c).and_then(|row| row.get(x)).copied().unwrap_or(0);
    for (x, pixel) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .take(width)
        .enumerate()
    {
        let rgb = match model {
            ColorModel::Gray => {
                let v = sample(0, x);
                [v, v, v]
            }
            ColorModel::YCbCr => ycbcr_to_rgb(sample(0, x), sample(1, x), sample(2, x)),
            ColorModel::Rgb => [sample(0, x), sample(1, x), sample(2, x)],
            ColorModel::Cmyk => {
                let k = u16::from(sample(3, x));
                let ink = |c: u8| ((u16::from(c) * k + 127) / 255) as u8;
                [ink(sample(0, x)), ink(sample(1, x)), ink(sample(2, x))]
            }
            ColorModel::Ycck => {
                let k = u16::from(sample(3, x));
                let [r, g, b] = ycbcr_to_rgb(sample(0, x), sample(1, x), sample(2, x));
                let ink = |c: u8| (((255 - u16::from(c)) * k + 127) / 255) as u8;
                [ink(r), ink(g), ink(b)]
            }
        };
        pixel[..3].copy_from_slice(&rgb);
        pixel[3] = 255;
    }
}

/// Apply EXIF orientation `orientation`.
pub(crate) fn orient(
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    orientation: u16,
) -> (Vec<u8>, usize, usize) {
    if !(2..=8).contains(&orientation) {
        return (rgba, width, height);
    }
    let transposed = orientation >= 5;
    let (out_w, out_h) = if transposed {
        (height, width)
    } else {
        (width, height)
    };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (y, height - 1 - x),
                7 => (width - 1 - y, height - 1 - x),
                _ => (width - 1 - y, x),
            };
            let from = (sy * width + sx) * 4;
            let to = (y * out_w + x) * 4;
            if let (Some(source), Some(target)) =
                (rgba.get(from..from + 4), out.get_mut(to..to + 4))
            {
                target.copy_from_slice(source);
            }
        }
    }
    (out, out_w, out_h)
}

#[cfg(test)]
mod tests {
    use super::{ColorModel, convert_row, orient, ycbcr_to_rgb};

    #[test]
    fn ycbcr_converts_as_jfif_defines() {
        assert_eq!(ycbcr_to_rgb(100, 128, 128), [100, 100, 100]);
        let red = ycbcr_to_rgb(76, 85, 255);
        assert!(red[0] > 250 && red[1] < 5 && red[2] < 5, "{red:?}");
        let blue = ycbcr_to_rgb(29, 255, 107);
        assert!(blue[2] > 250 && blue[0] < 5 && blue[1] < 5, "{blue:?}");
    }

    #[test]
    fn adobe_cmyk_is_read_inverted() {
        let rows = vec![vec![255], vec![255], vec![0], vec![255]];
        let mut out = [0u8; 4];
        convert_row(ColorModel::Cmyk, &rows, 1, &mut out);
        assert_eq!(out, [255, 255, 0, 255], "full yellow ink");
    }

    #[test]
    fn orientation_six_turns_the_image_clockwise() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let (out, w, h) = orient(rgba, 2, 1, 6);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&out[..4], &[255, 0, 0, 255], "red on top");
        assert_eq!(&out[4..], &[0, 255, 0, 255], "green below");
    }
}
