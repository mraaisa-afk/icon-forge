//! §3.6 detector 3 — outliers.
//!
//! Real sheets are consistent and then contain one icon that is not. The
//! roadmap's example is the one to keep in mind: *"99 icons are 2 px outline,
//! one is a filled blob."* No individual icon is wrong — the *sheet* has a
//! norm and one icon disagrees with it.
//!
//! Two mechanisms, because numbers alone do not catch that example:
//!
//! 1. **Modified z-scores (MAD)** over five per-icon numbers: ink size, stroke
//!    weight, node count, colour count and solidity. `z = 0.6745·(x − median) /
//!    MAD`, flagged above `3.5` (§3.6). The median/MAD pair is the point: a
//!    mean and a standard deviation would be dragged along by the very outlier
//!    being looked for.
//! 2. **Modal style / palette mismatch** — the sheet's most common palette
//!    (the set of fill colours, hashed by the caller) and style class (filled
//!    or outline) are treated as the norm when they cover at least half the
//!    sheet; an icon outside the norm is flagged even when every number is
//!    inside the fences. This is what actually catches the filled blob: 99
//!    outlines *is* the norm.
//!
//! # The MAD = 0 case
//!
//! If more than half the values are identical, the MAD is zero and the
//! z-score is undefined — which is exactly the "99 identical + 1 different"
//! case that must not be missed. Iglewicz and Hoaglin's own recommendation is
//! followed: when the MAD is zero, use the mean absolute deviation instead
//! (with the matching `0.7979` constant). Only a sheet where *every* value is
//! identical has no spread at all, and there nothing can be an outlier.

/// The modified z-score above which a value is an outlier (§3.6).
pub const OUTLIER_Z: f32 = 3.5;

/// The consistency constant that turns a MAD into a standard-deviation
/// estimate (Iglewicz & Hoaglin; `0.6745 = Φ⁻¹(0.75)`).
pub const MAD_SCALE: f32 = 0.6745;

/// The same constant for the mean-absolute-deviation fallback.
pub const MEAN_AD_SCALE: f32 = 0.7979;

/// Ink area over bbox area at or above which an icon reads as **filled**
/// rather than an outline. A ring sits far below it, a solid square above.
pub const FILLED_RATIO: f32 = 0.5;

/// How an icon deviates from its sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutlierKind {
    /// Ink size (`√ink area`, so the metric is a length like the others).
    InkSize,
    /// Stroke weight from the §3.5 distance transform.
    Stroke,
    /// Traced node count.
    NodeCount,
    /// Distinct colours in the traced palette.
    Colours,
    /// Solidity (§3.5): ink area over convex-hull area.
    Solidity,
    /// The icon's colour set is not the sheet's modal palette.
    Palette,
    /// The icon is filled where the sheet is outlines, or the reverse.
    Style,
}

impl OutlierKind {
    /// The stable name used by the CSV, the log lines and the UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InkSize => "ink-size",
            Self::Stroke => "stroke",
            Self::NodeCount => "node-count",
            Self::Colours => "colours",
            Self::Solidity => "solidity",
            Self::Palette => "palette",
            Self::Style => "style",
        }
    }

    /// Parses [`OutlierKind::as_str`] back.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ink-size" => Some(Self::InkSize),
            "stroke" => Some(Self::Stroke),
            "node-count" => Some(Self::NodeCount),
            "colours" => Some(Self::Colours),
            "solidity" => Some(Self::Solidity),
            "palette" => Some(Self::Palette),
            "style" => Some(Self::Style),
            _ => None,
        }
    }
}

/// Filled or outline — the sheet-wide style class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StyleClass {
    /// Ink fill ratio at or above [`FILLED_RATIO`].
    Filled,
    /// Below it: an outline, a ring, a glyph with holes.
    Outline,
}

impl StyleClass {
    /// The stable name used by the UI and `review.csv`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Filled => "filled",
            Self::Outline => "outline",
        }
    }

    /// Names the class a fill ratio belongs to.
    #[must_use]
    pub fn of(fill_ratio: f32) -> Self {
        if fill_ratio >= FILLED_RATIO {
            Self::Filled
        } else {
            Self::Outline
        }
    }
}

/// What the outlier detector reads per icon.
///
/// Every field is already a summary number from earlier stages, which is why
/// this detector is pure arithmetic: nothing here needs the pixels again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconStat {
    /// The plan's id.
    pub id: u32,
    /// `√ink area` in pixels.
    pub ink_size: f32,
    /// Stroke weight (§3.5), in pixels.
    pub stroke: f32,
    /// Traced node count.
    pub node_count: f32,
    /// Distinct colours in the traced palette.
    pub colours: f32,
    /// Solidity in `[0, 1]`.
    pub solidity: f32,
    /// Ink area over bbox area, in `[0, 1]`.
    pub fill_ratio: f32,
    /// Hash of the icon's sorted fill colours — the caller folds the palette
    /// into one comparable number (a hex of the four or five channels the icon
    /// uses), so this module stays free of colour handling.
    pub palette: u64,
}

/// One deviation, with the numbers that produced it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutlierFlag {
    /// Which icon.
    pub id: u32,
    /// Which metric.
    pub kind: OutlierKind,
    /// The modified z-score (or `0.0` for the two mismatch kinds, which are
    /// categorical and have no z-score).
    pub z: f32,
    /// The icon's value.
    pub value: f32,
    /// The sheet's median for that metric.
    pub median: f32,
}

/// Median and both absolute-deviation spreads of one metric.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    /// The median value.
    pub median: f32,
    /// Median absolute deviation (may legitimately be zero).
    pub mad: f32,
    /// Mean absolute deviation — the fallback when `mad` is zero.
    pub mean_ad: f32,
}

/// The median of a slice; `0.0` for an empty slice.
///
/// Even-length slices take the **mean of the two middles**, which is the
/// definition the spec's numbers assume (an odd split would make a two-icon
/// sheet's median equal one of its icons).
#[must_use]
pub fn median(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// The median absolute deviation of `values` about `median`.
#[must_use]
pub fn mad(values: &[f32], about: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let deviations: Vec<f32> = values.iter().map(|v| (v - about).abs()).collect();
    median(&deviations)
}

/// The mean absolute deviation of `values` about `median`.
#[must_use]
pub fn mean_ad(values: &[f32], about: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let sum: f32 = values.iter().map(|v| (v - about).abs()).sum();
    sum / values.len() as f32
}

/// Both spreads of one metric, in one pass over the caller's slice.
#[must_use]
pub fn spread(values: &[f32]) -> Spread {
    let median = median(values);
    Spread {
        median,
        mad: mad(values, median),
        mean_ad: mean_ad(values, median),
    }
}

/// The modified z-score of `value` against a metric's spread.
///
/// Uses the MAD when it is non-zero and the mean absolute deviation when it is
/// not (see the module docs); a metric with no spread at all scores `0.0`, so
/// a perfectly uniform sheet produces no outliers — and never a division by
/// zero.
#[must_use]
pub fn modified_z(value: f32, spread: &Spread) -> f32 {
    if spread.mad > 0.0 {
        MAD_SCALE * (value - spread.median) / spread.mad
    } else if spread.mean_ad > 0.0 {
        MEAN_AD_SCALE * (value - spread.median) / spread.mean_ad
    } else {
        0.0
    }
}

/// The sheet's most common palette hash, when one covers at least half of it.
///
/// `None` means the sheet has no palette norm — four shapes each using their
/// own colour are not evidence of anything being *wrong*, so nothing is
/// flagged on palette grounds.
#[must_use]
pub fn modal_palette(stats: &[IconStat]) -> Option<u64> {
    modal_by(stats, |s| s.palette)
}

/// The sheet's most common style class, when one covers at least half of it.
#[must_use]
pub fn modal_style(stats: &[IconStat]) -> Option<StyleClass> {
    modal_by(stats, |s| StyleClass::of(s.fill_ratio).as_str()).map(|name| {
        if name == StyleClass::Filled.as_str() {
            StyleClass::Filled
        } else {
            StyleClass::Outline
        }
    })
}

/// Mode of a key function over the stats, with the strict-majority rule and
/// ties broken toward the smaller key so the result is deterministic.
fn modal_by<T: Ord + Copy>(stats: &[IconStat], key: impl Fn(&IconStat) -> T) -> Option<T> {
    if stats.is_empty() {
        return None;
    }
    let mut counts: std::collections::BTreeMap<T, usize> = std::collections::BTreeMap::new();
    for stat in stats {
        *counts.entry(key(stat)).or_insert(0) += 1;
    }
    let (best, count) = counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(k, c)| (*k, *c))?;
    if count * 2 >= stats.len() {
        Some(best)
    } else {
        None
    }
}

/// The five numeric metrics the z-score detector watches, each with the flag
/// it raises.
type MetricReader = fn(&IconStat) -> f32;

/// Every outlier on the sheet, sorted by icon id and then by kind.
///
/// `z_max` is [`OUTLIER_Z`] at the call site — passed in so a gate can tighten
/// it without editing this module.
#[must_use]
pub fn scan(stats: &[IconStat], z_max: f32) -> Vec<OutlierFlag> {
    let mut out = Vec::new();
    let metrics: [(OutlierKind, MetricReader); 5] = [
        (OutlierKind::InkSize, |s| s.ink_size),
        (OutlierKind::Stroke, |s| s.stroke),
        (OutlierKind::NodeCount, |s| s.node_count),
        (OutlierKind::Colours, |s| s.colours),
        (OutlierKind::Solidity, |s| s.solidity),
    ];
    for (kind, get) in metrics {
        let values: Vec<f32> = stats.iter().map(get).collect();
        let spread = spread(&values);
        for stat in stats {
            let value = get(stat);
            let z = modified_z(value, &spread);
            if z.abs() > z_max {
                out.push(OutlierFlag {
                    id: stat.id,
                    kind,
                    z,
                    value,
                    median: spread.median,
                });
            }
        }
    }
    let palette = modal_palette(stats);
    let style = modal_style(stats);
    for stat in stats {
        if let Some(modal) = palette {
            if stat.palette != modal {
                out.push(OutlierFlag {
                    id: stat.id,
                    kind: OutlierKind::Palette,
                    z: 0.0,
                    value: stat.palette as f32,
                    median: modal as f32,
                });
            }
        }
        if let Some(modal) = style {
            if StyleClass::of(stat.fill_ratio) != modal {
                out.push(OutlierFlag {
                    id: stat.id,
                    kind: OutlierKind::Style,
                    z: 0.0,
                    value: stat.fill_ratio,
                    median: 0.0,
                });
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.kind.cmp(&b.kind)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(id: u32, ink_size: f32) -> IconStat {
        IconStat {
            id,
            ink_size,
            stroke: 4.0,
            node_count: 8.0,
            colours: 1.0,
            solidity: 0.9,
            fill_ratio: 0.2,
            palette: 7,
        }
    }

    #[test]
    fn median_takes_both_middles_for_even_slices() {
        assert!((median(&[1.0, 2.0, 3.0, 4.0]) - 2.5).abs() < 1e-6);
        assert!((median(&[4.0, 1.0, 3.0]) - 3.0).abs() < 1e-6);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn the_classic_ninety_nine_plus_one_is_caught_with_a_zero_mad() {
        // 99 outlines of the same size, one blob four times the size.
        let mut values = vec![10.0f32; 99];
        values.push(40.0);
        let spread = spread(&values);
        assert_eq!(spread.mad, 0.0, "more than half identical ⇒ MAD is zero");
        let z = modified_z(40.0, &spread);
        assert!(
            z > OUTLIER_Z,
            "the fallback must still flag it, got z={z} (mean_ad={})",
            spread.mean_ad
        );
        assert_eq!(modified_z(10.0, &spread), 0.0);
    }

    #[test]
    fn a_uniform_sheet_has_no_outliers() {
        let stats: Vec<IconStat> = (0..10).map(|i| stat(i, 12.0)).collect();
        assert!(scan(&stats, OUTLIER_Z).is_empty());
    }

    #[test]
    fn a_filled_blob_among_outlines_is_flagged_by_style() {
        let mut stats: Vec<IconStat> = (0..9).map(|i| stat(i, 12.0)).collect();
        let mut blob = stat(9, 12.0);
        blob.fill_ratio = 0.95;
        stats.push(blob);
        let flags = scan(&stats, OUTLIER_Z);
        assert!(
            flags
                .iter()
                .any(|f| f.id == 9 && f.kind == OutlierKind::Style),
            "expected a style flag, got {flags:?}"
        );
        // The nine outlines are the norm, so they are not flagged.
        assert!(flags.iter().all(|f| f.id == 9));
    }

    #[test]
    fn no_palette_norm_means_no_palette_flags() {
        // Four palettes, a quarter each: no majority, nothing is "wrong".
        let stats: Vec<IconStat> = (0..4)
            .map(|i| {
                let mut s = stat(i, 12.0);
                s.palette = u64::from(i);
                s
            })
            .collect();
        assert_eq!(modal_palette(&stats), None);
        assert!(!scan(&stats, OUTLIER_Z)
            .iter()
            .any(|f| f.kind == OutlierKind::Palette));
    }

    #[test]
    fn a_palette_outlier_is_flagged_when_the_sheet_has_a_norm() {
        let mut stats: Vec<IconStat> = (0..8).map(|i| stat(i, 12.0)).collect();
        let mut odd = stat(8, 12.0);
        odd.palette = 999;
        stats.push(odd);
        assert_eq!(modal_palette(&stats), Some(7));
        let flags = scan(&stats, OUTLIER_Z);
        assert!(flags
            .iter()
            .any(|f| f.id == 8 && f.kind == OutlierKind::Palette));
    }

    #[test]
    fn metric_outliers_report_their_numbers() {
        let mut stats: Vec<IconStat> = (0..10).map(|i| stat(i, 10.0)).collect();
        stats[3].stroke = 40.0;
        let flags = scan(&stats, OUTLIER_Z);
        let flag = flags
            .iter()
            .find(|f| f.kind == OutlierKind::Stroke)
            .expect("a stroke outlier");
        assert_eq!(flag.id, 3);
        assert!((flag.value - 40.0).abs() < 1e-6);
        assert!((flag.median - 4.0).abs() < 1e-6);
        assert!(flag.z.abs() > OUTLIER_Z);
    }

    #[test]
    fn scan_is_sorted_and_deterministic() {
        let mut stats: Vec<IconStat> = (0..6).map(|i| stat(i, 10.0)).collect();
        stats[0].stroke = 40.0;
        stats[5].ink_size = 90.0;
        let a = scan(&stats, OUTLIER_Z);
        let b = scan(&stats, OUTLIER_Z);
        assert_eq!(a, b);
        let mut sorted = a.clone();
        sorted.sort_by(|x, y| x.id.cmp(&y.id).then_with(|| x.kind.cmp(&y.kind)));
        assert_eq!(a, sorted);
    }

    #[test]
    fn names_round_trip() {
        for kind in [
            OutlierKind::InkSize,
            OutlierKind::Stroke,
            OutlierKind::NodeCount,
            OutlierKind::Colours,
            OutlierKind::Solidity,
            OutlierKind::Palette,
            OutlierKind::Style,
        ] {
            assert_eq!(OutlierKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(StyleClass::of(0.9), StyleClass::Filled);
        assert_eq!(StyleClass::of(0.1), StyleClass::Outline);
    }
}
