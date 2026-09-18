//! Sheet exporters — SVG, PDF, PNG, CSV and the manifest.
//!
//! Every writer takes the same two things: a finished [`SheetPlan`] (where each
//! icon goes, and at what scale) and the icons' [`Artwork`] (their geometry, in
//! the crop-local coordinates the plan's transform expects). Nothing here
//! re-derives placement, so a sheet exported as SVG, one exported as PDF and
//! the preview drawn in the webview are the same sheet in three formats.
//!
//! Icons are parsed with the engine's own SVG reader ([`crate::sheet::export`]
//! borrows `isg_core::editor::svg`) rather than with a second parser: the sheet
//! generator and the editor therefore agree about what a shape is, about
//! `fill="#rrggbb"`, and about what they refuse. An icon whose cached document
//! no longer parses is an error the caller can report per icon — never a
//! silently missing cell.
//!
//! Writers are pure (`std` + `isg_core`): the raster exports (PNG) and the
//! usvg re-parse gate live in `sheet_native`, next to the image crates, so the
//! byte-level formats stay testable everywhere.

pub mod csv;
pub mod pdf;
pub mod svg;

use isg_core::editor::svg as engine_svg;
use isg_core::editor::{Affine, Point, Seg, Subpath};

pub use csv::{
    derive_row, write_csv, CsvError, IconMeta, SheetRow, CSV_COLUMNS, DEFAULT_DELIMITER,
};
pub use pdf::{validate_pdf, write_sheet_pdf, PdfOptions, PdfSummary};
pub use svg::{write_sheet_svg, SvgOptions};

/// Fill used for an icon whose document gives it no colour.
pub const DEFAULT_FILL: [u8; 4] = [0x0a, 0x0a, 0x0a, 255];

/// Why an export could not be produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    /// The icon's cached SVG document did not parse.
    BadArtwork {
        /// Which icon.
        id: u32,
        /// The engine's reason.
        reason: String,
    },
    /// The plan and the artwork do not cover the same icons.
    MissingArtwork {
        /// Which icon.
        id: u32,
    },
    /// A page or canvas size came out impossible (zero, or not finite).
    BadGeometry(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadArtwork { id, reason } => write!(f, "icon {id}: {reason}"),
            Self::MissingArtwork { id } => write!(f, "no artwork for icon {id}"),
            Self::BadGeometry(what) => write!(f, "impossible geometry: {what}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// One filled region of an icon, in crop-local coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtworkShape {
    /// The outline (line and cubic segments, subpaths in paint order).
    pub path: Vec<Subpath>,
    /// `#rrggbbaa`, or [`DEFAULT_FILL`] when the document had no colour.
    pub fill: [u8; 4],
}

/// One icon's geometry, ready to place.
#[derive(Clone, Debug, PartialEq)]
pub struct Artwork {
    /// The plan id this artwork belongs to.
    pub id: u32,
    /// Display name (the sheet's `<title>` and the CSV's `name` column).
    pub name: String,
    /// The filled regions, in paint order.
    pub shapes: Vec<ArtworkShape>,
}

impl Artwork {
    /// A shape-less artwork (an icon that drew nothing) — exportable, invisible.
    #[must_use]
    pub fn empty(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            shapes: Vec::new(),
        }
    }
}

/// Parses one icon's cached SVG document into placeable artwork.
///
/// The document's own `viewBox` is *not* applied: the plan's transform is
/// expressed in the crop's pixel space, which is what the pipeline's emitter
/// writes into the paths, and what `viewBox="0 0 w h"` merely restates.
///
/// # Errors
///
/// [`ExportError::BadArtwork`] when the engine's reader refuses the document.
pub fn artwork_from_svg(
    id: u32,
    name: impl Into<String>,
    svg: &str,
) -> Result<Artwork, ExportError> {
    let parsed = engine_svg::parse(svg).map_err(|e| ExportError::BadArtwork {
        id,
        reason: e.message().to_string(),
    })?;
    let mut shapes = Vec::with_capacity(parsed.shapes.len());
    for shape in parsed.shapes {
        if shape.path.is_empty() {
            continue;
        }
        // The shape's own transform (and any inherited `<g>` ones) is baked in
        // here: the sheet's transform is the *only* one left afterwards, which
        // is what keeps the exporters simple enough to be obviously correct.
        let path = if shape.transform == Affine::IDENTITY {
            shape.path
        } else {
            shape
                .path
                .iter()
                .map(|sub| transformed(sub, shape.transform))
                .collect()
        };
        shapes.push(ArtworkShape {
            path,
            fill: shape.fill.unwrap_or(DEFAULT_FILL),
        });
    }
    Ok(Artwork {
        id,
        name: name.into(),
        shapes,
    })
}

/// One subpath with an affine folded into every point of it.
#[must_use]
pub fn transformed(sub: &Subpath, m: Affine) -> Subpath {
    let map = |p: Point| {
        let (x, y) = m.apply(p.x, p.y);
        Point::new(x, y)
    };
    Subpath {
        start: map(sub.start),
        closed: sub.closed,
        segs: sub
            .segs
            .iter()
            .map(|seg| match *seg {
                Seg::Line(to) => Seg::Line(map(to)),
                Seg::Cubic { c1, c2, to } => Seg::Cubic {
                    c1: map(c1),
                    c2: map(c2),
                    to: map(to),
                },
            })
            .collect(),
    }
}

/// Formats a coordinate the way every text exporter here does: three decimals,
/// trailing zeros removed, `-0` normalized to `0`.
///
/// One shared formatter means the SVG, the PDF and the preview all round the
/// same way, and a sheet stays byte-identical between runs (§3.3's determinism
/// rule, applied to exports).
#[must_use]
pub fn fmt_num(value: f32) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{value:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// `#rrggbb` — the alpha channel is dropped only when the caller asks for an
/// opaque document (SVG fills use it; PDF's model has no alpha here).
#[must_use]
pub fn hex_rgb(fill: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", fill[0], fill[1], fill[2])
}

/// The fill's colour as three `0..=1` components, for PDF.
#[must_use]
pub fn rgb_unit(fill: [u8; 4]) -> (f32, f32, f32) {
    (
        f32::from(fill[0]) / 255.0,
        f32::from(fill[1]) / 255.0,
        f32::from(fill[2]) / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" \
        viewBox=\"0 0 24 24\"><title>icon</title>\
        <path d=\"M4,4L12,4L12,12L4,12Z\" fill=\"#ff0000\"/>\
        <path d=\"M6,14C8,16 10,16 12,14Z\"/></svg>";

    #[test]
    fn reads_the_engine_parser_and_keeps_its_colours() {
        let art = artwork_from_svg(7, "star", ICON).expect("parses");
        assert_eq!(art.id, 7);
        assert_eq!(art.name, "star");
        assert_eq!(art.shapes.len(), 2);
        assert_eq!(art.shapes[0].fill, [255, 0, 0, 255]);
        // No `fill` attribute: the default ink colour, not black-by-accident.
        assert_eq!(art.shapes[1].fill, DEFAULT_FILL);
        assert_eq!(art.shapes[0].path.len(), 1);
        assert!(art.shapes[0].path[0].closed);
        // One cubic, and `Z` — which the engine records as `closed`, not as a
        // segment to the start point.
        assert_eq!(art.shapes[1].path[0].segs.len(), 1);
        assert!(art.shapes[1].path[0].closed);
    }

    #[test]
    fn bakes_a_shape_transform_into_its_geometry() {
        let svg = "<svg><g transform=\"translate(10,20)\"><path d=\"M0,0L4,0L4,4Z\"/></g></svg>";
        let art = artwork_from_svg(1, "moved", svg).expect("parses");
        assert_eq!(art.shapes[0].path[0].start, Point::new(10.0, 20.0));
        assert_eq!(
            art.shapes[0].path[0].segs[0],
            Seg::Line(Point::new(14.0, 20.0))
        );
    }

    #[test]
    fn a_document_the_engine_refuses_is_an_error_with_the_icon_id() {
        let err = artwork_from_svg(9, "bad", "<svg><path d=\"M0,0 B1,1\"/></svg>").unwrap_err();
        match err {
            ExportError::BadArtwork { id, reason } => {
                assert_eq!(id, 9);
                assert!(!reason.is_empty());
            }
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn numbers_are_short_stable_and_never_negative_zero() {
        assert_eq!(fmt_num(4.0), "4");
        assert_eq!(fmt_num(4.5), "4.5");
        assert_eq!(fmt_num(4.0004), "4");
        assert_eq!(fmt_num(-0.0004), "0");
        assert_eq!(fmt_num(-2.25), "-2.25");
        assert_eq!(fmt_num(f32::NAN), "0");
        assert_eq!(fmt_num(f32::INFINITY), "0");
        assert_eq!(hex_rgb([0x0a, 0xbb, 0xc0, 128]), "#0abbc0");
        let (r, g, b) = rgb_unit([255, 128, 0, 255]);
        assert!((r - 1.0).abs() < 1e-6 && (g - 128.0 / 255.0).abs() < 1e-6 && b == 0.0);
    }

    #[test]
    fn a_transform_folds_into_cubics_including_their_handles() {
        let sub = Subpath {
            start: Point::new(0.0, 0.0),
            closed: false,
            segs: vec![Seg::Cubic {
                c1: Point::new(1.0, 0.0),
                c2: Point::new(2.0, 1.0),
                to: Point::new(3.0, 3.0),
            }],
        };
        let moved = transformed(&sub, Affine::translate(5.0, -1.0));
        assert_eq!(moved.start, Point::new(5.0, -1.0));
        match moved.segs[0] {
            Seg::Cubic { c1, c2, to } => {
                assert_eq!(c1, Point::new(6.0, -1.0));
                assert_eq!(c2, Point::new(7.0, 0.0));
                assert_eq!(to, Point::new(8.0, 2.0));
            }
            Seg::Line(_) => panic!("a cubic stays a cubic"),
        }
    }
}
