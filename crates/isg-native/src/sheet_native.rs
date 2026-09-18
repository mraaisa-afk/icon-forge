//! The raster half of the sheet generator: the exported SVG, rendered.
//!
//! A sheet PNG is produced by rendering **the document the SVG exporter wrote**
//! — not by drawing the plan a second time. That is the whole design of this
//! module: the pixels and the vector file cannot disagree, because the pixels
//! *are* the vector file. It also means the raster export doubles as the
//! "opens cleanly in a real renderer" check, since `resvg`/`usvg` is the same
//! engine the quality scorer already trusts (§3.3 stage ⑧), and a document it
//! accepts is a document Chrome, Inkscape and Illustrator accept.
//!
//! Two things are worth knowing about the numbers:
//!
//! * **Scale is a separate choice from cell size.** A 16 × 64 px cell sheet is
//!   1024 px wide at 1×; print or a retina preview may want 2× or 4×. The
//!   scale multiplies the render, never the layout, so a 2× export is the same
//!   sheet with twice the pixels — never a relayouted one.
//! * **`tiny-skia` renders premultiplied and a PNG stores straight alpha.**
//!   The conversion is [`png::unpremultiply`], in the pure module, where it has
//!   tests; skipping it is the classic way to get dark fringes on every icon.
//!
//! Everything here is native-only (`resvg` lives in this crate's dependency
//! set, never in `isg-core`), which is why the arithmetic and the byte formats
//! are kept separate and testable on their own.

use crate::sheet::export::png::{unpremultiply, write_rgba_png, PngError};
use crate::sheet::export::svg::{write_sheet_svg, SvgOptions};
use crate::sheet::export::{Artwork, ExportError};
use crate::sheet::SheetPlan;

/// Pixels per side beyond which a raster export is refused, in each dimension.
pub const MAX_RASTER_SIDE: u32 = 16_384;
/// Total pixel budget for one raster export (64 Mi pixels ≈ 256 MiB of RGBA).
pub const MAX_RASTER_PIXELS: u64 = 64 * 1024 * 1024;

/// What the raster export should look like.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterOptions {
    /// How the SVG document itself is written (title, background rect, layer
    /// names). The raster inherits every one of these choices.
    pub svg: SvgOptions,
    /// Pixels per sheet pixel (1.0 = one output pixel per cell pixel).
    pub scale: f32,
    /// Fill behind the rendered icons. `None` keeps the sheet transparent,
    /// which is what an icon sheet usually wants.
    pub background: Option<[u8; 4]>,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            svg: SvgOptions::default(),
            scale: 1.0,
            background: None,
        }
    }
}

/// Why a raster export could not be produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// The vector side of the export refused (missing artwork, impossible size).
    Export(ExportError),
    /// `usvg` refused the document — the same gate §3.3 stage ⑦ uses.
    Usvg(String),
    /// The requested scale would exceed the pixel budget.
    TooLarge {
        /// Pixels per row at that scale.
        width: u32,
        /// Rows at that scale.
        height: u32,
    },
    /// The scale was not a positive finite number.
    BadScale(String),
    /// The pixmap allocation failed.
    Alloc,
    /// The PNG encoder refused the pixels (a size mismatch, which would be a
    /// bug in this module rather than in the caller's data).
    Png(PngError),
}

impl std::fmt::Display for RasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Export(e) => write!(f, "{e}"),
            Self::Usvg(e) => write!(f, "usvg rejected the sheet document: {e}"),
            Self::TooLarge { width, height } => {
                write!(f, "raster export would be {width}×{height} px")
            }
            Self::BadScale(s) => write!(f, "raster scale {s} is not a positive number"),
            Self::Alloc => f.write_str("pixmap allocation failed"),
            Self::Png(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RasterError {}

impl From<ExportError> for RasterError {
    fn from(e: ExportError) -> Self {
        Self::Export(e)
    }
}

/// A rendered sheet, plus the evidence that it came from a valid document.
///
/// The name is deliberately not `SheetRaster`: that one is the *decoded input*
/// sheet (`pipeline::raster`), this one is the *exported output* image.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedSheet {
    /// The PNG bytes (8-bit RGBA, stored deflate, byte-deterministic).
    pub png: Vec<u8>,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Pixels per sheet pixel that was rendered.
    pub scale: f32,
    /// The document those pixels are a rendering of.
    pub document: String,
    /// The size `usvg` read out of the document.
    pub document_size: (f32, f32),
    /// Filled paths the document contains.
    pub paths: usize,
}

/// Renders the sheet as a PNG.
///
/// # Errors
///
/// [`RasterError::Export`] for a missing icon or an empty plan,
/// [`RasterError::Usvg`] when the written document fails the re-parse gate,
/// [`RasterError::BadScale`] / [`RasterError::TooLarge`] for an impossible
/// raster size, and [`RasterError::Alloc`] when the pixmap cannot be allocated.
pub fn render_sheet_png(
    plan: &SheetPlan,
    artwork: &[Artwork],
    options: &RasterOptions,
) -> Result<RenderedSheet, RasterError> {
    if !options.scale.is_finite() || options.scale <= 0.0 {
        return Err(RasterError::BadScale(format!("{}", options.scale)));
    }
    let document = write_sheet_svg(plan, artwork, &options.svg)?;
    let tree = resvg::usvg::Tree::from_str(&document, &resvg::usvg::Options::default())
        .map_err(|e| RasterError::Usvg(e.to_string()))?;
    let document_size = (tree.size().width(), tree.size().height());
    let width = (document_size.0 * options.scale).round().max(1.0);
    let height = (document_size.1 * options.scale).round().max(1.0);
    if width > MAX_RASTER_SIDE as f32
        || height > MAX_RASTER_SIDE as f32
        || f64::from(width) * f64::from(height) > MAX_RASTER_PIXELS as f64
    {
        return Err(RasterError::TooLarge {
            width: width as u32,
            height: height as u32,
        });
    }
    let (width, height) = (width as u32, height as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or(RasterError::Alloc)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(options.scale, options.scale),
        &mut pixmap.as_mut(),
    );
    let mut rgba = unpremultiply(pixmap.data());
    if let Some(background) = options.background {
        composite_in_place(&mut rgba, background);
    }
    let png = write_rgba_png(width, height, &rgba).map_err(RasterError::Png)?;
    Ok(RenderedSheet {
        png,
        width,
        height,
        scale: options.scale,
        document,
        document_size,
        paths: artwork.iter().map(|a| a.shapes.len()).sum(),
    })
}

/// Composites straight RGBA over an opaque background, keeping four channels.
fn composite_in_place(rgba: &mut [u8], background: [u8; 4]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let a = u32::from(pixel[3]);
        let mix = |src: u8, bg: u8| -> u8 {
            (((u32::from(src) * a) + (u32::from(bg) * (255 - a)) + 127) / 255) as u8
        };
        pixel[0] = mix(pixel[0], background[0]);
        pixel[1] = mix(pixel[1], background[1]);
        pixel[2] = mix(pixel[2], background[2]);
        pixel[3] = 255;
    }
}

/// Runs a document through the `usvg` gate and reports the size it read.
///
/// The SVG exporter already re-parses every document it writes with the
/// engine's own reader; this is the second, independent reader — the one an
/// outside viewer behaves like.
///
/// # Errors
///
/// A human-readable reason when `usvg` refuses the document.
pub fn validate_sheet_svg(document: &str) -> Result<(f32, f32), String> {
    let tree = resvg::usvg::Tree::from_str(document, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    Ok((tree.size().width(), tree.size().height()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::export::svg::write_sheet_svg;
    use crate::sheet::{IconInput, IconMetrics, Placement, SheetSpec};
    use isg_core::editor::svg as engine_svg;

    fn plan_of(n: u32, spec: SheetSpec) -> SheetPlan {
        let icons: Vec<IconInput> = (1..=n)
            .map(|id| IconInput {
                id,
                metrics: IconMetrics {
                    ink_x: 0.0,
                    ink_y: 0.0,
                    ink_w: 20.0,
                    ink_h: 20.0,
                    ink_area: 200,
                    centroid_x: 10.0,
                    centroid_y: 10.0,
                    stroke: 4.0,
                    solidity: 0.5,
                },
            })
            .collect();
        SheetPlan::new(&icons, spec)
    }

    fn spec() -> SheetSpec {
        SheetSpec {
            cell: 64,
            padding: 8,
            gap: 8,
            margin: 8,
            columns: 2,
            ink_ratio: 0.8,
            placement: Placement::Center,
        }
    }

    /// One icon whose ink box is a solid square of the given colour.
    fn artwork_of(n: u32, fill: &str) -> Vec<Artwork> {
        (1..=n)
            .map(|id| {
                let svg = format!("<svg><path d=\"M0,0L20,0L20,20L0,20Z\" fill=\"{fill}\"/></svg>");
                crate::sheet::export::artwork_from_svg(id, format!("icon-{id:03}"), &svg)
                    .expect("artwork")
            })
            .collect()
    }

    fn pixel(raster: &RenderedSheet, x: u32, y: u32) -> [u8; 4] {
        let info = crate::sheet::export::parse_png(&raster.png).expect("png parses");
        assert_eq!((info.width, info.height), (raster.width, raster.height));
        let at = 4 * (y as usize * raster.width as usize + x as usize);
        [
            info.pixels[at],
            info.pixels[at + 1],
            info.pixels[at + 2],
            info.pixels[at + 3],
        ]
    }

    #[test]
    fn renders_the_exported_document_into_a_readable_png() {
        let plan = plan_of(4, spec());
        let art = artwork_of(4, "#ff0000");
        let raster = render_sheet_png(&plan, &art, &RasterOptions::default()).expect("renders");
        // Four icons over two columns: 152 px square at 1×.
        assert_eq!((raster.width, raster.height), (152, 152));
        assert_eq!(raster.document_size, (152.0, 152.0));
        assert_eq!(raster.paths, 4);
        assert_eq!(raster.scale, 1.0);
        // The first icon's ink box is (20.8, 20.8) + 38.4: its middle is ink.
        let middle = pixel(&raster, 40, 40);
        assert_eq!(middle[3], 255);
        assert!(
            middle[0] > 200 && middle[1] < 40 && middle[2] < 40,
            "{middle:?}"
        );
        // …and the margin is transparent, because no background was asked for.
        assert_eq!(pixel(&raster, 2, 2)[3], 0);
        // The icon's own cell is where the ink is: the padding is empty, the
        // ink's interior is solid, and the pixel that straddles the ink box's
        // edge is *partially* covered — a real renderer antialiases, so the
        // first pixel of the box is a fraction, not a hard step.
        assert_eq!(pixel(&raster, 17, 40)[3], 0, "the padding is empty");
        assert_eq!(pixel(&raster, 40, 40)[3], 255, "the ink is solid");
        let edge = pixel(&raster, 20, 40)[3];
        assert!(
            edge > 0 && edge < 255,
            "the ink box's edge is antialiased, got alpha {edge}"
        );
        // The document that was rendered is the one the exporter wrote.
        let again = write_sheet_svg(&plan, &art, &SvgOptions::default());
        assert_eq!(Ok(raster.document.clone()), again);
    }

    #[test]
    fn the_document_passes_both_readers_and_a_background_flattens_the_alpha() {
        let plan = plan_of(2, spec());
        let art = artwork_of(2, "#3366ff");
        let raster = render_sheet_png(
            &plan,
            &art,
            &RasterOptions {
                background: Some([255, 255, 255, 255]),
                ..RasterOptions::default()
            },
        )
        .expect("renders");
        // The second reader: the one an outside viewer behaves like.
        let (w, h) = validate_sheet_svg(&raster.document).expect("usvg accepts the sheet");
        assert!(
            (w - 152.0).abs() < 0.01 && (h - 80.0).abs() < 0.01,
            "{w}×{h}"
        );
        // The engine's reader, too.
        engine_svg::parse(&raster.document).expect("the engine accepts the sheet");
        // With a background, no pixel is transparent anywhere — two icons over
        // two columns are 152 × 80 px, so the far corner is (150, 78).
        for (x, y) in [(2, 2), (40, 40), (150, 78)] {
            assert_eq!(pixel(&raster, x, y)[3], 255, "pixel {x},{y}");
        }
        assert_eq!(pixel(&raster, 2, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn scale_multiplies_the_raster_and_never_the_layout() {
        let plan = plan_of(2, spec());
        let art = artwork_of(2, "#000000");
        let double = render_sheet_png(
            &plan,
            &art,
            &RasterOptions {
                scale: 2.0,
                ..RasterOptions::default()
            },
        )
        .expect("renders");
        assert_eq!((double.width, double.height), (304, 160));
        // The document is unchanged by the scale: only the raster grew.
        assert_eq!(double.document_size, (152.0, 80.0));
        assert_eq!(
            double.document,
            render_sheet_png(&plan, &art, &RasterOptions::default())
                .expect("renders")
                .document
        );
    }

    #[test]
    fn the_png_is_byte_identical_between_runs() {
        let plan = plan_of(3, spec());
        let art = artwork_of(3, "#0a0a0a");
        let a = render_sheet_png(&plan, &art, &RasterOptions::default()).expect("renders");
        let b = render_sheet_png(&plan, &art, &RasterOptions::default()).expect("renders");
        assert_eq!(a.png, b.png);
    }

    #[test]
    fn an_impossible_raster_is_refused_not_attempted() {
        let plan = plan_of(2, spec());
        let art = artwork_of(2, "#000000");
        let err = render_sheet_png(
            &plan,
            &art,
            &RasterOptions {
                scale: 0.0,
                ..RasterOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, RasterError::BadScale(_)), "{err:?}");
        let err = render_sheet_png(
            &plan,
            &art,
            &RasterOptions {
                scale: 1000.0,
                ..RasterOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, RasterError::TooLarge { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_icon_is_refused_before_anything_is_rendered() {
        let plan = plan_of(3, spec());
        let art = artwork_of(2, "#000000");
        let err = render_sheet_png(&plan, &art, &RasterOptions::default()).unwrap_err();
        assert_eq!(
            err,
            RasterError::Export(ExportError::MissingArtwork { id: 3 })
        );
    }
}
