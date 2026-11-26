use std::io;

use clipserve::capture::decode_png;

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
