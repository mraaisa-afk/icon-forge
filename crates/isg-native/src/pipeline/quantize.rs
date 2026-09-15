//! §3.3-④ Quantize (colour mode only) — deterministic k-means++ with an
//! elbow-chosen k, per-layer de-fringing open, layers ordered by ink area
//! DESC = paint order (ARCHITECTURE.md §3.3).
//!
//! Determinism contract: no `rand` — seeding uses a fixed-seed integer LCG,
//! distances are exact integer squares, and iteration counts are fixed, so
//! the same crop bytes always produce the same layers byte-for-byte.
//!
//! Note: the crop may contain sheet-background pixels (the bbox is not
//! necessarily tight after stage ②/③); they simply form their own cluster /
//! layer and are painted first (largest area), which the stacked emit in W3
//! can drop via the foreground mask.

use isg_core::ForegroundMask;

use super::clean::open3;

/// Quantizer tunables. Field-stable: the vectorization cache key will embed
/// the k target chosen per preset, not these internals.
#[derive(Clone, Copy, Debug)]
pub struct QuantizeParams {
    /// Upper bound for the elbow search (`1..=k_max`, hard-capped at 32).
    pub k_max: u32,
    /// Stride-sampled pixel budget fed to the elbow/k-means search.
    pub sample_cap: u32,
    /// Lloyd iterations per k-means run (fixed for determinism).
    pub iters: u32,
    /// Layers smaller than this many ink pixels are dropped.
    pub layer_min_px: u32,
    /// Pixels below this alpha are not ink and join no layer.
    pub alpha_min: u8,
}

impl Default for QuantizeParams {
    fn default() -> Self {
        Self {
            k_max: 12,
            sample_cap: 2048,
            iters: 10,
            layer_min_px: 4,
            alpha_min: 128,
        }
    }
}

impl QuantizeParams {
    /// Defaults with a preset-specific k cap (the profile's palette target).
    #[must_use]
    pub fn with_k_cap(k_max: u32) -> Self {
        Self {
            k_max: k_max.max(1),
            ..Self::default()
        }
    }
}

/// One flat-colour paint layer over the crop.
pub struct Layer {
    /// Mean RGBA of the layer's pixels (the paint colour).
    pub rgba: [u8; 4],
    /// De-fringed (open-filtered) ink mask, crop-local `width × height`.
    pub mask: ForegroundMask,
    /// Ink pixel count of the de-fringed mask (paint-order sort key).
    pub area: u64,
}

/// Quantizes a crop (RGBA8, `w × h`) into paint layers ordered by ink area
/// DESC (paint order). Empty when the crop has no opaque-enough pixels.
#[must_use]
pub fn quantize_crop(rgba: &[u8], w: u32, h: u32, p: &QuantizeParams) -> Vec<Layer> {
    let n = w as usize * h as usize;
    if n == 0 || rgba.len() < n * 4 {
        return Vec::new();
    }
    let px = rgba.as_chunks::<4>().0;
    let mut opaque = 0usize;
    for c in px {
        if c[3] >= p.alpha_min {
            opaque += 1;
        }
    }
    if opaque == 0 {
        return Vec::new();
    }
    let stride = (opaque / p.sample_cap as usize).max(1);
    let mut samples: Vec<[i32; 3]> = Vec::new();
    let mut seen = 0usize;
    for c in px {
        if c[3] < p.alpha_min {
            continue;
        }
        if seen.is_multiple_of(stride) {
            samples.push([i32::from(c[0]), i32::from(c[1]), i32::from(c[2])]);
        }
        seen += 1;
    }

    // Elbow: run k-means for every k in 1..=k_cap and pick the knee of the
    // inertia curve (largest second difference, lowest k wins ties).
    let k_cap = p
        .k_max
        .clamp(1, 32)
        .min(samples.len().min(u32::MAX as usize) as u32);
    let mut inertias = Vec::with_capacity(k_cap as usize);
    for k in 1..=k_cap {
        let (centres, assign) = kmeans(&samples, k, p.iters);
        inertias.push(inertia(&samples, &centres, &assign));
    }
    let k = pick_elbow(&inertias);
    let (centres, _) = kmeans(&samples, k, p.iters);

    // Assign every opaque pixel; per-cluster masks + mean colours.
    let k = centres.len();
    let mut masks: Vec<ForegroundMask> = (0..k).map(|_| ForegroundMask::new(w, h)).collect();
    let mut sums = vec![[0u64; 4]; k];
    let mut counts = vec![0u64; k];
    for (i, c) in px.iter().enumerate().take(n) {
        if c[3] < p.alpha_min {
            continue;
        }
        let s = [i32::from(c[0]), i32::from(c[1]), i32::from(c[2])];
        let mut best = 0usize;
        let mut best_d = u32::MAX;
        for (ci, cc) in centres.iter().enumerate() {
            let d = sq_dist(s, *cc);
            if d < best_d {
                best_d = d;
                best = ci;
            }
        }
        masks[best].set_index(i, true);
        for ch in 0..4 {
            sums[best][ch] += u64::from(c[ch]);
        }
        counts[best] += 1;
    }

    // De-fringe each layer with the open filter, drop specks, sort DESC.
    let mut layers: Vec<Layer> = Vec::new();
    for ci in 0..k {
        let mean = |s: u64| -> u8 { s.checked_div(counts[ci]).unwrap_or(0) as u8 };
        let rgba = [
            mean(sums[ci][0]),
            mean(sums[ci][1]),
            mean(sums[ci][2]),
            mean(sums[ci][3]),
        ];
        let mask = open3(&masks[ci]);
        let area = mask.ink_count();
        if area < u64::from(p.layer_min_px.max(1)) {
            continue;
        }
        layers.push(Layer { rgba, mask, area });
    }
    layers.sort_by(|a, b| b.area.cmp(&a.area).then(a.rgba.cmp(&b.rgba)));
    layers
}

/// Elbow selection: `argmax` of the second difference of the inertia curve,
/// returning the k just after the biggest drop. Curves shorter than 3 points
/// have no knee — the largest k is used.
fn pick_elbow(inertias: &[f32]) -> u32 {
    if inertias.len() < 3 {
        return inertias.len() as u32;
    }
    let drops: Vec<f32> = (0..inertias.len() - 1)
        .map(|i| inertias[i] - inertias[i + 1])
        .collect();
    let mut best = 0.0f32;
    let mut best_i = 0usize;
    for i in 0..drops.len() - 1 {
        let s = drops[i] - drops[i + 1];
        if s > best {
            best = s;
            best_i = i;
        }
    }
    best_i as u32 + 2
}

/// Fixed-seed LCG (Knuth MMIX) — the deterministic stand-in for RNG.
struct Lcg(u64);

impl Lcg {
    fn new() -> Self {
        Self(0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Uniform-ish value in `0..n` via 128-bit multiply-shift (n > 0).
    fn next_below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }
}

fn sq_dist(a: [i32; 3], b: [i32; 3]) -> u32 {
    let (dr, dg, db) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dr * dr + dg * dg + db * db) as u32
}

/// Lloyd's algorithm with k-means++ seeding (deterministic LCG).
/// Returns the centres and per-sample assignments.
fn kmeans(samples: &[[i32; 3]], k: u32, iters: u32) -> (Vec<[i32; 3]>, Vec<u8>) {
    let k = k.max(1) as usize;
    let mut rng = Lcg::new();
    let mut centres = seed_pp(samples, k, &mut rng);
    let mut assign = vec![0u8; samples.len()];
    for _ in 0..iters {
        assign_points(samples, &centres, &mut assign);
        let mut moved = false;
        for (ci, centre) in centres.iter_mut().enumerate().take(k) {
            let (mut sums, mut cnt) = ([0i64; 3], 0u64);
            for (s, &a) in samples.iter().zip(assign.iter()) {
                if usize::from(a) == ci {
                    sums[0] += i64::from(s[0]);
                    sums[1] += i64::from(s[1]);
                    sums[2] += i64::from(s[2]);
                    cnt += 1;
                }
            }
            for ch in 0..3 {
                let old = centre[ch];
                let next = sums[ch]
                    .checked_div(cnt as i64)
                    .map(|v| v.clamp(0, 255) as i32)
                    .unwrap_or(old);
                if next != old {
                    centre[ch] = next;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    assign_points(samples, &centres, &mut assign);
    (centres, assign)
}

/// k-means++ seeding: first centre uniform, each next with probability
/// proportional to squared distance to the nearest existing centre.
fn seed_pp(samples: &[[i32; 3]], k: usize, rng: &mut Lcg) -> Vec<[i32; 3]> {
    let mut centres = Vec::with_capacity(k);
    centres.push(samples[rng.next_below(samples.len() as u64) as usize]);
    let mut dist = vec![u64::MAX; samples.len()];
    while centres.len() < k {
        let last = *centres.last().unwrap();
        let mut total = 0u64;
        for (i, s) in samples.iter().enumerate() {
            let d = u64::from(sq_dist(*s, last));
            dist[i] = d.min(dist[i]);
            total += dist[i];
        }
        let idx = if total == 0 {
            // All samples coincide with existing centres — duplicate freely.
            rng.next_below(samples.len() as u64) as usize
        } else {
            let target = rng.next_below(total);
            let mut acc = 0u64;
            let mut idx = samples.len() - 1;
            for (i, &d) in dist.iter().enumerate() {
                acc += d;
                if acc > target {
                    idx = i;
                    break;
                }
            }
            idx
        };
        centres.push(samples[idx]);
    }
    centres
}

fn assign_points(samples: &[[i32; 3]], centres: &[[i32; 3]], assign: &mut [u8]) {
    for (s, a) in samples.iter().zip(assign.iter_mut()) {
        let mut best = 0usize;
        let mut best_d = u32::MAX;
        for (ci, c) in centres.iter().enumerate() {
            let d = sq_dist(*s, *c);
            if d < best_d {
                best_d = d;
                best = ci;
            }
        }
        *a = best as u8;
    }
}

fn inertia(samples: &[[i32; 3]], centres: &[[i32; 3]], assign: &[u8]) -> f32 {
    samples
        .iter()
        .zip(assign)
        .map(|(s, &a)| sq_dist(*s, centres[usize::from(a)]) as f32)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `w × h` RGBA buffer: transparent everywhere, then opaque blocks.
    fn blocks(w: u32, h: u32, regions: &[(u32, u32, u32, u32, [u8; 4])]) -> Vec<u8> {
        let mut rgba = vec![0u8; 4 * w as usize * h as usize];
        for (x0, y0, rw, rh, c) in regions {
            for y in *y0..*y0 + *rh {
                for x in *x0..*x0 + *rw {
                    let i = (y as usize * w as usize + x as usize) * 4;
                    rgba[i..i + 4].copy_from_slice(&c[..]);
                }
            }
        }
        rgba
    }

    #[test]
    fn two_colour_crop_picks_k2() {
        let rgba = blocks(
            16,
            16,
            &[
                (0, 0, 8, 16, [255, 0, 0, 255]),
                (8, 0, 8, 16, [0, 0, 255, 255]),
            ],
        );
        let layers = quantize_crop(&rgba, 16, 16, &QuantizeParams::default());
        assert_eq!(layers.len(), 2, "red + blue");
        assert_eq!(layers[0].area, 128);
        assert_eq!(layers[1].area, 128);
        // Deterministic tie-break: equal areas sort by RGBA.
        assert!(layers[0].rgba <= layers[1].rgba);
    }

    #[test]
    fn three_equal_blocks_pick_k3() {
        let rgba = blocks(
            36,
            12,
            &[
                (0, 0, 12, 12, [255, 0, 0, 255]),
                (12, 0, 12, 12, [0, 255, 0, 255]),
                (24, 0, 12, 12, [0, 0, 255, 255]),
            ],
        );
        let layers = quantize_crop(&rgba, 36, 12, &QuantizeParams::default());
        assert_eq!(layers.len(), 3, "one layer per block");
        for l in &layers {
            assert_eq!(l.area, 144, "open(3) on a solid block is identity");
        }
    }

    #[test]
    fn quantize_is_deterministic() {
        let rgba = blocks(
            16,
            16,
            &[
                (0, 0, 8, 16, [255, 0, 0, 255]),
                (8, 0, 8, 16, [0, 0, 255, 255]),
            ],
        );
        let a = quantize_crop(&rgba, 16, 16, &QuantizeParams::default());
        let b = quantize_crop(&rgba, 16, 16, &QuantizeParams::default());
        assert_eq!(a.len(), b.len());
        for (la, lb) in a.iter().zip(b.iter()) {
            assert_eq!(la.rgba, lb.rgba);
            assert_eq!(la.area, lb.area);
            assert_eq!(la.mask, lb.mask);
        }
    }

    #[test]
    fn single_pixel_speck_layer_is_dropped() {
        let rgba = blocks(
            16,
            16,
            &[
                (0, 0, 16, 16, [255, 255, 255, 255]),
                (0, 0, 8, 8, [255, 0, 0, 255]),
                (14, 14, 1, 1, [0, 0, 255, 255]),
            ],
        );
        let layers = quantize_crop(&rgba, 16, 16, &QuantizeParams::default());
        assert_eq!(layers.len(), 2, "1-px blue speck de-fringed away");
        assert!(layers.iter().all(|l| l.rgba != [0, 0, 255, 255]));
    }

    #[test]
    fn transparent_pixels_join_no_layer() {
        let rgba = blocks(8, 8, &[(0, 0, 4, 8, [255, 0, 0, 255])]);
        let layers = quantize_crop(&rgba, 8, 8, &QuantizeParams::default());
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].area, 32, "only the opaque half");
        assert_eq!(layers[0].rgba, [255, 0, 0, 255]);
    }

    #[test]
    fn k_cap_from_profile_bounds_layer_count() {
        let rgba = blocks(
            36,
            12,
            &[
                (0, 0, 12, 12, [255, 0, 0, 255]),
                (12, 0, 12, 12, [0, 255, 0, 255]),
                (24, 0, 12, 12, [0, 0, 255, 255]),
            ],
        );
        let layers = quantize_crop(&rgba, 36, 12, &QuantizeParams::with_k_cap(2));
        assert_eq!(layers.len(), 2, "k_max=2 caps the palette");
    }
}
