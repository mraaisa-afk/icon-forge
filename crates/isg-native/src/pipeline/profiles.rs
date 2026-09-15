//! §3.3-⑤ Trace profiles — the 7-preset table.
//!
//! Frozen [`TracePreset`] ordinals (isg-core) map 1:1 onto the §3.3-⑤ doc
//! names, which double as the UI labels behind the 1–5 quality slider:
//!
//! | preset (ordinal) | doc name | slider | mode |
//! |---|---|---|---|
//! | `Draft` (1) | `mono-fast` | 1 | BW, polygon fit, heavy speckle filter |
//! | `Wireframe` (2) | `mono-clean` | 2 | BW + Bradley–Roth adaptive threshold |
//! | `Lineart` (3) | `scan` | 3 | BW + adaptive, preserves 1-px features |
//! | `Balanced` (4) | `flat-8` | 3 | colour, ~8-colour palette, stacked |
//! | `Detailed` (5) | `flat-cutout` | 4 | colour, cutout hierarchy, seam-free |
//! | `HighFidelity` (6) | `detailed` | 5 | colour, near-lossless splines |
//! | `Pixel` (7) | `pixel-art` | 5 | colour, exact pixel-lattice fit |
//!
//! vtracer's own `simplify` stays off everywhere: §3.3-⑥ mandates our custom
//! geometry pass ([`super::simplify`]), not the paper.js-style one.

use isg_core::TracePreset;
use vtracer::{Clustering, Config, FitMode, Hierarchical, Preset};

use super::simplify::SimplifyParams;

/// Everything stage ⑤/⑥ need to trace one icon for a preset.
pub struct TraceProfile {
    /// Frozen preset this profile was built from.
    pub preset: TracePreset,
    /// §3.3-⑤ doc name (UI label).
    pub doc_name: &'static str,
    /// UI quality-slider level (1–5).
    pub slider: u8,
    /// Colour mode (stage ④ quantize feeds the palette) vs binary.
    pub colour: bool,
    /// Outline-only intent (Wireframe); honoured by the W3 emit stage.
    pub stroke_only: bool,
    /// Stage ④ palette target `k_max` (colour presets only).
    pub k: Option<u32>,
    /// Stage ⑥ custom simplify parameters.
    pub simplify: SimplifyParams,
}

impl TraceProfile {
    /// Builds the vtracer configuration for this profile. The colour
    /// palette (stage ④ output) is injected by the caller before `build`.
    #[must_use]
    pub fn vtracer_config(&self) -> Config {
        let mut cfg = Config::from_preset(if self.colour {
            Preset::Poster
        } else {
            Preset::Bw
        });
        match self.preset {
            TracePreset::Draft => {
                cfg.mode = FitMode::Polygon;
                cfg.filter_speckle = 8;
                cfg.corner_threshold = 90;
                cfg.length_threshold = 8.0;
            }
            TracePreset::Wireframe => {
                cfg.mode = FitMode::Spline;
                cfg.filter_speckle = 4;
                cfg.corner_threshold = 60;
                cfg.length_threshold = 4.0;
                cfg.binary_adaptive = true;
            }
            TracePreset::Lineart => {
                cfg.mode = FitMode::Spline;
                cfg.filter_speckle = 2;
                cfg.corner_threshold = 45;
                cfg.length_threshold = 2.0;
                cfg.binary_adaptive = true;
            }
            TracePreset::Balanced => {
                cfg.mode = FitMode::Spline;
                cfg.hierarchical = Hierarchical::Stacked;
                cfg.filter_speckle = 4;
                cfg.corner_threshold = 60;
                cfg.length_threshold = 4.0;
            }
            TracePreset::Detailed => {
                cfg.mode = FitMode::Spline;
                cfg.hierarchical = Hierarchical::Cutout;
                cfg.filter_speckle = 2;
                cfg.corner_threshold = 60;
                cfg.length_threshold = 3.0;
            }
            TracePreset::HighFidelity => {
                cfg.mode = FitMode::Spline;
                cfg.hierarchical = Hierarchical::Cutout;
                cfg.filter_speckle = 1;
                cfg.corner_threshold = 45;
                cfg.length_threshold = 2.0;
                cfg.max_iterations = 10;
            }
            TracePreset::Pixel => {
                cfg.mode = FitMode::Pixel;
                cfg.hierarchical = Hierarchical::Stacked;
                cfg.filter_speckle = 1;
            }
        }
        cfg.clustering = if self.colour {
            Clustering::ColorCluster
        } else {
            Clustering::Binary
        };
        cfg
    }
}

/// The canonical preset → profile table (§3.3-⑤).
#[must_use]
pub fn profile(preset: TracePreset) -> TraceProfile {
    match preset {
        TracePreset::Draft => TraceProfile {
            preset,
            doc_name: "mono-fast",
            slider: 1,
            colour: false,
            stroke_only: false,
            k: None,
            simplify: SimplifyParams {
                rdp_px: 0.6,
                vis_area_px2: 0.8,
                ..SimplifyParams::default()
            },
        },
        TracePreset::Wireframe => TraceProfile {
            preset,
            doc_name: "mono-clean",
            slider: 2,
            colour: false,
            stroke_only: true,
            k: None,
            simplify: SimplifyParams::default(),
        },
        TracePreset::Lineart => TraceProfile {
            preset,
            doc_name: "scan",
            slider: 3,
            colour: false,
            stroke_only: false,
            k: None,
            simplify: SimplifyParams {
                rdp_px: 0.25,
                vis_area_px2: 0.3,
                ..SimplifyParams::default()
            },
        },
        TracePreset::Balanced => TraceProfile {
            preset,
            doc_name: "flat-8",
            slider: 3,
            colour: true,
            stroke_only: false,
            k: Some(8),
            simplify: SimplifyParams::default(),
        },
        TracePreset::Detailed => TraceProfile {
            preset,
            doc_name: "flat-cutout",
            slider: 4,
            colour: true,
            stroke_only: false,
            k: Some(12),
            simplify: SimplifyParams {
                rdp_px: 0.3,
                vis_area_px2: 0.35,
                ..SimplifyParams::default()
            },
        },
        TracePreset::HighFidelity => TraceProfile {
            preset,
            doc_name: "detailed",
            slider: 5,
            colour: true,
            stroke_only: false,
            k: Some(24),
            simplify: SimplifyParams {
                rdp_px: 0.15,
                vis_area_px2: 0.15,
                ..SimplifyParams::default()
            },
        },
        TracePreset::Pixel => TraceProfile {
            preset,
            doc_name: "pixel-art",
            slider: 5,
            colour: true,
            stroke_only: false,
            k: Some(32),
            // Pixel lattices must stay exact: every geometry pass off.
            simplify: SimplifyParams {
                collinear_deg: 0.0,
                corner_turn_deg: 90.0,
                rdp_px: 0.0,
                vis_area_px2: 0.0,
                snap_px: 0.0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TracePreset; 7] = [
        TracePreset::Draft,
        TracePreset::Wireframe,
        TracePreset::Lineart,
        TracePreset::Balanced,
        TracePreset::Detailed,
        TracePreset::HighFidelity,
        TracePreset::Pixel,
    ];

    #[test]
    fn table_is_complete_and_distinct() {
        let mut names: Vec<&str> = ALL.iter().map(|&p| profile(p).doc_name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 7, "each preset maps to its own doc name");
        for &p in &ALL {
            let prof = profile(p);
            assert!(
                (1..=5).contains(&prof.slider),
                "{} slider {}",
                prof.doc_name,
                prof.slider
            );
            assert_eq!(prof.colour, prof.k.is_some(), "{}", prof.doc_name);
        }
    }

    #[test]
    fn wireframe_is_the_only_stroke_only_profile() {
        for &p in &ALL {
            assert_eq!(profile(p).stroke_only, p == TracePreset::Wireframe);
        }
    }

    #[test]
    fn bw_presets_use_binary_clustering() {
        let cfg = profile(TracePreset::Draft).vtracer_config();
        assert_eq!(cfg.clustering, Clustering::Binary);
        assert_eq!(cfg.mode, FitMode::Polygon);
        assert!(!cfg.binary_adaptive);

        let cfg = profile(TracePreset::Wireframe).vtracer_config();
        assert!(cfg.binary_adaptive, "mono-clean = BW + adaptive");

        let cfg = profile(TracePreset::Lineart).vtracer_config();
        assert!(cfg.binary_adaptive);
    }

    #[test]
    fn colour_presets_use_expected_hierarchies() {
        let cfg = profile(TracePreset::Balanced).vtracer_config();
        assert_eq!(cfg.clustering, Clustering::ColorCluster);
        assert_eq!(cfg.hierarchical, Hierarchical::Stacked);

        let cfg = profile(TracePreset::Detailed).vtracer_config();
        assert_eq!(cfg.hierarchical, Hierarchical::Cutout);

        let cfg = profile(TracePreset::Pixel).vtracer_config();
        assert_eq!(cfg.mode, FitMode::Pixel);
    }
}
