//! A rendered story: straight RGBA pixels, read from and written to PNG
//! files, and compared with a golden image.

use std::path::Path;

use sidestep_engine::codec::{self, Encoding};

/// Pixels, `width × height`, straight (not premultiplied) RGBA, row by row
/// from the top left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    /// Premultiplied RGBA pixels, made straight.
    pub fn from_premultiplied(width: u32, height: u32, pixels: impl IntoIterator<Item = [u8; 4]>) -> Picture {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for [r, g, b, a] in pixels {
            let straight = |c: u8| {
                if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 }
            };
            rgba.extend_from_slice(&[straight(r), straight(g), straight(b), a]);
        }
        Picture { width, height, rgba }
    }

    pub fn open(path: &Path) -> Result<Picture, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let image = sidestep_ui::Image::decode(bytes).ok_or_else(|| format!("{}: not an image", path.display()))?;
        let (width, height) = image.pixel_size();
        let rgba = image.to_rgba8().ok_or_else(|| format!("{}: no pixels", path.display()))?;
        Ok(Picture { width, height, rgba })
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let png = codec::encode(Encoding::Png, self.width, self.height, &self.rgba, None)
            .ok_or_else(|| format!("{}: could not encode {}×{} pixels", path.display(), self.width, self.height))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn pixel(&self, i: usize) -> [u8; 4] {
        let p = &self.rgba[i * 4..i * 4 + 4];
        [p[0], p[1], p[2], p[3]]
    }

    /// How this picture differs from `golden`: pixels count as different
    /// when a channel differs by more than `threshold`.
    pub fn compare(&self, golden: &Picture, threshold: u8) -> Comparison {
        if (self.width, self.height) != (golden.width, golden.height) {
            return Comparison::Size { actual: (self.width, self.height), golden: (golden.width, golden.height) };
        }
        let count = self.width as usize * self.height as usize;
        let mut differing = 0;
        let mut worst = 0;
        let mut diff = Vec::with_capacity(count * 4);
        for i in 0..count {
            let (a, g) = (self.pixel(i), golden.pixel(i));
            let delta = (0..4).map(|c| a[c].abs_diff(g[c])).max().unwrap_or(0);
            worst = worst.max(delta);
            if delta > threshold {
                differing += 1;
                diff.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                // The picture, faded, so what differs stands out.
                let fade = |c: u8| 255 - (255 - c) / 4;
                diff.extend_from_slice(&[fade(a[0]), fade(a[1]), fade(a[2]), 255]);
            }
        }
        if differing == 0 {
            return Comparison::Same;
        }
        Comparison::Pixels {
            differing,
            of: count,
            worst,
            diff: Picture { width: self.width, height: self.height, rgba: diff },
        }
    }
}

/// What [`Picture::compare`] found.
#[derive(Debug)]
pub enum Comparison {
    Same,
    Size {
        actual: (u32, u32),
        golden: (u32, u32),
    },
    /// `differing` of `of` pixels differ, by at most `worst` in a channel;
    /// `diff` shows them in red over the picture, faded.
    Pixels {
        differing: usize,
        of: usize,
        worst: u8,
        diff: Picture,
    },
}

impl Comparison {
    /// Whether the difference is within `tolerance`, the fraction of
    /// pixels that may differ.
    pub fn passes(&self, tolerance: f64) -> bool {
        match self {
            Comparison::Same => true,
            Comparison::Size { .. } => false,
            Comparison::Pixels { differing, of, .. } => (*differing as f64) <= tolerance * *of as f64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, px: [u8; 4]) -> Picture {
        Picture { width, height, rgba: px.repeat(width as usize * height as usize) }
    }

    #[test]
    fn premultiplied_pixels_are_made_straight() {
        let p = Picture::from_premultiplied(3, 1, [[128, 0, 64, 128], [0, 0, 0, 0], [255, 255, 255, 255]]);
        assert_eq!(p.rgba, [255, 0, 128, 128, 0, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn comparisons() {
        let a = solid(4, 4, [10, 20, 30, 255]);
        assert!(matches!(a.compare(&a, 0), Comparison::Same));
        assert!(matches!(a.compare(&solid(4, 5, [10, 20, 30, 255]), 0), Comparison::Size { .. }));

        let mut b = a.clone();
        b.rgba[0] = 13;
        assert!(matches!(a.compare(&b, 3), Comparison::Same), "within the threshold");
        let c = a.compare(&b, 2);
        let Comparison::Pixels { differing, of, worst, ref diff } = c else { panic!("{c:?}") };
        assert_eq!((differing, of, worst), (1, 16, 3));
        assert_eq!(&diff.rgba[..4], [255, 0, 0, 255]);
        assert!(!c.passes(0.0));
        assert!(c.passes(1.0 / 16.0));
    }

    #[test]
    fn pictures_survive_a_png_file() {
        let dir = std::env::temp_dir().join(format!("sidestep-stories-{}", std::process::id()));
        let path = dir.join("p.png");
        let p = Picture::from_premultiplied(2, 2, [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0, 0, 0, 0]]);
        p.save(&path).unwrap();
        assert_eq!(Picture::open(&path).unwrap(), p);
        let _ = std::fs::remove_dir_all(dir);
    }
}
