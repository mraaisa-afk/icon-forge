//! # Sheet generator — §3.5 auto-leveling and the sheet a designer ships
//!
//! Phase 5 turns a library of vectorized icons into the artifact the workflow
//! actually needs: one evenly-leveled sheet, plus the metadata that goes with
//! it (names, slugs, tags) and the exports (SVG, PDF, PNG, CSV).
//!
//! The module is split by what a function needs to know:
//!
//! * [`layout`] — where the cells are. Pure arithmetic.
//! * [`metrics`] — what an icon's ink looks like, measured from its mask.
//! * [`level`] — §3.5 auto-leveling: the scale, the placement, and the report
//!   that explains both (`ink_size_cv`, `stroke_cv`, baseline spread).
//! * [`export`] — writers for SVG, PDF, PNG, CSV and the manifest.
//!
//! Everything above `export` is dependency-free on purpose (std and
//! `isg_core` only): the arithmetic that decides how big every icon is has to
//! be the same on every platform, has to be fast enough to run live under a
//! slider, and has to be testable without decoding an image. Only the exporters
//! reach for the raster and SVG crates.
//!
//! ## The plan
//!
//! [`plan`] is the whole computation: icons in (id + measured ink), a
//! [`SheetSpec`] in, and a [`SheetPlan`] out — sheet size, one placement per
//! icon in reading order, and the leveling report. The native exporter, the
//! Tauri command and (through a mirrored fixture) the webview preview all call
//! *this* function, so nothing anywhere else has to re-derive where an icon
//! goes.

pub mod export;
pub mod layout;
pub mod level;
pub mod metrics;

pub use layout::GridLayout;
pub use level::{
    cv, median, place, spread, IconInput, IconPlacement, LevelReport, Rect, FLAG_OVERFLOW_BACKOFF,
    FLAG_SOLIDITY_CLAMPED, FLAG_STROKE_CLAMPED,
};
pub use metrics::{measure, IconMetrics};

/// How the icons should be placed inside their cells (§3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Geometrically centred — the default, and what most sheets want.
    Center,
    /// Centred, then shifted by half the box-centre-to-centroid delta. Corrects
    /// top-heavy arrows and pins, which read as low when they are centred.
    OpticalCenter,
    /// Sitting on the inner box's baseline, so a row shares a bottom edge.
    Baseline,
}

impl Placement {
    /// Every mode, in the order the UI offers them.
    pub const ALL: [Self; 3] = [Self::Center, Self::OpticalCenter, Self::Baseline];

    /// Stable machine-readable name (the UI sends this back).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::OpticalCenter => "optical",
            Self::Baseline => "baseline",
        }
    }

    /// Parses [`Placement::as_str`]; anything unknown is the default.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        match name {
            "optical" => Self::OpticalCenter,
            "baseline" => Self::Baseline,
            _ => Self::Center,
        }
    }
}

/// The cell specification: everything the leveling arithmetic depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetSpec {
    /// Cell side in pixels (cells are square).
    pub cell: u32,
    /// Padding inside each cell, in pixels — the ink never enters it unless the
    /// icon overflows and is flagged.
    pub padding: u32,
    /// Gap between cells, in pixels.
    pub gap: u32,
    /// Margin around the whole sheet, in pixels.
    pub margin: u32,
    /// Cells per row.
    pub columns: u32,
    /// Fraction of the cell's inner box the ink should fill (`0..=1`); §3.5's
    /// example uses `0.80`.
    pub ink_ratio: f32,
    /// How the ink is anchored inside the cell.
    pub placement: Placement,
}

impl Default for SheetSpec {
    fn default() -> Self {
        Self {
            cell: 64,
            padding: 8,
            gap: 8,
            margin: 8,
            columns: 16,
            ink_ratio: 0.80,
            placement: Placement::Center,
        }
    }
}

impl SheetSpec {
    /// The inner box's side: `cell − 2·padding`, floored at zero.
    #[must_use]
    pub fn inner(&self) -> f32 {
        (self.cell as f32 - 2.0 * self.padding as f32).max(0.0)
    }

    /// Reads a spec back from the wire form the UI sends.
    ///
    /// Values are clamped rather than rejected: these are four sliders and a
    /// placement choice, and a slider cannot express something invalid. A cell
    /// smaller than its padding is the one combination that would make the
    /// inner box vanish, so padding is capped at half the cell.
    #[must_use]
    pub fn from_wire(
        cell: u32,
        padding: u32,
        gap: u32,
        margin: u32,
        columns: u32,
        ink_ratio: f32,
        placement: &str,
    ) -> Self {
        let cell = cell.clamp(8, 4096);
        Self {
            cell,
            padding: padding.min(cell / 2),
            gap: gap.min(4096),
            margin: margin.min(4096),
            columns: columns.clamp(1, 4096),
            ink_ratio: if ink_ratio.is_finite() {
                ink_ratio.clamp(0.05, 1.0)
            } else {
                0.80
            },
            placement: Placement::parse(placement),
        }
    }
}

/// A finished plan: the sheet, every placement, and the report.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetPlan {
    /// The spec this plan was computed from.
    pub spec: SheetSpec,
    /// The resolved grid.
    pub layout: GridLayout,
    /// One placement per icon, in reading order.
    pub placements: Vec<IconPlacement>,
    /// What the leveling did.
    pub report: LevelReport,
}

impl SheetPlan {
    /// Computes the plan for an already-measured set of icons.
    #[must_use]
    pub fn new(icons: &[IconInput], spec: SheetSpec) -> Self {
        let layout = GridLayout::solve(icons.len(), &spec);
        let (placements, report) = place(icons, &spec, &layout);
        Self {
            spec,
            layout,
            placements,
            report,
        }
    }

    /// The plan's sheet size in pixels.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.layout.width, self.layout.height)
    }

    /// The placement for one id, if the icon is on this sheet.
    #[must_use]
    pub fn placement(&self, id: u32) -> Option<&IconPlacement> {
        self.placements.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_spec_is_the_documented_example() {
        let spec = SheetSpec::default();
        // §3.5: (64 − 2·8) · 0.80 = 38.4 px of target ink.
        assert_eq!(spec.inner(), 48.0);
        assert!((spec.inner() * spec.ink_ratio - 38.4).abs() < 1e-5);
    }

    #[test]
    fn wire_values_are_clamped_not_rejected() {
        // A cell can never be smaller than its padding, or the inner box would
        // vanish and every icon would divide by zero.
        let spec = SheetSpec::from_wire(64, 500, 8, 8, 0, 5.0, "nonsense");
        assert_eq!(spec.padding, 32);
        assert_eq!(spec.columns, 1);
        assert_eq!(spec.ink_ratio, 1.0);
        assert_eq!(spec.placement, Placement::Center);
        // A NaN ratio falls back to the documented default.
        let spec = SheetSpec::from_wire(64, 8, 8, 8, 4, f32::NAN, "baseline");
        assert_eq!(spec.ink_ratio, 0.80);
        assert_eq!(spec.placement, Placement::Baseline);
    }

    #[test]
    fn placement_names_round_trip() {
        for p in Placement::ALL {
            assert_eq!(Placement::parse(p.as_str()), p);
        }
        assert_eq!(Placement::parse("optical"), Placement::OpticalCenter);
        assert_eq!(Placement::parse(""), Placement::Center);
    }

    #[test]
    fn a_plan_sizes_itself_from_the_icon_count() {
        let spec = SheetSpec {
            columns: 10,
            ..SheetSpec::default()
        };
        let icons: Vec<IconInput> = (0..25)
            .map(|id| IconInput {
                id,
                metrics: IconMetrics {
                    ink_x: 0.0,
                    ink_y: 0.0,
                    ink_w: 20.0,
                    ink_h: 20.0,
                    ink_area: 400,
                    centroid_x: 10.0,
                    centroid_y: 10.0,
                    stroke: 4.0,
                    solidity: 0.8,
                },
            })
            .collect();
        let plan = SheetPlan::new(&icons, spec);
        assert_eq!(plan.layout.rows, 3);
        assert_eq!(plan.placements.len(), 25);
        assert_eq!(plan.size().0, 2 * 8 + 10 * 64 + 9 * 8);
        assert_eq!(
            plan.placement(24).map(|p| p.cell),
            Some((8 + 4 * 72, 8 + 2 * 72))
        );
        assert_eq!(plan.placement(99), None);
    }
}
