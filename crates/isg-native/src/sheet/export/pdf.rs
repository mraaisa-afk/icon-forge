//! The sheet as a PDF — written by hand, and read back by [`validate_pdf`].
//!
//! Why hand-rolled: a PDF writer is the one exporter that cannot be faked with
//! a text file, the mainstream crates either pull a large tree in (`printpdf`,
//! `lopdf`) or are GPL-adjacent in their examples, and this workspace keeps its
//! dependency surface closed (`isg-native` may not grow a PDF stack for one
//! button). The format needed here is small and old: one page, `re`/`m`/`l`/`c`
//! path operators, `rg` for fill colour, `f*` for the even-odd fill the canvas
//! uses, and a cross-reference table. PDF 1.7 has been readable by every
//! viewer since 2006, and a strictly-written xref is what "opens cleanly"
//! means in practice.
//!
//! Two details are easy to get wrong and are therefore both *tested*:
//!
//! * **The y axis.** PDF's origin is bottom-left; the sheet's is top-left. Every
//!   coordinate flips through the page height, in one place.
//! * **The xref.** Offsets are byte positions of the object headers, the table
//!   is exactly 20 bytes per entry, and `startxref` points at the `xref`
//!   keyword — a file that is one byte out opens as a blank page in some
//!   readers and refuses in others.
//!
//! [`validate_pdf`] walks the file the way a reader does: header, xref, the
//! catalogue's page tree, every page's media box, and the content stream's
//! declared length. It is our own gate, used by the Phase-5 tests, and it is
//! deliberately *not* lenient.

use std::collections::HashMap;

use super::{fmt_num, rgb_unit, Artwork, ExportError};
use crate::sheet::SheetPlan;
use isg_core::editor::Seg;

/// Points per pixel: 96 px/inch is the design default, 72 pt/inch is PDF's.
pub const DEFAULT_PDF_SCALE: f32 = 72.0 / 96.0;

/// What the PDF writer should put around the plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfOptions {
    /// Pixels to points.
    pub scale: f32,
    /// Optional page background (painted first, so icons sit on it).
    pub background: Option<[u8; 4]>,
    /// Document title (the reader's window title).
    pub title: String,
    /// `/Creator` — who made the artwork.
    pub creator: String,
    /// `/Producer` — what wrote the file.
    pub producer: String,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            scale: DEFAULT_PDF_SCALE,
            background: None,
            title: "icon-sheet".to_string(),
            creator: "Icon Forge".to_string(),
            producer: "Icon Forge sheet generator".to_string(),
        }
    }
}

/// What [`validate_pdf`] found (also what the tests quote as evidence).
#[derive(Clone, Debug, PartialEq)]
pub struct PdfSummary {
    /// The version from the header line.
    pub version: String,
    /// Objects the xref declares.
    pub objects: usize,
    /// Pages in the page tree.
    pub pages: usize,
    /// The first page's media box, in points.
    pub media_box: (f32, f32),
    /// Filled path operators counted in the content streams.
    pub fills: usize,
    /// Bytes of content streams.
    pub content_bytes: usize,
}

/// Writes `plan` and `artwork` as a one-page PDF sheet.
///
/// # Errors
///
/// [`ExportError::MissingArtwork`] for an icon with no art, and
/// [`ExportError::BadGeometry`] when the plan or the scale has no area.
pub fn write_sheet_pdf(
    plan: &SheetPlan,
    artwork: &[Artwork],
    options: &PdfOptions,
) -> Result<Vec<u8>, ExportError> {
    let (width_px, height_px) = plan.size();
    if width_px == 0 || height_px == 0 {
        return Err(ExportError::BadGeometry("sheet has no area".to_string()));
    }
    if !options.scale.is_finite() || options.scale <= 0.0 {
        return Err(ExportError::BadGeometry(format!(
            "scale {} is not a positive length",
            options.scale
        )));
    }
    let page_w = width_px as f32 * options.scale;
    let page_h = height_px as f32 * options.scale;
    if !page_w.is_finite() || !page_h.is_finite() || page_w <= 0.0 || page_h <= 0.0 {
        return Err(ExportError::BadGeometry("page has no area".to_string()));
    }

    let by_id: HashMap<u32, &Artwork> = artwork.iter().map(|a| (a.id, a)).collect();
    let mut content = String::with_capacity(plan.placements.len() * 256 + 256);
    if let Some(background) = options.background {
        let (r, g, b) = rgb_unit(background);
        content.push_str(&format!(
            "{} {} {} rg 0 0 {} {} re f\n",
            fmt_num(r),
            fmt_num(g),
            fmt_num(b),
            fmt_num(page_w),
            fmt_num(page_h)
        ));
    }
    // The flip: sheet coordinates are top-left, PDF's are bottom-left.
    let flip = |y: f32| page_h - y * options.scale;
    for placement in &plan.placements {
        let icon = by_id
            .get(&placement.id)
            .ok_or(ExportError::MissingArtwork { id: placement.id })?;
        if icon.shapes.is_empty() {
            continue;
        }
        let (tx, ty, scale) = placement.transform();
        let k = options.scale;
        content.push_str("q\n");
        for shape in &icon.shapes {
            let (r, g, b) = rgb_unit(shape.fill);
            content.push_str(&format!(
                "{} {} {} rg\n",
                fmt_num(r),
                fmt_num(g),
                fmt_num(b)
            ));
            // One `m` + segments + `f*` per filled region: the sheet's transform
            // is applied here, once, exactly as the SVG exporter does it.
            let map = |x: f32, y: f32| ((tx + x * scale) * k, flip(ty + y * scale));
            let mut started = false;
            for sub in &shape.path {
                let (sx, sy) = map(sub.start.x, sub.start.y);
                content.push_str(&format!("{} {} m\n", fmt_num(sx), fmt_num(sy)));
                started = true;
                for seg in &sub.segs {
                    match seg {
                        Seg::Line(to) => {
                            let (x, y) = map(to.x, to.y);
                            content.push_str(&format!("{} {} l\n", fmt_num(x), fmt_num(y)));
                        }
                        Seg::Cubic { c1, c2, to } => {
                            let (x1, y1) = map(c1.x, c1.y);
                            let (x2, y2) = map(c2.x, c2.y);
                            let (x3, y3) = map(to.x, to.y);
                            content.push_str(&format!(
                                "{} {} {} {} {} {} c\n",
                                fmt_num(x1),
                                fmt_num(y1),
                                fmt_num(x2),
                                fmt_num(y2),
                                fmt_num(x3),
                                fmt_num(y3)
                            ));
                        }
                    }
                }
                if sub.closed {
                    content.push_str("h\n");
                }
            }
            if started {
                // Even-odd, matching how the editor and the canvas fill a path
                // with several subpaths: a traced ring must stay a ring.
                content.push_str("f*\n");
            }
        }
        content.push_str("Q\n");
    }

    Ok(assemble(&content, page_w, page_h, options))
}

/// Builds the file: five objects, an xref table, a trailer.
fn assemble(content: &str, page_w: f32, page_h: f32, options: &PdfOptions) -> Vec<u8> {
    let body = |n: usize, text: &str| format!("{n} 0 obj\n{text}\nendobj\n");
    let mut objects: Vec<String> = Vec::with_capacity(5);
    objects.push(body(1, "<< /Type /Catalog /Pages 2 0 R >>"));
    objects.push(body(2, "<< /Type /Pages /Kids [ 3 0 R ] /Count 1 >>"));
    objects.push(body(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [ 0 0 {} {} ] \
             /Resources << /ProcSet [ /PDF ] >> /Contents 4 0 R >>",
            fmt_num(page_w),
            fmt_num(page_h)
        ),
    ));
    objects.push(format!(
        "4 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
        content.len()
    ));
    objects.push(body(
        5,
        &format!(
            "<< /Title ({}) /Creator ({}) /Producer ({}) >>",
            pdf_string(&options.title),
            pdf_string(&options.creator),
            pdf_string(&options.producer)
        ),
    ));

    let mut out = Vec::with_capacity(content.len() + 1024);
    // The file is deliberately pure ASCII — no binary marker comment. A sheet
    // of traced icons has nothing binary in it, and staying ASCII is what lets
    // `validate_pdf` assert the encoding instead of tolerating a byte range.
    out.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for object in &objects {
        offsets.push(out.len());
        out.extend_from_slice(object.as_bytes());
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Escapes a PDF literal string: `\`, `(`, `)` are special, and the encoding is
/// ASCII here — a non-ASCII title is transliterated to `?` rather than written
/// as bytes no viewer would agree on.
fn pdf_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\r' | '\n' | '\t' => out.push(' '),
            c if c.is_ascii() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

/// Walks a PDF the way a reader does, or explains what is wrong with it.
///
/// Checks, in order: the header, `startxref`, the xref table's offsets, every
/// object header, the catalogue, the page tree, each page's `/MediaBox` and
/// `/Contents`, and — for each stream — that `/Length` matches the bytes
/// between `stream` and `endstream`.
///
/// # Errors
///
/// A human-readable reason, naming the byte offset where possible.
pub fn validate_pdf(bytes: &[u8]) -> Result<PdfSummary, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("sheet PDF is not ASCII: {e}"))?;
    if !text.starts_with("%PDF-1.") {
        return Err("missing %PDF-1.x header".to_string());
    }
    let version = text[..8].to_string();
    let end = text.rfind("%%EOF").ok_or("missing %%EOF")?;
    if !text[end + 5..].trim().is_empty() {
        return Err("trailing data after %%EOF".to_string());
    }
    let startxref_at = text.rfind("startxref").ok_or("missing startxref")?;
    let xref_at: usize = text[startxref_at + 9..]
        .trim()
        .lines()
        .next()
        .ok_or("startxref has no offset")?
        .trim()
        .parse()
        .map_err(|_| "startxref offset is not a number".to_string())?;
    if !text[xref_at..].starts_with("xref") {
        return Err(format!("no xref table at byte {xref_at}"));
    }
    let table = text[xref_at + 4..].trim_start();
    let mut lines = table.lines();
    let header = lines.next().ok_or("empty xref table")?.trim();
    let mut header_parts = header.split_whitespace();
    let first: usize = header_parts
        .next()
        .ok_or("xref header has no first object")?
        .parse()
        .map_err(|_| "xref first object is not a number".to_string())?;
    let count: usize = header_parts
        .next()
        .ok_or("xref header has no count")?
        .parse()
        .map_err(|_| "xref count is not a number".to_string())?;
    if first != 0 || count == 0 {
        return Err(format!("unexpected xref range {first}..{}", first + count));
    }
    let mut offsets: Vec<Option<usize>> = Vec::with_capacity(count);
    for (i, line) in lines.take(count).enumerate() {
        let entry = line.trim_end();
        let mut parts = entry.split_whitespace();
        let offset = parts
            .next()
            .ok_or_else(|| format!("xref entry {i} is empty"))?;
        let _generation = parts
            .next()
            .ok_or_else(|| format!("xref entry {i} has no generation"))?;
        let kind = parts
            .next()
            .ok_or_else(|| format!("xref entry {i} has no type"))?;
        let offset: usize = offset
            .parse()
            .map_err(|_| format!("xref entry {i}: bad offset {offset}"))?;
        match kind {
            "f" => offsets.push(None),
            "n" => offsets.push(Some(offset)),
            other => return Err(format!("xref entry {i}: bad type {other}")),
        }
    }
    if offsets.len() != count {
        return Err(format!(
            "xref declares {count} entries, found {}",
            offsets.len()
        ));
    }

    // Every object must start where the xref says, and must be the object the
    // xref claims it is.
    let mut bodies: HashMap<usize, &str> = HashMap::new();
    for (index, offset) in offsets.iter().enumerate() {
        let Some(offset) = *offset else {
            continue;
        };
        let head = text
            .get(offset..)
            .ok_or_else(|| format!("object {index}: offset {offset} is past the end"))?;
        let expect = format!("{index} 0 obj");
        if !head.starts_with(&expect) {
            return Err(format!(
                "object {index}: byte {offset} holds {:?}, not {expect:?}",
                &head[..head.len().min(16)]
            ));
        }
        let body_start = offset + expect.len();
        let body_end = text[body_start..]
            .find("endobj")
            .ok_or_else(|| format!("object {index}: no endobj"))?
            + body_start;
        bodies.insert(index, &text[body_start..body_end]);
    }

    let trailer_at = text.find("trailer").ok_or("missing trailer")?;
    let trailer = &text[trailer_at..startxref_at];
    let root = trailer
        .split("/Root")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("trailer has no usable /Root")?;
    let catalog = bodies.get(&root).ok_or("catalogue object is missing")?;
    if !catalog.contains("/Type /Catalog") {
        return Err(format!("object {root} is not a catalogue"));
    }
    let pages_ref = catalog
        .split("/Pages")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("catalogue has no /Pages")?;
    let pages = bodies
        .get(&pages_ref)
        .ok_or("page tree object is missing")?;
    if !pages.contains("/Type /Pages") {
        return Err(format!("object {pages_ref} is not a page tree"));
    }
    let kids_array = pages
        .split("/Kids")
        .nth(1)
        .and_then(|s| s.split('[').nth(1))
        .and_then(|s| s.split(']').next())
        .ok_or("page tree has no /Kids array")?;
    let tokens: Vec<&str> = kids_array.split_whitespace().collect();
    let mut kids: Vec<usize> = Vec::new();
    for reference in tokens.chunks(3) {
        // Each kid is an indirect reference `object generation R`; a strict
        // reader refuses anything else rather than guessing at the number.
        match reference {
            [object, "0", "R"] => kids.push(
                object
                    .parse()
                    .map_err(|_| format!("page tree: bad kid reference {object}"))?,
            ),
            _ => return Err(format!("page tree: malformed kid reference {reference:?}")),
        }
    }
    let declared: usize = pages
        .split("/Count")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or("page tree has no /Count")?;
    if kids.len() != declared || kids.is_empty() {
        return Err(format!(
            "/Count {declared} does not match {} kids",
            kids.len()
        ));
    }

    let mut media_box = (0.0f32, 0.0f32);
    let mut fills = 0usize;
    let mut content_bytes = 0usize;
    for (i, kid) in kids.iter().enumerate() {
        let page = bodies
            .get(kid)
            .ok_or_else(|| format!("page {i}: object {kid} is missing"))?;
        if !page.contains("/Type /Page") {
            return Err(format!("page {i}: object {kid} is not a page"));
        }
        let media = page
            .split("/MediaBox")
            .nth(1)
            .and_then(|s| s.split('[').nth(1))
            .and_then(|s| s.split(']').next())
            .ok_or_else(|| format!("page {i} has no /MediaBox"))?;
        let numbers: Vec<f32> = media
            .split_whitespace()
            .filter_map(|t| t.parse::<f32>().ok())
            .collect();
        if numbers.len() != 4 {
            return Err(format!("page {i}: /MediaBox needs four numbers"));
        }
        if numbers[2] <= numbers[0] || numbers[3] <= numbers[1] {
            return Err(format!("page {i}: /MediaBox has no area"));
        }
        if i == 0 {
            media_box = (numbers[2] - numbers[0], numbers[3] - numbers[1]);
        }
        let contents = page
            .split("/Contents")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| format!("page {i} has no /Contents"))?;
        let stream_object = bodies
            .get(&contents)
            .ok_or_else(|| format!("page {i}: content object {contents} is missing"))?;
        let declared_length: usize = stream_object
            .split("/Length")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("page {i}: content stream has no /Length"))?;
        let stream_at = stream_object
            .find("stream\n")
            .ok_or_else(|| format!("page {i}: content object has no stream"))?
            + "stream\n".len();
        let after = &stream_object[stream_at..];
        // The declared length covers the content only — not the `endstream`
        // keyword that follows it, which is exactly where an off-by-ten hides.
        let stream_end = after
            .find("endstream")
            .ok_or_else(|| format!("page {i}: content object has no endstream"))?;
        let stream = &after[..stream_end];
        let actual = stream.len();
        if actual != declared_length {
            return Err(format!(
                "page {i}: /Length {declared_length} but the stream holds {actual} bytes"
            ));
        }
        // An empty stream is a blank page, which is legal; a non-empty one must
        // end with a complete drawing operation (`Q` restores, `f*` fills).
        let body = stream.trim_end();
        if !body.is_empty() && !body.ends_with('Q') && !body.ends_with("f*") {
            return Err(format!("page {i}: content stream ends mid-operation"));
        }
        fills += stream.matches("f*\n").count();
        content_bytes += actual;
    }

    Ok(PdfSummary {
        version,
        objects: bodies.len(),
        pages: kids.len(),
        media_box,
        fills,
        content_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::export::{artwork_from_svg, Artwork};
    use crate::sheet::{GridLayout, IconInput, IconMetrics, Placement, SheetPlan, SheetSpec};

    fn metrics(side: f32) -> IconMetrics {
        IconMetrics {
            ink_x: 0.0,
            ink_y: 0.0,
            ink_w: side,
            ink_h: side,
            ink_area: (side * side * 0.5) as u32,
            centroid_x: side * 0.5,
            centroid_y: side * 0.5,
            stroke: 4.0,
            solidity: 0.5,
        }
    }

    fn plan_of(n: u32, spec: SheetSpec) -> SheetPlan {
        let icons: Vec<IconInput> = (1..=n)
            .map(|id| IconInput {
                id,
                metrics: metrics(20.0),
            })
            .collect();
        SheetPlan::new(&icons, spec)
    }

    fn artwork_of(n: u32) -> Vec<Artwork> {
        (1..=n)
            .map(|id| {
                artwork_from_svg(
                    id,
                    format!("icon-{id:03}"),
                    "<svg><path d=\"M0,0L20,0C24,4 24,16 20,20L0,20Z\" fill=\"#3366ff\"/></svg>",
                )
                .expect("artwork")
            })
            .collect()
    }

    fn grid_spec() -> SheetSpec {
        SheetSpec {
            columns: 2,
            cell: 64,
            padding: 8,
            gap: 8,
            margin: 8,
            ink_ratio: 0.8,
            placement: Placement::Center,
        }
    }

    #[test]
    fn writes_a_document_its_own_validator_accepts() {
        let plan = plan_of(4, grid_spec());
        let art = artwork_of(4);
        let bytes = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let summary = validate_pdf(&bytes).expect("validates");
        assert_eq!(summary.version, "%PDF-1.7");
        assert_eq!(summary.pages, 1);
        assert_eq!(summary.objects, 5);
        // Four icons over two columns: a 152 × 152 px sheet at 0.75 pt/px.
        assert!((summary.media_box.0 - 114.0).abs() < 0.01, "{summary:?}");
        assert!((summary.media_box.1 - 114.0).abs() < 0.01);
        assert_eq!(summary.fills, 4);
        assert!(summary.content_bytes > 200, "{summary:?}");
    }

    #[test]
    fn every_icon_is_filled_with_its_own_colour_and_placed_where_the_plan_says() {
        let plan = plan_of(2, grid_spec());
        let art = vec![
            artwork_from_svg(
                1,
                "a",
                "<svg><path d=\"M0,0L4,0L4,4Z\" fill=\"#ff0000\"/></svg>",
            )
            .expect("artwork"),
            artwork_from_svg(
                2,
                "b",
                "<svg><path d=\"M0,0L4,0L4,4Z\" fill=\"#00ff00\"/></svg>",
            )
            .expect("artwork"),
        ];
        let bytes = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        let text = std::str::from_utf8(&bytes).expect("ascii");
        assert!(text.contains("1 0 0 rg"), "red fill missing");
        assert!(text.contains("0 1 0 rg"), "green fill missing");
        // The first icon's ink box starts at 8 + 12.8 = 20.8 px ⇒ 15.6 pt, and
        // its y is flipped through the 60 pt page: 60 − 15.6 = 44.4.
        assert!(text.contains("15.6 44.4 m"), "placement missing: {text}");
        // The second cell is one step to the right (72 px = 54 pt).
        assert!(text.contains("69.6 44.4 m"));
    }

    #[test]
    fn a_background_is_painted_first() {
        let plan = plan_of(1, grid_spec());
        let art = artwork_of(1);
        let bytes = write_sheet_pdf(
            &plan,
            &art,
            &PdfOptions {
                background: Some([255, 255, 255, 255]),
                ..PdfOptions::default()
            },
        )
        .expect("writes");
        let text = std::str::from_utf8(&bytes).expect("ascii");
        let background = text.find("1 1 1 rg").expect("background");
        let icon = text.find("0.2 0.4 1 rg").expect("icon fill");
        assert!(background < icon, "the background must come first");
        validate_pdf(&bytes).expect("still valid");
    }

    #[test]
    fn the_file_is_byte_identical_between_runs() {
        let plan = plan_of(3, grid_spec());
        let art = artwork_of(3);
        let a = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        let b = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        assert_eq!(a, b);
    }

    #[test]
    fn the_validator_catches_a_broken_xref_and_a_short_stream() {
        let plan = plan_of(2, grid_spec());
        let art = artwork_of(2);
        let good = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");

        // A truncated file: no %%EOF at all.
        let mut truncated = good.clone();
        truncated.truncate(good.len() - 8);
        assert!(validate_pdf(&truncated).is_err());

        let text = String::from_utf8(good).expect("ascii");

        // A wrong xref offset: the first object's byte position is shifted by
        // one, which a lenient reader would silently accept as a blank page.
        let first_object = text.find("1 0 obj").expect("the first object");
        let broken = text.replacen(
            &format!("{first_object:010} 00000 n"),
            &format!("{:010} 00000 n", first_object + 1),
            1,
        );
        assert!(broken != text, "the fixture must actually change");
        assert!(validate_pdf(broken.as_bytes()).is_err());

        // A wrong /Length: the reader would read into the next object.
        let length_at = text.find("/Length ").expect("a stream length") + "/Length ".len();
        let length: usize = text[length_at..]
            .split_whitespace()
            .next()
            .expect("a number")
            .parse()
            .expect("a number");
        let broken = text.replacen(
            &format!("/Length {length}"),
            &format!("/Length {}", length + 1),
            1,
        );
        assert!(broken != text, "the fixture must actually change");
        assert!(validate_pdf(broken.as_bytes()).is_err());
    }

    #[test]
    fn a_pdf_is_refused_when_the_plan_or_the_scale_is_impossible() {
        let plan = plan_of(1, grid_spec());
        let art = artwork_of(1);
        let err = write_sheet_pdf(
            &plan,
            &art,
            &PdfOptions {
                scale: 0.0,
                ..PdfOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::BadGeometry(_)));
        let empty = SheetPlan::new(&[], grid_spec());
        let err = write_sheet_pdf(&empty, &[], &PdfOptions::default());
        // An empty sheet is still a sheet (one empty page) — a zero *size* is not.
        assert!(err.is_ok(), "{err:?}");
    }

    #[test]
    fn a_missing_icon_is_refused() {
        let plan = plan_of(2, grid_spec());
        let art = artwork_of(1);
        let err = write_sheet_pdf(&plan, &art, &PdfOptions::default()).unwrap_err();
        assert_eq!(err, ExportError::MissingArtwork { id: 2 });
    }

    #[test]
    fn an_icon_with_no_shapes_costs_nothing_but_keeps_its_cell() {
        let plan = plan_of(2, grid_spec());
        let art = vec![
            artwork_from_svg(1, "a", "<svg><path d=\"M0,0L4,0L4,4Z\"/></svg>").expect("artwork"),
            Artwork::empty(2, "blank"),
        ];
        let bytes = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        let summary = validate_pdf(&bytes).expect("validates");
        assert_eq!(summary.fills, 1);
    }

    #[test]
    fn text_is_escaped_and_transliterated() {
        assert_eq!(pdf_string("a(b)c\\d"), "a\\(b\\)c\\\\d");
        assert_eq!(pdf_string("café\nx"), "caf? x");
        let plan = plan_of(1, grid_spec());
        let art = artwork_of(1);
        let bytes = write_sheet_pdf(
            &plan,
            &art,
            &PdfOptions {
                title: "sheet (v2) — café".to_string(),
                ..PdfOptions::default()
            },
        )
        .expect("writes");
        validate_pdf(&bytes).expect("validates");
        let text = std::str::from_utf8(&bytes).expect("ascii");
        assert!(
            text.contains("(sheet \\(v2\\) ? caf?)"),
            "escaped title missing"
        );
    }

    #[test]
    fn a_multi_page_sheet_is_still_one_page_but_every_cell_follows_the_plan() {
        // 17 icons over 16 columns: two grid rows, one PDF page (the page is
        // sized to the sheet, which is what a viewer can open).
        let plan = plan_of(
            17,
            SheetSpec {
                columns: 16,
                ..grid_spec()
            },
        );
        let art = artwork_of(17);
        let bytes = write_sheet_pdf(&plan, &art, &PdfOptions::default()).expect("writes");
        let summary = validate_pdf(&bytes).expect("validates");
        assert_eq!(summary.pages, 1);
        assert_eq!(summary.fills, 17);
        assert_eq!(plan.layout.rows, 2);
    }

    #[test]
    fn a_grid_layout_is_not_needed_to_validate_but_a_zero_size_is_refused() {
        let mut plan = plan_of(1, grid_spec());
        plan.layout = GridLayout {
            columns: 0,
            rows: 0,
            width: 0,
            height: 0,
        };
        let art = artwork_of(1);
        let err = write_sheet_pdf(&plan, &art, &PdfOptions::default()).unwrap_err();
        assert!(matches!(err, ExportError::BadGeometry(_)));
    }
}
