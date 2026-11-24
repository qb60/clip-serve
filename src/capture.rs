use std::{
    borrow::Cow,
    io,
    net::SocketAddrV4,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use image::{ExtendedColorType, ImageEncoder, codecs::png};

use crate::store::StoredImage;

#[derive(Clone, Copy, Debug)]
pub struct Capture {
    generation: u64,
    pub sequence: u32,
}

pub struct CaptureState {
    pub enabled: bool,
    baseline: u32,
    generation: u64,
    server: Option<(SocketAddrV4, Arc<AtomicBool>)>,
}

impl CaptureState {
    pub fn new(enabled: bool, sequence: u32) -> Self {
        Self {
            enabled,
            baseline: sequence,
            generation: 0,
            server: None,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool, sequence: u32) {
        self.enabled = enabled;
        self.baseline = sequence;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_server(&mut self, server: Option<(SocketAddrV4, Arc<AtomicBool>)>, sequence: u32) {
        self.server = server;
        self.set_enabled(self.enabled, sequence);
    }

    pub fn endpoint(&self) -> Option<SocketAddrV4> {
        let (endpoint, healthy) = self.server.as_ref()?;
        healthy.load(Ordering::Acquire).then_some(*endpoint)
    }

    pub fn record_write(&mut self, sequence: u32) {
        self.baseline = sequence;
    }

    pub fn observe(&mut self, sequence: u32, own_clipboard: bool) -> Option<Capture> {
        if !self.enabled || self.endpoint().is_none() || sequence == self.baseline || own_clipboard
        {
            return None;
        }
        self.baseline = sequence;
        Some(Capture {
            generation: self.generation,
            sequence,
        })
    }

    pub fn validate(&self, job: Capture, sequence: u32) -> Option<SocketAddrV4> {
        if self.enabled && self.generation == job.generation && job.sequence == sequence {
            self.endpoint()
        } else {
            None
        }
    }
}

pub fn rgba_len(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

pub fn encode_png(width: usize, height: usize, rgba: &[u8]) -> io::Result<Arc<[u8]>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid clipboard image dimensions.",
        )
    };
    let (width, height) = (
        u32::try_from(width).map_err(|_| invalid())?,
        u32::try_from(height).map_err(|_| invalid())?,
    );
    if rgba_len(width, height) != Some(rgba.len()) {
        return Err(invalid());
    }
    let mut data = Vec::new();
    png::PngEncoder::new(&mut data)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(io::Error::other)?;
    Ok(data.into())
}

pub fn decode_png(png: &[u8]) -> io::Result<image::RgbaImage> {
    let decoder = png::PngDecoder::new(std::io::Cursor::new(png)).map_err(io::Error::other)?;
    Ok(image::DynamicImage::from_decoder(decoder)
        .map_err(io::Error::other)?
        .into_rgba8())
}

pub fn preview_png(png: &[u8]) -> io::Result<image::RgbaImage> {
    const MAX_SIDE: u32 = 2048;
    let pixels = decode_png(png)?;
    if pixels.width().max(pixels.height()) <= MAX_SIDE {
        return Ok(pixels);
    }
    Ok(image::DynamicImage::ImageRgba8(pixels)
        .thumbnail(MAX_SIDE, MAX_SIDE)
        .into_rgba8())
}

pub fn prepare_image(width: usize, height: usize, rgba: Cow<'_, [u8]>) -> io::Result<StoredImage> {
    let png = encode_png(width, height, &rgba)?;
    let (width, height) = (width as u32, height as u32);
    // Encoding has already validated the dimensions and buffer length.
    let pixels = image::RgbaImage::from_raw(width, height, rgba.into_owned()).unwrap();
    let thumbnail = image::DynamicImage::ImageRgba8(pixels)
        .thumbnail(width.min(136), height.min(96))
        .into_rgba8();
    Ok(StoredImage {
        png,
        thumbnail: Arc::new(thumbnail),
    })
}

pub fn dibv5(png: &[u8]) -> io::Result<Vec<u8>> {
    const HEADER: usize = 124;
    let pixels = decode_png(png)?;
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Image dimensions exceed the Windows bitmap format.",
        )
    };
    let width = i32::try_from(pixels.width()).map_err(|_| invalid())?;
    let height = i32::try_from(pixels.height()).map_err(|_| invalid())?;
    let len = pixels.as_raw().len();
    let size = u32::try_from(len).map_err(|_| invalid())?;
    let mut dib = Vec::new();
    dib.try_reserve_exact(HEADER.checked_add(len).ok_or_else(invalid)?)
        .map_err(io::Error::other)?;
    dib.resize(HEADER, 0);
    dib[12..14].copy_from_slice(&1_u16.to_le_bytes()); // Planes
    dib[14..16].copy_from_slice(&32_u16.to_le_bytes()); // Bits per pixel
    for (offset, value) in [
        (0, HEADER as u32),
        (4, width as u32),
        (8, (-height) as u32), // Top-down rows
        (16, 3),               // BI_BITFIELDS
        (20, size),
        (40, 0x00ff0000), // Red mask
        (44, 0x0000ff00), // Green mask
        (48, 0x000000ff), // Blue mask
        (52, 0xff000000), // Alpha mask
        (56, 0x73524742), // LCS_sRGB
        (108, 4),         // LCS_GM_IMAGES
    ] {
        dib[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for rgba in pixels.as_raw().as_chunks::<4>().0 {
        dib.extend_from_slice(&[rgba[2], rgba[1], rgba[0], rgba[3]]);
    }
    Ok(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_toggle_restore_and_failure_state_transitions() {
        let mut state = CaptureState::new(false, 40);
        let endpoint = "127.0.0.1:8888".parse().unwrap();
        let health = Arc::new(AtomicBool::new(true));
        state.set_server(Some((endpoint, Arc::clone(&health))), 40);
        assert!(state.observe(41, false).is_none());
        state.set_enabled(true, 41);
        assert!(state.observe(41, false).is_none());
        let first = state.observe(42, false).unwrap();
        assert!(state.observe(42, false).is_none());
        assert_eq!(state.validate(first, 42), Some(endpoint));
        assert_eq!(state.validate(first, 43), None);

        state.record_write(43);
        assert!(state.observe(43, true).is_none());
        assert!(state.observe(43, false).is_none());
        assert!(state.observe(44, true).is_none());
        assert!(state.enabled);
        let next = state.observe(44, false).unwrap();
        health.store(false, Ordering::Release);
        assert_eq!(state.validate(next, 44), None);
        assert!(state.observe(45, false).is_none());

        health.store(true, Ordering::Release);
        state.set_enabled(false, 44);
        state.record_write(45);
        assert!(!state.enabled);
        state.set_enabled(true, 45);
        assert_eq!(state.validate(next, 44), None);
        assert!(state.observe(45, true).is_none());
        assert!(state.observe(46, false).is_some());
    }

    #[test]
    fn png_round_trip_and_bad_image() {
        let pixels = [1, 2, 3, 4, 255, 128, 200, 0];
        let png = encode_png(2, 1, &pixels).unwrap();
        assert_eq!(decode_png(&png).unwrap().as_raw(), &pixels);
        let dib = dibv5(&png).unwrap();
        assert_eq!(&dib[8..12], &(-1_i32).to_le_bytes());
        assert_eq!(&dib[124..], &[3, 2, 1, 4, 200, 128, 255, 0]);
        assert!(encode_png(2, 2, &pixels).is_err());
        assert!(encode_png(0, 0, &[]).is_err());
        assert!(encode_png(usize::MAX, usize::MAX, &[]).is_err());
        assert!(decode_png(b"not a PNG").is_err());
    }

    #[test]
    fn thumbnails_preserve_aspect_ratio_and_leave_original_png_at_full_size() {
        let pixels = vec![255; 400 * 200 * 4];
        let stored = prepare_image(400, 200, Cow::Owned(pixels)).unwrap();
        assert_eq!(stored.thumbnail.dimensions(), (136, 68));
        assert_eq!(decode_png(&stored.png).unwrap().dimensions(), (400, 200));
        let small = prepare_image(1, 2, Cow::Borrowed(&[255; 8])).unwrap();
        assert_eq!(small.thumbnail.dimensions(), (1, 2));
        assert!(prepare_image(1, 2, Cow::Borrowed(&[255; 4])).is_err());
    }

    #[test]
    fn preview_uses_original_pixels_and_caps_long_side() {
        let small = encode_png(1, 1, &[0, 1, 2, 255]).unwrap();
        assert_eq!(preview_png(&small).unwrap().as_raw(), &[0, 1, 2, 255]);

        let large = encode_png(4096, 64, &vec![0; 4096 * 64 * 4]).unwrap();
        assert_eq!(preview_png(&large).unwrap().dimensions(), (2048, 32));
        assert!(preview_png(b"not a PNG").is_err());
    }
}
