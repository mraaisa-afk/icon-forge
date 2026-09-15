//! §3.3-① Normalize — decode, EXIF orientation, RGBA8, max-dimension cap.
//!
//! Determinism contract: same input bytes → byte-identical [`SheetRaster`]
//! (fixed Triangle resample, no ambient configuration).

use image::{DynamicImage, RgbaImage};

use crate::IsgError;
use super::raster::SheetRaster;

/// Decodes an encoded sheet (PNG/JPEG), applies EXIF orientation, converts
/// to RGBA8 and caps the larger dimension at `max_dim` (fixed Triangle
/// filter). Luma is materialized for the frozen [`isg_core::RasterView`]
/// seam.
pub fn normalize(bytes: &[u8], max_dim: u32) -> Result<SheetRaster, IsgError> {
    let img: DynamicImage =
        image::load_from_memory(bytes).map_err(|e| IsgError::Corrupt(format!("decode: {e}")))?;
    let mut rgba = img.to_rgba8();
    if let Some(o) = exif_orientation(bytes) {
        apply_orientation(&mut rgba, o);
    }
    let (w, h) = rgba.dimensions();
    if w.max(h) > max_dim {
        let s = f64::from(max_dim) / f64::from(w.max(h));
        let nw = ((f64::from(w) * s).round() as u32).max(1);
        let nh = ((f64::from(h) * s).round() as u32).max(1);
        rgba = image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Triangle);
    }
    let (w, h) = rgba.dimensions();
    Ok(SheetRaster::from_rgba(w, h, rgba.into_raw()))
}

/// EXIF orientation value (1–8) from a JPEG byte stream, if present.
///
/// Hand-rolled APP1/IFD0 scan (tag 274): avoids a new dependency for one
/// integer. PNG sheets (no EXIF in practice) return `None` immediately.
#[must_use]
pub fn exif_orientation(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None; // not a JPEG
    }
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            break; // desynced stream — give up, treat as unoriented
        }
        let marker = bytes[i + 1];
        if marker == 0xFF || (0xD0..=0xD9).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA {
            break; // start of scan — EXIF must precede it
        }
        let seg_len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if seg_len < 2 {
            break;
        }
        let seg = bytes.get(i + 4..i + 2 + seg_len)?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            return parse_tiff_orientation(&seg[6..]).filter(|o| (1..=8).contains(o));
        }
        i += 2 + seg_len;
    }
    None
}

/// Reads the orientation tag (274) out of the TIFF block inside an APP1
/// payload (the bytes after `Exif\0\0`).
fn parse_tiff_orientation(tiff: &[u8]) -> Option<u16> {
    let le = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let rd16 = |o: usize| -> Option<u16> {
        let b = tiff.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let rd32 = |o: usize| -> Option<u32> {
        let b = tiff.get(o..o + 4)?;
        Some(if le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    if rd16(2)? != 42 {
        return None;
    }
    let ifd0 = rd32(4)? as usize;
    let count = rd16(ifd0)? as usize;
    for e in 0..count {
        let base = ifd0 + 2 + 12 * e;
        if rd16(base)? == 274 {
            // SHORT inline: first two bytes of the value field.
            return rd16(base + 8);
        }
    }
    None
}

/// Applies EXIF orientation 2–8 (1 is a no-op and never passed).
///
/// Out-of-place `imageops` primitives (rotate90 = 90° clockwise); the
/// copies are irrelevant next to decode cost and keep the transforms
/// obviously correct.
pub fn apply_orientation(img: &mut RgbaImage, orientation: u16) {
    match orientation {
        2 => *img = image::imageops::flip_horizontal(img),
        3 => *img = image::imageops::rotate180(img),
        4 => *img = image::imageops::flip_vertical(img),
        5 => {
            // transpose: 90° CW, then mirror horizontally
            *img = image::imageops::flip_horizontal(&image::imageops::rotate90(img));
        }
        6 => *img = image::imageops::rotate90(img),
        7 => {
            // transverse: 90° CW, then mirror vertically
            *img = image::imageops::flip_vertical(&image::imageops::rotate90(img));
        }
        8 => *img = image::imageops::rotate270(img),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use image::{ExtendedColorType, ImageEncoder};
    use super::*;

    /// Encodes RGBA pixels to an in-memory PNG (the pipeline itself only
    /// *decodes*; encoding is test scaffolding).
    fn png_bytes(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(px, w, h, ExtendedColorType::Rgba8)
            .expect("png encode");
        png
    }

    #[test]
    fn normalizes_png_to_rgba8_luma() {
        // 6 pixels (3×2): red levels 1..=6, opaque.
        let px: Vec<u8> = (1u8..=6).flat_map(|v| [v, 0, 0, 255]).collect();
        let bytes = png_bytes(3, 2, &px);
        let r = normalize(&bytes, 4096).unwrap();
        assert_eq!((r.width(), r.height()), (3, 2));
        assert_eq!(r.pixel(0, 0), [1, 0, 0, 255]);
        assert_eq!(r.pixel(2, 1), [6, 0, 0, 255]);
    }

    #[test]
    fn normalize_is_deterministic() {
        let bytes = png_bytes(32, 16, &vec![7u8; 32 * 16 * 4]);
        let a = normalize(&bytes, 4096).unwrap();
        let b = normalize(&bytes, 4096).unwrap();
        assert_eq!(a.rgba(), b.rgba());
        assert_eq!(a.luma(), b.luma());
    }

    #[test]
    fn caps_max_dimension_with_triangle_filter() {
        let bytes = png_bytes(64, 32, &vec![200u8; 64 * 32 * 4]);
        let r = normalize(&bytes, 16).unwrap();
        assert_eq!(r.width(), 16);
        assert_eq!(r.height(), 8, "aspect preserved");
    }

    #[test]
    fn exif_parser_reads_le_and_be_tiff_blocks() {
        // II (LE): header + one IFD entry {tag 274, type SHORT(3), count 1,
        // value 6 inline}.
        let le = {
            let mut t = Vec::new();
            t.extend_from_slice(b"II");
            t.extend_from_slice(&42u16.to_le_bytes());
            t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
            t.extend_from_slice(&1u16.to_le_bytes()); // entry count
            t.extend_from_slice(&274u16.to_le_bytes());
            t.extend_from_slice(&3u16.to_le_bytes());
            t.extend_from_slice(&1u32.to_le_bytes());
            t.extend_from_slice(&6u16.to_le_bytes());
            t.extend_from_slice(&0u16.to_le_bytes());
            t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
            let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
            jpeg.extend_from_slice(&((t.len() as u16 + 8).to_be_bytes()));
            jpeg.extend_from_slice(b"Exif\0\0");
            jpeg.extend_from_slice(&t);
            jpeg
        };
        assert_eq!(exif_orientation(&le), Some(6));
        assert_eq!(parse_tiff_orientation(&le[12..]), Some(6));

        // MM (BE): value 8 stored big-endian in the value field.
        let mut t = Vec::new();
        t.extend_from_slice(b"MM");
        t.extend_from_slice(&42u16.to_be_bytes());
        t.extend_from_slice(&8u32.to_be_bytes());
        t.extend_from_slice(&1u16.to_be_bytes());
        t.extend_from_slice(&274u16.to_be_bytes());
        t.extend_from_slice(&3u16.to_be_bytes());
        t.extend_from_slice(&1u32.to_be_bytes());
        t.extend_from_slice(&8u16.to_be_bytes());
        t.extend_from_slice(&0u16.to_be_bytes());
        t.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(parse_tiff_orientation(&t), Some(8));
    }

    #[test]
    fn exif_returns_none_for_png_and_orientationless_jpeg() {
        assert_eq!(exif_orientation(&png_bytes(2, 2, &vec![0; 16])), None);
        let bare = [0xFF, 0xD8, 0xFF, 0xD9];
        assert_eq!(exif_orientation(&bare), None);
    }

    /// 3×2 image: `A B C / D E F` as distinct red-level pixels.
    fn grid() -> RgbaImage {
        let px: Vec<u8> = (1u8..=6).flat_map(|v| [v, 0, 0, 255]).collect();
        RgbaImage::from_raw(3, 2, px).unwrap()
    }

    fn reds(img: &RgbaImage) -> Vec<u8> {
        img.pixels().map(|p| p[0]).collect()
    }

    #[test]
    fn orientation_transforms_match_spec() {
        let mut g = grid();
        apply_orientation(&mut g, 2); // mirror horizontally
        assert_eq!(reds(&g), vec![3, 2, 1, 6, 5, 4]);

        let mut g = grid();
        apply_orientation(&mut g, 3); // 180°
        assert_eq!(reds(&g), vec![6, 5, 4, 3, 2, 1]);

        let mut g = grid();
        apply_orientation(&mut g, 4); // mirror vertically
        assert_eq!(reds(&g), vec![4, 5, 6, 1, 2, 3]);

        let mut g = grid();
        apply_orientation(&mut g, 5); // transpose
        assert_eq!(reds(&g), vec![1, 4, 2, 5, 3, 6]);

        let mut g = grid();
        apply_orientation(&mut g, 6); // 90° CW
        assert_eq!(reds(&g), vec![4, 1, 5, 2, 6, 3]);

        let mut g = grid();
        apply_orientation(&mut g, 7); // transverse (anti-diagonal)
        assert_eq!(reds(&g), vec![6, 3, 5, 2, 4, 1]);

        let mut g = grid();
        apply_orientation(&mut g, 8); // 90° CCW
        assert_eq!(reds(&g), vec![3, 6, 2, 5, 1, 4]);
    }
}
