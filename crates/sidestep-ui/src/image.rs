//! Images: pixels a program made, or a file the render thread decodes the
//! first time the image is drawn, so decoding happens off the main thread.
//! The render thread keeps what it decoded (and smaller copies it made for
//! drawing small) until the last clone of the image is dropped.

use std::sync::Arc;

use kurbo::Size;
use sidestep_engine::codec;
use sidestep_engine::protocol::ToRender;
use sidestep_engine::raster::images::{ImageData, Pixels, next_key};

/// An image: cheap to clone, and shareable between threads.
#[derive(Clone)]
pub struct Image {
    pixels: Arc<Shared>,
    /// Points per pixel, as the file's density or the program said.
    scale: f64,
}

/// The pixels, which the render thread caches by their key until the last
/// image showing them goes.
struct Shared(Arc<ImageData>);

impl Drop for Shared {
    fn drop(&mut self) {
        sidestep_engine::backend::send_if_running(ToRender::ForgetImages { keys: vec![self.0.key] });
    }
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image").field("pixels", &self.pixel_size()).field("scale", &self.scale).finish()
    }
}

impl Image {
    /// An image of straight (not premultiplied) RGBA pixels, rows
    /// `width × 4` bytes apart, top row first. `None` if there aren't
    /// `width × height` of them.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Image> {
        let len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
        if width == 0 || height == 0 || rgba.len() != len {
            return None;
        }
        let premultiplied: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = u16::from(p[3]);
                let m = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
                [m(p[0]), m(p[1]), m(p[2]), p[3]]
            })
            .collect();
        Some(Image::of(width, height, Pixels::Rgba(Arc::from(premultiplied)), 1.0))
    }

    /// An image file's (PNG, JPEG, GIF, WebP, BMP, TIFF or ICO), turned
    /// upright by its orientation, at its density (72 dots per inch is a
    /// point per pixel). Only its header is read now; `None` if the codecs
    /// don't read it.
    pub fn decode(bytes: impl Into<Arc<[u8]>>) -> Option<Image> {
        let bytes: Arc<[u8]> = bytes.into();
        let header = codec::header(&bytes)?;
        let (mut width, mut height) = (header.width, header.height);
        if header.orientation.swaps() {
            std::mem::swap(&mut width, &mut height);
        }
        let scale = (72.0 / header.dpi.0).clamp(1.0 / 16.0, 16.0);
        Some(Image::of(width, height, Pixels::Encoded(bytes, true), scale))
    }

    /// Read and [`decode`](Image::decode) a file.
    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Image> {
        let bytes = std::fs::read(path)?;
        Image::decode(bytes).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not an image"))
    }

    fn of(width: u32, height: u32, pixels: Pixels, scale: f64) -> Image {
        let data = Arc::new(ImageData { key: next_key(), generation: 0, width, height, pixels });
        Image::from_data(data, scale)
    }

    /// Pixels the engine made (a key of their own), at `scale` points per
    /// pixel.
    pub(crate) fn from_data(data: Arc<ImageData>, scale: f64) -> Image {
        Image { pixels: Arc::new(Shared(data)), scale }
    }

    /// The same pixels at `scale` points per pixel: 0.5 draws an image made
    /// for a 2× screen at its size in points.
    pub fn with_scale(&self, scale: f64) -> Image {
        Image { pixels: self.pixels.clone(), scale: scale.max(f64::MIN_POSITIVE) }
    }

    /// Its size in pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.pixels.0.width, self.pixels.0.height)
    }

    /// Its size in points.
    pub fn size(&self) -> Size {
        let (w, h) = self.pixel_size();
        Size::new(f64::from(w) * self.scale, f64::from(h) * self.scale)
    }

    pub(crate) fn data(&self) -> &Arc<ImageData> {
        &self.pixels.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_are_premultiplied_once() {
        let image = Image::from_rgba(1, 1, &[200, 100, 50, 128]).expect("an image");
        let Pixels::Rgba(px) = &image.data().pixels else { panic!("pixels") };
        assert_eq!(&px[..], [100, 50, 25, 128]);
        assert_eq!(image.size(), Size::new(1.0, 1.0));
        assert!(Image::from_rgba(2, 1, &[0; 4]).is_none());
        assert!(Image::from_rgba(0, 0, &[]).is_none());
    }

    #[test]
    fn files_are_read_by_their_header() {
        assert!(Image::decode(&b"not an image"[..]).is_none());
        let png = codec::encode(codec::Encoding::Png, 3, 2, &[255; 24], None).expect("a PNG");
        let image = Image::decode(png).expect("decoded");
        assert_eq!(image.pixel_size(), (3, 2));
        assert_eq!(image.with_scale(0.5).size(), Size::new(1.5, 1.0));
    }
}
