//! §3.3 stage ⑦ — emit + validate.
//!
//! Turns the stage ⑤⑥ SVG fragment into the final per-icon document:
//! same-style paths are merged, the viewBox is normalized, and the
//! `<title>`/`<desc>`/`<metadata>` header is written. The document is then
//! re-parsed with `usvg` (part of the MPL-2.0 resvg stack, unmodified,
//! behind this adapter); a document that fails the re-parse is returned as
//! an error and is **never** handed to the caller as output. Geometry is
//! not touched here — coordinates are already quantized to 2 decimals by
//! stage ⑥ (`path_precision = 2`).
//!
//! "Flag and fall back" from the spec is caller policy (the W4 batch job
//! marks the icon failed and keeps serving the previous cached SVG); this
//! layer only refuses to emit.

use super::trace::IconVectors;

/// Cache/emit format version. Part of the stage ⑧ cache key and of the
/// emitted `<metadata>`; bumping it invalidates every cached payload.
pub const CACHE_VERSION: u32 = 1;

/// Stage ⑦ failure modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// The fragment contained no `<path>` elements at all.
    EmptyFragment,
    /// A `<path>` had an empty `d` (usvg would silently drop it).
    EmptyPath,
    /// The assembled document failed the `usvg` re-parse gate.
    Unparseable(String),
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmitError::EmptyFragment => f.write_str("fragment has no paths"),
            EmitError::EmptyPath => f.write_str("path element with empty d"),
            EmitError::Unparseable(e) => write!(f, "usvg rejected the document: {e}"),
        }
    }
}

impl std::error::Error for EmitError {}

/// A scanned `<path …/>` element: its `d` payload and its style signature
/// (every attribute except `d`, whitespace-normalized).
struct ScannedPath {
    d: String,
    style: String,
}

/// Finds `name="…"` in an attribute string; returns the value and the
/// `(start, end)` byte span of the whole `name="…"` chunk. A preceding
/// `-` or alphanumeric (as in `stroke-width=` for `width=`) is rejected.
fn extract_attr<'a>(attrs: &'a str, name: &str) -> Option<(&'a str, (usize, usize))> {
    let needle = format!("{name}=\"");
    let start = attrs
        .find(&needle)
        .filter(|&i| i == 0 || !attrs[..i].ends_with(|c: char| c.is_alphanumeric() || c == '-'))?;
    let vstart = start + needle.len();
    let vend = attrs[vstart..].find('"')? + vstart;
    Some((&attrs[vstart..vend], (start, vend + 1)))
}

/// Extracts every `<path …/>` element from a vtracer fragment. Anything
/// that is not a `<path` element is dropped (the emitted document is
/// rebuilt from scratch), and a `d`-less path is recorded with an empty
/// `d` so the caller can reject it instead of letting usvg silently lose
/// geometry.
fn scan_paths(fragment: &str) -> Vec<ScannedPath> {
    let mut out = Vec::new();
    let mut rest = fragment;
    while let Some(pos) = rest.find("<path") {
        let after = &rest[pos + 5..];
        let Some(gt) = after.find('>') else {
            break;
        };
        let inner = after[..gt].trim();
        let (d, style) = match extract_attr(inner, "d") {
            Some((d, span)) => {
                let mut s = String::with_capacity(inner.len() + 1);
                s.push_str(inner[..span.0].trim());
                s.push(' ');
                s.push_str(inner[span.1..].trim());
                (
                    d.to_string(),
                    s.trim().trim_end_matches('/').trim().to_string(),
                )
            }
            None => (
                String::new(),
                inner.trim_end_matches('/').trim().to_string(),
            ),
        };
        out.push(ScannedPath { d, style });
        rest = &after[gt + 1..];
    }
    out
}

/// Merges runs of paths that share a style signature; returns the merged
/// fragment as canonical `<path d="…" style/>` elements in input order.
fn merge_same_style(fragment: &str) -> Result<String, EmitError> {
    let scanned = scan_paths(fragment);
    if scanned.is_empty() {
        return Err(EmitError::EmptyFragment);
    }
    let mut out = String::new();
    let mut i = 0;
    while i < scanned.len() {
        let style = scanned[i].style.clone();
        let mut d = String::with_capacity(64);
        while i < scanned.len() && scanned[i].style == style {
            if scanned[i].d.is_empty() {
                return Err(EmitError::EmptyPath);
            }
            if !d.is_empty() {
                d.push(' ');
            }
            d.push_str(&scanned[i].d);
            i += 1;
        }
        out.push_str("<path d=\"");
        out.push_str(&d);
        out.push('"');
        if !style.is_empty() {
            out.push(' ');
            out.push_str(&style);
        }
        out.push_str("/>");
    }
    Ok(out)
}

/// Re-parses `doc` with usvg. This is the stage ⑦ gate: a document that
/// does not parse can never leave the pipeline.
pub fn validate(doc: &str) -> Result<(), EmitError> {
    let tree = resvg::usvg::Tree::from_str(doc, &resvg::usvg::Options::default())
        .map_err(|e| EmitError::Unparseable(e.to_string()))?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return Err(EmitError::Unparseable(
            "document has non-positive size".to_string(),
        ));
    }
    Ok(())
}

/// Builds the final document for one icon (viewBox `0 0 w h`, deterministic
/// header, merged paths) and validates it through the usvg gate.
pub fn emit_svg(
    v: &IconVectors,
    w: u32,
    h: u32,
    preset: &str,
    stroke_only: bool,
) -> Result<String, EmitError> {
    let body = merge_same_style(&v.svg)?;
    let mut doc = String::with_capacity(body.len() + 176);
    doc.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\"");
    doc.push_str(&format!(
        " width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">"
    ));
    doc.push_str("<title>icon</title>");
    if stroke_only {
        doc.push_str(&format!(
            "<desc>Icon Forge preset={preset} stroke-only</desc>"
        ));
    } else {
        doc.push_str(&format!("<desc>Icon Forge preset={preset}</desc>"));
    }
    doc.push_str(&format!(
        "<metadata>icon-forge v{CACHE_VERSION} preset={preset} paths={}</metadata>",
        body.matches("<path").count()
    ));
    doc.push_str(&body);
    doc.push_str("</svg>");
    validate(&doc)?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = "<path d=\"M4,4L12,4L12,12L4,12Z\" fill=\"#0a0a0a\"/>";
    const SMALL: &str = "<path d=\"M0,0L2,0L2,2Z\" fill=\"#0a0a0a\"/>";
    const RED: &str = "<path d=\"M1,1L3,1L3,3Z\" fill=\"#ff0000\"/>";

    fn vectors(fragment: &str) -> IconVectors {
        IconVectors {
            svg: fragment.to_string(),
            palette: Vec::new(),
        }
    }

    #[test]
    fn emits_valid_document_through_usvg_gate() {
        let doc = emit_svg(&vectors(SQUARE), 16, 16, "mono-clean", false).unwrap();
        assert!(
            doc.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
            "{doc}"
        );
        assert!(doc.contains("viewBox=\"0 0 16 16\""));
        assert!(doc.contains("<title>icon</title>"));
        assert!(doc.contains("<desc>Icon Forge preset=mono-clean</desc>"));
        assert!(doc.contains("<metadata>icon-forge v1 preset=mono-clean paths=1</metadata>"));
        assert!(doc.ends_with("</svg>"));
        validate(&doc).unwrap();
    }

    #[test]
    fn same_style_paths_are_merged_in_order() {
        let frag = format!("{SQUARE}{SMALL}");
        let doc = emit_svg(&vectors(&frag), 16, 16, "mono-clean", false).unwrap();
        assert_eq!(doc.matches("<path").count(), 1, "{doc}");
        assert!(
            doc.contains("d=\"M4,4L12,4L12,12L4,12Z M0,0L2,0L2,2Z\""),
            "{doc}"
        );
    }

    #[test]
    fn different_styles_stay_separate() {
        let frag = format!("{SQUARE}{RED}");
        let doc = emit_svg(&vectors(&frag), 16, 16, "flat-8", false).unwrap();
        assert_eq!(doc.matches("<path").count(), 2, "{doc}");
        assert!(doc.contains("fill=\"#0a0a0a\""));
        assert!(doc.contains("fill=\"#ff0000\""));
    }

    #[test]
    fn empty_fragment_is_rejected() {
        let err = emit_svg(&vectors(""), 16, 16, "mono-clean", false).unwrap_err();
        assert_eq!(err, EmitError::EmptyFragment);
    }

    #[test]
    fn path_with_empty_d_is_rejected() {
        let frag = "<path d=\"\" fill=\"#000000\"/>";
        let err = emit_svg(&vectors(frag), 16, 16, "mono-clean", false).unwrap_err();
        assert_eq!(err, EmitError::EmptyPath);
    }

    #[test]
    fn unparseable_document_is_never_returned() {
        // '<' inside an attribute value is a hard XML error.
        let frag = "<path d=\"<\" fill=\"#000000\"/>";
        let err = emit_svg(&vectors(frag), 16, 16, "mono-clean", false).unwrap_err();
        assert!(matches!(err, EmitError::Unparseable(_)), "{err:?}");
    }

    #[test]
    fn emit_is_byte_deterministic() {
        let frag = format!("{SQUARE}{SMALL}");
        let a = emit_svg(&vectors(&frag), 16, 16, "flat-8", true).unwrap();
        let b = emit_svg(&vectors(&frag), 16, 16, "flat-8", true).unwrap();
        assert_eq!(a, b);
        assert!(a.contains("stroke-only"), "{a}");
    }
}
