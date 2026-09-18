//! The sheet as one SVG document.
//!
//! One `<g>` per icon, carrying the *only* transform in the file: the plan's
//! `translate(…) scale(…)`. Inside the group the icon's own geometry is written
//! verbatim (already crop-local from the emitter), so the sheet is a faithful
//! container rather than a re-tracing — what an editor shows after opening it is
//! the geometry the library stores, and the layer names are the icon names.
//!
//! Presentation attributes only: no `<style>`, no CSS, no `<defs>` reuse. That
//! is the subset Inkscape, Illustrator and Chrome all agree on, and it keeps
//! the file readable by a human who opens it in a text editor.

use std::collections::HashMap;

use super::{fmt_num, hex_rgb, Artwork, ExportError};
use crate::sheet::{Placement, SheetPlan};
use isg_core::editor::{Seg, Subpath};

/// What the SVG writer should put around the plan.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgOptions {
    /// The document's `<title>` (the sheet's name).
    pub title: String,
    /// Optional background rectangle; `None` leaves the sheet transparent,
    /// which is what an icon sheet usually wants.
    pub background: Option<[u8; 4]>,
    /// Write each icon's name as the group's `<title>` and `id` (on by default:
    /// it is what makes the sheet navigable in a design tool).
    pub name_layers: bool,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            title: "icon-sheet".to_string(),
            background: None,
            name_layers: true,
        }
    }
}

/// Writes `plan` and `artwork` as one SVG sheet.
///
/// # Errors
///
/// [`ExportError::MissingArtwork`] when an icon in the plan has no artwork, and
/// [`ExportError::BadGeometry`] when the plan's size is not a positive number.
pub fn write_sheet_svg(
    plan: &SheetPlan,
    artwork: &[Artwork],
    options: &SvgOptions,
) -> Result<String, ExportError> {
    let (width, height) = plan.size();
    if width == 0 || height == 0 {
        return Err(ExportError::BadGeometry("sheet has no area".to_string()));
    }
    let by_id: HashMap<u32, &Artwork> = artwork.iter().map(|a| (a.id, a)).collect();
    let mut out = String::with_capacity(plan.placements.len() * 192 + 512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\"");
    out.push_str(&format!(
        " width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">"
    ));
    out.push_str(&format!("<title>{}</title>", escape(&options.title)));
    out.push_str(&format!(
        "<desc>Icon Forge sheet · {} icons · {} px cells · {} placement</desc>",
        plan.placements.len(),
        plan.spec.cell,
        placement_name(plan.spec.placement)
    ));
    out.push_str(&format!(
        "<metadata>icon-forge sheet v1 cell=\"{}\" padding=\"{}\" gap=\"{}\" ink-ratio=\"{}\" \
         placement=\"{}\" icons=\"{}\" ink-size-cv=\"{:.4}\"</metadata>",
        plan.spec.cell,
        plan.spec.padding,
        plan.spec.gap,
        fmt_num(plan.spec.ink_ratio),
        plan.spec.placement.as_str(),
        plan.placements.len(),
        plan.report.ink_size_cv
    ));
    if let Some(background) = options.background {
        out.push_str(&format!(
            "<rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"{}\"",
            hex_rgb(background)
        ));
        if background[3] < 255 {
            out.push_str(&format!(
                " fill-opacity=\"{}\"",
                fmt_num(f32::from(background[3]) / 255.0)
            ));
        }
        out.push_str("/>");
    }
    out.push('\n');

    for placement in &plan.placements {
        let icon = by_id
            .get(&placement.id)
            .ok_or(ExportError::MissingArtwork { id: placement.id })?;
        let (tx, ty, scale) = placement.transform();
        out.push_str("<g");
        if options.name_layers {
            out.push_str(&format!(" id=\"{}\"", escape_id(&icon.name)));
        }
        out.push_str(&format!(
            " transform=\"translate({},{}) scale({})\">",
            fmt_num(tx),
            fmt_num(ty),
            fmt_num(scale)
        ));
        if options.name_layers {
            out.push_str(&format!("<title>{}</title>", escape(&icon.name)));
        }
        for shape in &icon.shapes {
            out.push_str(&format!(
                "<path d=\"{}\" fill=\"{}\"",
                path_data(&shape.path),
                hex_rgb(shape.fill)
            ));
            if shape.fill[3] < 255 {
                out.push_str(&format!(
                    " fill-opacity=\"{}\"",
                    fmt_num(f32::from(shape.fill[3]) / 255.0)
                ));
            }
            out.push_str("/>");
        }
        out.push_str("</g>\n");
    }
    out.push_str("</svg>\n");
    Ok(out)
}

/// The `d` attribute for a list of subpaths (absolute commands only).
#[must_use]
pub fn path_data(path: &[Subpath]) -> String {
    let mut d = String::new();
    for sub in path {
        d.push('M');
        d.push_str(&fmt_num(sub.start.x));
        d.push(',');
        d.push_str(&fmt_num(sub.start.y));
        for seg in &sub.segs {
            match seg {
                Seg::Line(to) => {
                    d.push('L');
                    d.push_str(&fmt_num(to.x));
                    d.push(',');
                    d.push_str(&fmt_num(to.y));
                }
                Seg::Cubic { c1, c2, to } => {
                    d.push('C');
                    d.push_str(&fmt_num(c1.x));
                    d.push(',');
                    d.push_str(&fmt_num(c1.y));
                    d.push(' ');
                    d.push_str(&fmt_num(c2.x));
                    d.push(',');
                    d.push_str(&fmt_num(c2.y));
                    d.push(' ');
                    d.push_str(&fmt_num(to.x));
                    d.push(',');
                    d.push_str(&fmt_num(to.y));
                }
            }
        }
        if sub.closed {
            d.push('Z');
        }
    }
    d
}

fn placement_name(placement: Placement) -> &'static str {
    placement.as_str()
}

/// XML text escaping for titles and names.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not legal XML 1.0 at all; a sheet with one
            // in a name must still export.
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// An id-safe form of a name: XML-escaped, with whitespace collapsed to `-`.
fn escape_id(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.trim().chars() {
        if c.is_whitespace() {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
            continue;
        }
        last_dash = false;
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 => out.push('_'),
            c => out.push(c),
        }
    }
    if out.is_empty() {
        "icon".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::export::{artwork_from_svg, Artwork};
    use crate::sheet::{GridLayout, IconInput, IconMetrics, SheetPlan, SheetSpec};
    use isg_core::editor::svg as engine_svg;
    use isg_core::editor::{Point, Subpath};

    fn metrics(side: f32, stroke: f32, solidity: f32) -> IconMetrics {
        IconMetrics {
            ink_x: 0.0,
            ink_y: 0.0,
            ink_w: side,
            ink_h: side,
            ink_area: (side * side * solidity) as u32,
            centroid_x: side * 0.5,
            centroid_y: side * 0.5,
            stroke,
            solidity,
        }
    }

    fn plan_of(n: u32) -> SheetPlan {
        let icons: Vec<IconInput> = (1..=n)
            .map(|id| IconInput {
                id,
                metrics: metrics(20.0, 4.0, 0.5),
            })
            .collect();
        let spec = SheetSpec {
            columns: 2,
            ..SheetSpec::default()
        };
        SheetPlan::new(&icons, spec)
    }

    fn artwork_of(n: u32, side: f32) -> Vec<Artwork> {
        (1..=n)
            .map(|id| {
                let svg = format!(
                    "<svg><path d=\"M0,0L{side},0L{side},{side}L0,{side}Z\" \
                     fill=\"#ff8800\"/></svg>"
                );
                artwork_from_svg(id, format!("icon-{id:03}"), &svg).expect("artwork")
            })
            .collect()
    }

    #[test]
    fn writes_a_wellformed_document_the_engine_can_read_back() {
        let plan = plan_of(4);
        let art = artwork_of(4, 20.0);
        let svg = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
        assert!(svg.ends_with("</svg>\n"));
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        // 4 cells over 2 columns: 2·8 + 2·64 + 1·8 = 152 px square.
        assert!(
            svg.contains("width=\"152\" height=\"152\" viewBox=\"0 0 152 152\""),
            "{svg}"
        );
        // The engine's own reader is the strictest XML-ish check available here:
        // it must find exactly the four icons, each one square.
        let reparsed = engine_svg::parse(&svg).expect("the sheet parses");
        assert_eq!(reparsed.shapes.len(), 4);
        for shape in &reparsed.shapes {
            assert_eq!(shape.path.len(), 1);
            assert!(shape.path[0].closed);
            assert_eq!(shape.fill, Some([0xff, 0x88, 0x00, 255]));
        }
        // The reader keeps each icon's own geometry and the group transform, so
        // placement is checked by composing them the way a renderer would: the
        // ink's top-left corner lands at 8 + 12.8 in its cell.
        assert_eq!(reparsed.shapes[0].path[0].start, Point::new(0.0, 0.0));
        let placed = |shape: &engine_svg::SvgShape| shape.transform.apply(0.0, 0.0);
        assert_eq!(placed(&reparsed.shapes[0]), (20.8, 20.8));
        assert_eq!(placed(&reparsed.shapes[1]), (92.8, 20.8));
        assert_eq!(placed(&reparsed.shapes[2]), (20.8, 92.8));
        // And the far corner of the first icon stays inside its own cell: the
        // 20 px ink, grown by the 1.92 scale, ends at 20.8 + 38.4 = 59.2 < 72.
        let far = reparsed.shapes[0].transform.apply(20.0, 20.0);
        assert!(
            (far.0 - 59.2).abs() < 1e-3 && (far.1 - 59.2).abs() < 1e-3,
            "{far:?}"
        );
    }

    #[test]
    fn groups_are_named_and_escaped() {
        let plan = plan_of(1);
        let art = vec![artwork_from_svg(1, "arrow <up> & \"down\"", "<svg/>").expect("artwork")];
        let svg = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert!(
            svg.contains("<title>arrow &lt;up&gt; &amp; &quot;down&quot;</title>"),
            "{svg}"
        );
        assert!(
            svg.contains("id=\"arrow-&lt;up&gt;-&amp;-&quot;down&quot;\""),
            "{svg}"
        );
        // …and the file is still readable XML for the engine's scanner.
        engine_svg::parse(&svg).expect("escaped names do not break the document");
    }

    #[test]
    fn the_metadata_block_records_the_spec_and_the_headline_number() {
        let plan = plan_of(3);
        let art = artwork_of(3, 20.0);
        let svg = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert!(svg.contains(
            "<metadata>icon-forge sheet v1 cell=\"64\" padding=\"8\" gap=\"8\" \
            ink-ratio=\"0.8\" placement=\"center\" icons=\"3\" ink-size-cv=\"0.0000\"</metadata>"
        ));
    }

    #[test]
    fn a_background_is_optional_and_carries_its_alpha() {
        let plan = plan_of(1);
        let art = artwork_of(1, 20.0);
        let plain = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert!(!plain.contains("<rect"));
        let tinted = write_sheet_svg(
            &plan,
            &art,
            &SvgOptions {
                background: Some([255, 255, 255, 128]),
                ..SvgOptions::default()
            },
        )
        .expect("writes");
        assert!(tinted.contains("<rect x=\"0\" y=\"0\" width=\"80\" height=\"80\" fill=\"#ffffff\" fill-opacity=\"0.502\"/>"));
    }

    #[test]
    fn a_missing_icon_is_an_error_not_a_hole_in_the_sheet() {
        let plan = plan_of(3);
        let art = artwork_of(2, 20.0);
        let err = write_sheet_svg(&plan, &art, &SvgOptions::default()).unwrap_err();
        assert_eq!(err, ExportError::MissingArtwork { id: 3 });
    }

    #[test]
    fn output_is_byte_identical_between_runs() {
        let plan = plan_of(6);
        let art = artwork_of(6, 20.0);
        let a = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        let b = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert_eq!(a, b);
    }

    #[test]
    fn an_icon_with_no_shapes_still_takes_its_cell() {
        let plan = plan_of(2);
        let art = vec![artwork_of(1, 20.0).remove(0), Artwork::empty(2, "blank")];
        let svg = write_sheet_svg(&plan, &art, &SvgOptions::default()).expect("writes");
        assert_eq!(svg.matches("<g ").count(), 2);
        assert_eq!(svg.matches("<path").count(), 1);
    }

    #[test]
    fn path_data_covers_lines_cubics_and_closes() {
        let sub = Subpath {
            start: Point::new(1.0, 2.0),
            closed: true,
            segs: vec![
                Seg::Line(Point::new(3.0, 2.0)),
                Seg::Cubic {
                    c1: Point::new(3.5, 2.5),
                    c2: Point::new(3.0, 3.0),
                    to: Point::new(1.0, 3.0),
                },
            ],
        };
        assert_eq!(path_data(&[sub]), "M1,2L3,2C3.5,2.5 3,3 1,3Z");
        assert_eq!(path_data(&[]), "");
    }

    #[test]
    fn a_sheet_with_no_area_is_refused() {
        let plan = SheetPlan {
            spec: SheetSpec::default(),
            layout: GridLayout {
                columns: 0,
                rows: 0,
                width: 0,
                height: 0,
            },
            placements: Vec::new(),
            report: crate::sheet::LevelReport {
                icons: 0,
                ink_size_cv: 0.0,
                stroke_cv: 0.0,
                baseline_spread: 0.0,
                median_stroke: 0.0,
                median_solidity: 0.0,
                overflow_backoffs: 0,
                stroke_clamped: 0,
                solidity_clamped: 0,
            },
        };
        let err = write_sheet_svg(&plan, &[], &SvgOptions::default()).unwrap_err();
        assert!(matches!(err, ExportError::BadGeometry(_)));
    }
}
