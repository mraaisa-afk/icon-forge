//! §3.6 detector 1 — the quality flags.
//!
//! Three ways an icon can be *technically* fine and *actually* unusable:
//!
//! * **`LowQuality`** — the composite score (§3.3 stage ⑧) is below `0.80`,
//!   the line the spec draws for "a human would re-trace this by hand".
//! * **`OverComplex`** — the traced outline carries more nodes than the shape
//!   can justify: more than `4·√area` of them. A 40×40 filled square is
//!   1600 px² of ink, so its budget is 160 nodes; a square needs four, and a
//!   trace that spends a hundred is following noise, not geometry.
//! * **`OpenContour`** — a subpath was never closed. The fill still renders
//!   (SVG closes a fill implicitly), but the *outline* does not, so a stroke
//!   pass or an editor round-trip shows a hairline notch. On a sheet it is
//!   nearly always a segmentation defect at an icon's edge rather than a
//!   deliberate design.
//!
//! The three thresholds live here as constants because they are also what the
//! review UI prints next to a flag and what the Phase-6 gate asserts against —
//! a number typed inline at the call site could drift away from both.

/// Composite score below which an icon is flagged [`QualityFlag::LowQuality`].
pub const LOW_QUALITY_COMPOSITE: f32 = 0.80;

/// Node budget per icon: `4·√area` (§3.6).
pub const NODES_PER_SQRT_AREA: f32 = 4.0;

/// One reason an icon needs a human look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityFlag {
    /// Composite below [`LOW_QUALITY_COMPOSITE`].
    LowQuality,
    /// Node count above [`node_budget`] for the icon's ink area.
    OverComplex,
    /// At least one subpath is not closed.
    OpenContour,
}

impl QualityFlag {
    /// The stable name used by the CSV, the log lines and the UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LowQuality => "low-quality",
            Self::OverComplex => "over-complex",
            Self::OpenContour => "open-contour",
        }
    }

    /// Parses [`QualityFlag::as_str`] back.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "low-quality" => Some(Self::LowQuality),
            "over-complex" => Some(Self::OverComplex),
            "open-contour" => Some(Self::OpenContour),
            _ => None,
        }
    }
}

/// What the quality detector reads per icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QualityInput {
    /// The plan's id for the icon.
    pub id: u32,
    /// Composite score from the vectorizer's stage ⑧, in `[0, 1]`.
    pub composite: f32,
    /// Segments in the traced outline: every line and cubic, summed over the
    /// icon's subpaths (an arc counts as the cubics it was emitted as).
    pub node_count: u32,
    /// Ink area in pixels, from the §3.5 mask metrics.
    pub ink_area: u64,
    /// True when every subpath of every shape is closed.
    pub closed: bool,
}

/// The node budget for a given ink area: `4·√area`.
#[must_use]
pub fn node_budget(ink_area: u64) -> f32 {
    NODES_PER_SQRT_AREA * (ink_area as f32).sqrt()
}

/// Every flag `input` earns, in a fixed order: low-quality, over-complex,
/// open-contour.
///
/// The order is stable so the UI can render a row of badges without sorting,
/// and so a log line is byte-comparable between runs.
#[must_use]
pub fn flags(input: &QualityInput) -> Vec<QualityFlag> {
    let mut out = Vec::new();
    if input.composite < LOW_QUALITY_COMPOSITE {
        out.push(QualityFlag::LowQuality);
    }
    // An icon with no ink has nothing to be over-complex *about*, and its
    // budget is zero — without this guard every empty trace would be flagged.
    if input.ink_area > 0 && f64::from(input.node_count) > f64::from(node_budget(input.ink_area)) {
        out.push(QualityFlag::OverComplex);
    }
    if !input.closed {
        out.push(QualityFlag::OpenContour);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(composite: f32, node_count: u32, ink_area: u64, closed: bool) -> QualityInput {
        QualityInput {
            id: 1,
            composite,
            node_count,
            ink_area,
            closed,
        }
    }

    #[test]
    fn the_composite_threshold_is_exclusive() {
        assert_eq!(
            flags(&input(0.799, 4, 1600, true)),
            vec![QualityFlag::LowQuality]
        );
        assert!(flags(&input(0.80, 4, 1600, true)).is_empty());
        assert!(flags(&input(0.95, 4, 1600, true)).is_empty());
    }

    #[test]
    fn the_node_budget_is_four_sqrt_area() {
        // A 1600 px² square: 4·40 = 160 nodes.
        assert!((node_budget(1600) - 160.0).abs() < 1e-3);
        assert!(flags(&input(0.9, 160, 1600, true)).is_empty());
        assert_eq!(
            flags(&input(0.9, 161, 1600, true)),
            vec![QualityFlag::OverComplex]
        );
    }

    #[test]
    fn an_empty_icon_is_never_over_complex() {
        assert!(flags(&input(0.9, 3, 0, true)).is_empty());
    }

    #[test]
    fn an_open_contour_always_flags() {
        assert_eq!(
            flags(&input(0.99, 4, 1600, false)),
            vec![QualityFlag::OpenContour]
        );
    }

    #[test]
    fn every_flag_combines_in_a_fixed_order() {
        assert_eq!(
            flags(&input(0.5, 500, 100, false)),
            vec![
                QualityFlag::LowQuality,
                QualityFlag::OverComplex,
                QualityFlag::OpenContour
            ]
        );
    }

    #[test]
    fn names_round_trip() {
        for flag in [
            QualityFlag::LowQuality,
            QualityFlag::OverComplex,
            QualityFlag::OpenContour,
        ] {
            assert_eq!(QualityFlag::parse(flag.as_str()), Some(flag));
        }
        assert_eq!(QualityFlag::parse("nonsense"), None);
    }
}
