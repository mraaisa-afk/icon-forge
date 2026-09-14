//! PNG/JPEG-backed luma raster for the spike.

use isg_core::RasterView;

/// Decoded sheet: dimensions + row-major luma (0.0–255.0).
#[derive(Clone, Debug)]
pub struct PngRaster {
    width: u32,
    height: u32,
    luma: Vec<f32>,
}

impl PngRaster {
    /// Decodes an image file into a luma raster (the spike's decode stage).
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let img = image::open(path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let gray = img.to_luma8();
        let (w, h) = (gray.width(), gray.height());
        let luma: Vec<f32> = gray.as_raw().iter().map(|&v| v as f32).collect();
        Ok(Self {
            width: w,
            height: h,
            luma,
        })
    }

    /// Raw luma buffer (length `width * height`, row-major, top-left origin).
    pub fn luma(&self) -> &[f32] {
        &self.luma
    }

    /// Grayscale bytes (for building crop images).
    pub fn gray_bytes(&self) -> Vec<u8> {
        self.luma
            .iter()
            .map(|v| v.round().clamp(0.0, 255.0) as u8)
            .collect()
    }

    /// A row-slice of the raw luma (alias of [`RasterView::luma_row`]).
    pub fn row(&self, y: u32) -> &[f32] {
        self.luma_row(y)
    }
}

impl RasterView for PngRaster {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn luma_row(&self, y: u32) -> &[f32] {
        let w = self.width as usize;
        &self.luma[y as usize * w..(y as usize + 1) * w]
    }
}

/// Decode error passthrough (keeps call sites tidy).
pub type DecodeResult<T> = std::io::Result<T>;
