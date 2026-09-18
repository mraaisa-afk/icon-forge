//! A PNG writer and reader, in this crate, on purpose.
//!
//! Icon sheets need a raster export, and a sheet is about the least
//! PNG-hostile image there is: flat colours, large areas, no photographs. What
//! this writer produces is a **valid, deterministic PNG** — 8-bit RGBA, no
//! interlacing, a filter byte of zero on every scanline — whose pixel data is
//! carried in deflate's *stored* blocks. Stored blocks cost size (≈ 4 bytes per
//! pixel plus 5 bytes per 64 KiB) and buy two things that matter more here:
//! the encoder is 100 bytes of arithmetic with no dependency and no version
//! skew, and the output is byte-identical across platforms and toolchains, the
//! same property the cache and the emitters are built around (§3.3).
//!
//! Writing the format also means *reading* it: [`parse_png`] is the reader the
//! tests use to prove the round trip, which keeps the evidence for "the export
//! is a real PNG" inside the crate rather than in a comment.
//!
//! Everything here is pure `std`. The rasterizer that produces the pixels lives
//! next door (`isg-native::sheet_native`, which owns the resvg render); this
//! module only turns a pixel buffer into bytes and back.

/// The eight-byte signature every PNG starts with.
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Colour type 6: truecolour with alpha.
const COLOR_TYPE_RGBA: u8 = 6;
/// Larger stored blocks are not allowed by the deflate format.
const MAX_STORED_BLOCK: usize = 65_535;

/// Why an encode or decode refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PngError {
    /// Width or height was zero.
    EmptyImage,
    /// The buffer does not hold `4 · width · height` bytes.
    SizeMismatch {
        /// What the caller declared.
        expected: usize,
        /// What the buffer holds.
        got: usize,
    },
    /// The width or height does not fit a 32-bit IHDR field.
    TooLarge,
    /// The bytes are not a PNG this reader understands.
    Malformed(String),
}

impl std::fmt::Display for PngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyImage => f.write_str("image has no area"),
            Self::SizeMismatch { expected, got } => {
                write!(f, "expected {expected} bytes of RGBA, got {got}")
            }
            Self::TooLarge => f.write_str("image is too large for a PNG"),
            Self::Malformed(why) => write!(f, "malformed PNG: {why}"),
        }
    }
}

impl std::error::Error for PngError {}

/// What [`parse_png`] found in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngInfo {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bit depth (always 8 here).
    pub bit_depth: u8,
    /// Colour type (6 = RGBA).
    pub color_type: u8,
    /// Interlace method (0 = none).
    pub interlace: u8,
    /// Bytes of `IDAT` payload.
    pub idat_bytes: usize,
    /// The image's pixels, decoded (straight RGBA, `4 · w · h` bytes).
    pub pixels: Vec<u8>,
}

/// Encodes straight (non-premultiplied) 8-bit RGBA pixels as a PNG.
///
/// # Errors
///
/// [`PngError::EmptyImage`] for a zero dimension,
/// [`PngError::SizeMismatch`] when the buffer is not `4 · w · h` bytes, and
/// [`PngError::TooLarge`] past the format's 2³¹−1 pixel limit.
pub fn write_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PngError> {
    if width == 0 || height == 0 {
        return Err(PngError::EmptyImage);
    }
    let expected = 4 * width as usize * height as usize;
    if rgba.len() != expected {
        return Err(PngError::SizeMismatch {
            expected,
            got: rgba.len(),
        });
    }
    if width > 0x7fff_ffff || height > 0x7fff_ffff {
        return Err(PngError::TooLarge);
    }

    // Raw scanlines: one filter byte (0, "none") then the pixels. Nothing is
    // filtered because nothing is compressed — a filter would only add bytes.
    let stride = 4 * width as usize;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    let mut out = Vec::with_capacity(raw.len() + raw.len() / MAX_STORED_BLOCK * 5 + 128);
    out.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(COLOR_TYPE_RGBA);
    ihdr.push(0); // compression: deflate
    ihdr.push(0); // filter method: adaptive
    ihdr.push(0); // interlace: none
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Wraps `data` in a zlib stream built from deflate's stored blocks.
#[must_use]
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / MAX_STORED_BLOCK * 5 + 16);
    // CMF: deflate, 32 KiB window. FLG: no preset dictionary, no compression
    // level, and the two-byte header's checksum is its own `% 31` remainder.
    out.push(0x78);
    out.push(0x01);
    if data.is_empty() {
        // An empty deflate stream is one final empty stored block.
        out.push(0x01);
        out.extend_from_slice(&[0, 0, 0xff, 0xff]);
    }
    let mut offset = 0;
    while offset < data.len() {
        let take = MAX_STORED_BLOCK.min(data.len() - offset);
        let last = offset + take == data.len();
        out.push(u8::from(last)); // BFINAL, BTYPE = 00
        let len = take as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + take]);
        offset += take;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// zlib's Adler-32 over `data`.
#[must_use]
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    // 5 552 is the largest run that cannot overflow a u32 accumulator.
    for chunk in data.chunks(5_552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// CRC-32 (the PNG/zlib polynomial) over `data`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Appends one chunk: length, type, payload, CRC over type + payload.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc_input = Vec::with_capacity(4 + payload.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Reads a PNG back: header, chunks, CRCs, the stored deflate stream, and the
/// scanlines. Strict on purpose — this is the crate's own evidence that what it
/// writes is a real PNG, so it refuses what a lenient reader would guess at.
///
/// # Errors
///
/// [`PngError::Malformed`] with the byte offset (or the chunk name) where the
/// file stopped making sense.
pub fn parse_png(bytes: &[u8]) -> Result<PngInfo, PngError> {
    if bytes.len() < PNG_SIGNATURE.len() || bytes[..8] != PNG_SIGNATURE {
        return Err(PngError::Malformed("bad signature".to_string()));
    }
    let mut cursor = 8usize;
    let mut header: Option<(u32, u32, u8, u8, u8, u8)> = None;
    let mut idat: Vec<u8> = Vec::new();
    let mut saw_end = false;
    while cursor + 8 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]) as usize;
        let kind = &bytes[cursor + 4..cursor + 8];
        let payload_at = cursor + 8;
        let payload_end = payload_at
            .checked_add(length)
            .ok_or_else(|| PngError::Malformed("chunk length overflows".to_string()))?;
        if payload_end + 4 > bytes.len() {
            return Err(PngError::Malformed(format!(
                "chunk {} runs past the end at byte {cursor}",
                String::from_utf8_lossy(kind)
            )));
        }
        let mut crc_input = Vec::with_capacity(4 + length);
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(&bytes[payload_at..payload_end]);
        let declared = u32::from_be_bytes([
            bytes[payload_end],
            bytes[payload_end + 1],
            bytes[payload_end + 2],
            bytes[payload_end + 3],
        ]);
        let actual = crc32(&crc_input);
        if declared != actual {
            return Err(PngError::Malformed(format!(
                "chunk {} CRC {declared:08x} but the payload hashes to {actual:08x}",
                String::from_utf8_lossy(kind)
            )));
        }
        let payload = &bytes[payload_at..payload_end];
        match kind {
            b"IHDR" => {
                if payload.len() != 13 {
                    return Err(PngError::Malformed("IHDR is not 13 bytes".to_string()));
                }
                let width = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let height = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
                if width == 0 || height == 0 {
                    return Err(PngError::Malformed("zero-sized image".to_string()));
                }
                header = Some((
                    width,
                    height,
                    payload[8],
                    payload[9],
                    payload[10],
                    payload[12],
                ));
            }
            b"IDAT" => idat.extend_from_slice(payload),
            b"IEND" => {
                saw_end = true;
                break;
            }
            _ => {}
        }
        cursor = payload_end + 4;
    }
    if !saw_end {
        return Err(PngError::Malformed("no IEND".to_string()));
    }
    let (width, height, bit_depth, color_type, _compression, interlace) =
        header.ok_or_else(|| PngError::Malformed("no IHDR".to_string()))?;
    if bit_depth != 8 || color_type != COLOR_TYPE_RGBA || interlace != 0 {
        return Err(PngError::Malformed(format!(
            "unsupported format: depth {bit_depth}, colour type {color_type}, interlace {interlace}"
        )));
    }
    let raw = zlib_inflate_stored(&idat)?;
    let stride = 4 * width as usize;
    let expected = (stride + 1) * height as usize;
    if raw.len() != expected {
        return Err(PngError::Malformed(format!(
            "pixel data is {} bytes, expected {expected}",
            raw.len()
        )));
    }
    let mut pixels = Vec::with_capacity(stride * height as usize);
    for y in 0..height as usize {
        let line = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        if line[0] != 0 {
            return Err(PngError::Malformed(format!(
                "scanline {y} uses filter {}, which this writer never emits",
                line[0]
            )));
        }
        pixels.extend_from_slice(&line[1..]);
    }
    Ok(PngInfo {
        width,
        height,
        bit_depth,
        color_type,
        interlace,
        idat_bytes: idat.len(),
        pixels,
    })
}

/// Inflates a zlib stream made only of stored blocks (the writer's own output).
fn zlib_inflate_stored(idat: &[u8]) -> Result<Vec<u8>, PngError> {
    if idat.len() < 6 {
        return Err(PngError::Malformed("zlib stream is too short".to_string()));
    }
    let cmf = idat[0];
    let flg = idat[1];
    if cmf & 0x0f != 8 {
        return Err(PngError::Malformed("not a deflate stream".to_string()));
    }
    if (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
        return Err(PngError::Malformed("zlib header checksum".to_string()));
    }
    let (mut cursor, end) = (2usize, idat.len() - 4);
    let mut out = Vec::new();
    loop {
        if cursor >= end {
            return Err(PngError::Malformed("deflate stream ends early".to_string()));
        }
        let head = idat[cursor];
        cursor += 1;
        if head & 0x06 != 0 {
            return Err(PngError::Malformed(
                "the stream is compressed, not stored".to_string(),
            ));
        }
        if cursor + 4 > end {
            return Err(PngError::Malformed(
                "stored block header is short".to_string(),
            ));
        }
        let len = u16::from_le_bytes([idat[cursor], idat[cursor + 1]]);
        let nlen = u16::from_le_bytes([idat[cursor + 2], idat[cursor + 3]]);
        cursor += 4;
        if len != !nlen {
            return Err(PngError::Malformed("stored block length check".to_string()));
        }
        let take = len as usize;
        if cursor + take > end {
            return Err(PngError::Malformed(
                "stored block runs past the stream".to_string(),
            ));
        }
        out.extend_from_slice(&idat[cursor..cursor + take]);
        cursor += take;
        if head & 1 == 1 {
            break;
        }
    }
    let declared = u32::from_be_bytes([idat[end], idat[end + 1], idat[end + 2], idat[end + 3]]);
    if declared != adler32(&out) {
        return Err(PngError::Malformed("Adler-32 mismatch".to_string()));
    }
    Ok(out)
}

/// Converts premultiplied RGBA (what `tiny-skia` renders) into straight RGBA
/// (what a PNG stores).
#[must_use]
pub fn unpremultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        let a = u16::from(pixel[3]);
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if a == 255 {
            out.extend_from_slice(pixel);
            continue;
        }
        // Round-to-nearest division by the alpha: the inverse of the multiply
        // the renderer did, and stable for the flat colours a sheet contains.
        let scale = |v: u8| -> u8 { (((u16::from(v) * 255) + a / 2) / a).min(255) as u8 };
        out.extend_from_slice(&[scale(pixel[0]), scale(pixel[1]), scale(pixel[2]), pixel[3]]);
    }
    out
}

/// Composites straight RGBA over an opaque background, returning RGB triples.
///
/// Used when a sheet is exported with a background colour: a JPEG or a print
/// viewer will not honour alpha, and guessing at white is the caller's call.
#[must_use]
pub fn composite_over_rgb(rgba: &[u8], background: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        let a = u32::from(pixel[3]);
        let mix = |src: u8, bg: u8| -> u8 {
            (((u32::from(src) * a) + (u32::from(bg) * (255 - a)) + 127) / 255) as u8
        };
        out.extend_from_slice(&[
            mix(pixel[0], background[0]),
            mix(pixel[1], background[1]),
            mix(pixel[2], background[2]),
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkerboard(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(4 * w as usize * h as usize);
        for y in 0..h {
            for x in 0..w {
                let on = (x + y) % 2 == 0;
                pixels.extend_from_slice(&if on { [10, 20, 30, 255] } else { [0, 0, 0, 0] });
            }
        }
        pixels
    }

    #[test]
    fn the_two_checksums_match_their_published_vectors() {
        // zlib's own example: Adler-32("abc").
        assert_eq!(adler32(b"abc"), 0x024d_0127);
        assert_eq!(adler32(b""), 1);
        // The CRC-32 check value.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn a_written_png_parses_back_to_the_same_pixels() {
        let pixels = checkerboard(7, 5);
        let png = write_rgba_png(7, 5, &pixels).expect("writes");
        assert_eq!(&png[..8], &PNG_SIGNATURE);
        let info = parse_png(&png).expect("parses");
        assert_eq!((info.width, info.height), (7, 5));
        assert_eq!(info.bit_depth, 8);
        assert_eq!(info.color_type, 6);
        assert_eq!(info.interlace, 0);
        assert_eq!(info.pixels, pixels);
        assert!(info.idat_bytes > 0);
    }

    #[test]
    fn the_chunk_layout_is_the_one_a_reader_expects() {
        let png = write_rgba_png(1, 1, &[1, 2, 3, 4]).expect("writes");
        // IHDR first, IEND last, and every chunk length is declared.
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // The IHDR's 13 bytes carry width, height, depth 8, colour type 6,
        // compression 0, filter 0, interlace 0.
        let ihdr = &png[16..29];
        assert_eq!(&ihdr[0..4], &1u32.to_be_bytes());
        assert_eq!(&ihdr[4..8], &1u32.to_be_bytes());
        assert_eq!(&ihdr[8..13], &[8, 6, 0, 0, 0]);
    }

    #[test]
    fn a_single_pixel_png_is_22_bytes_of_chunks_plus_its_pixels() {
        // 1 × 1 RGBA: raw = filter + 4 bytes = 5 bytes, in one stored block.
        let png = write_rgba_png(1, 1, &[9, 8, 7, 6]).expect("writes");
        let info = parse_png(&png).expect("parses");
        assert_eq!(info.pixels, vec![9, 8, 7, 6]);
        // Signature + IHDR (25) + IDAT (12 + 2 + 5 + 5 + 4) + IEND (12).
        assert_eq!(png.len(), 8 + 25 + 12 + 16 + 12);
    }

    #[test]
    fn a_tall_image_spans_several_stored_blocks() {
        // 128 × 128 RGBA is 65 664 raw bytes: two stored blocks (the first one
        // full at 65 535, the second carrying the remainder), so the BFINAL bit
        // and the LEN/NLEN fields are both exercised.
        let pixels = vec![200u8; 4 * 128 * 128];
        let png = write_rgba_png(128, 128, &pixels).expect("writes");
        let info = parse_png(&png).expect("parses");
        assert_eq!(info.pixels.len(), pixels.len());
        assert_eq!(info.pixels[0], 200);
        assert_eq!(info.pixels[pixels.len() - 1], 200);
        assert!(info.idat_bytes > 65_535);
    }

    #[test]
    fn a_bad_crc_is_refused_rather_than_decoded() {
        let mut png = write_rgba_png(2, 2, &checkerboard(2, 2)).expect("writes");
        // Flip a bit in the IDAT payload.
        let at = png.len() / 2;
        png[at] ^= 0xff;
        let err = parse_png(&png).unwrap_err();
        assert!(matches!(err, PngError::Malformed(_)), "{err:?}");
    }

    #[test]
    fn wrong_sizes_and_empty_images_are_refused() {
        assert_eq!(write_rgba_png(0, 4, &[]), Err(PngError::EmptyImage));
        assert_eq!(
            write_rgba_png(2, 2, &[0; 15]),
            Err(PngError::SizeMismatch {
                expected: 16,
                got: 15
            })
        );
        assert!(matches!(
            parse_png(b"not a png at all"),
            Err(PngError::Malformed(_))
        ));
        assert!(matches!(
            parse_png(&PNG_SIGNATURE),
            Err(PngError::Malformed(_))
        ));
    }

    #[test]
    fn output_is_byte_identical_between_runs() {
        let pixels = checkerboard(9, 9);
        assert_eq!(
            write_rgba_png(9, 9, &pixels).expect("writes"),
            write_rgba_png(9, 9, &pixels).expect("writes")
        );
    }

    #[test]
    fn premultiplied_alpha_is_undone_for_storage() {
        // 50 % alpha premultiplied: stored as the colour it stands for.
        let premultiplied = [128, 0, 64, 128, 10, 10, 10, 255, 0, 0, 0, 0];
        let straight = unpremultiply(&premultiplied);
        assert_eq!(straight[3], 128);
        assert!(straight[0] >= 254, "{}", straight[0]);
        assert!((i32::from(straight[2]) - 127).abs() <= 1, "{}", straight[2]);
        assert_eq!(&straight[4..8], &[10, 10, 10, 255]);
        assert_eq!(&straight[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn compositing_over_a_background_flattens_to_rgb() {
        let flat = composite_over_rgb(&[255, 255, 255, 128, 0, 0, 0, 0], [40, 40, 40, 255]);
        // Half-white over dark grey, then fully transparent ⇒ pure background.
        assert_eq!(flat.len(), 6);
        assert!(
            i32::from(flat[0]) > 140 && i32::from(flat[0]) < 150,
            "{flat:?}"
        );
        assert_eq!(&flat[3..6], &[40, 40, 40]);
    }

    #[test]
    fn an_empty_deflate_stream_is_still_valid() {
        let zlib = zlib_stored(&[]);
        assert_eq!(zlib.len(), 11); // header + empty final stored block + adler
        assert_eq!(&zlib[2..7], &[0x01, 0, 0, 0xff, 0xff]);
    }
}
