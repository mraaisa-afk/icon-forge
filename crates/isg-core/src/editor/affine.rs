//! Affine transforms in the editor's coordinate system.

/// A 2×3 affine transform.
///
/// Stored row-major as `[m0, m1, m2, m3, m4, m5]` with
/// `x' = m0·x + m2·y + m4` and `y' = m1·x + m3·y + m5` — exactly the argument
/// order of Canvas2D's `setTransform`/`transform` and of SVG's
/// `matrix(m0 m1 m2 m3 m4 m5)`. Keeping the layout identical means geometry
/// crosses the WASM boundary and reaches the canvas (or an SVG attribute)
/// without a re-mapping step that could silently transpose a rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    /// The six matrix entries, row-major.
    pub m: [f32; 6],
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    /// Builds a transform from the six matrix entries.
    #[must_use]
    pub const fn new(m: [f32; 6]) -> Self {
        Self { m }
    }

    /// A pure translation.
    #[must_use]
    pub const fn translate(tx: f32, ty: f32) -> Self {
        Self {
            m: [1.0, 0.0, 0.0, 1.0, tx, ty],
        }
    }

    /// A pure (possibly non-uniform) scale about the origin.
    #[must_use]
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            m: [sx, 0.0, 0.0, sy, 0.0, 0.0],
        }
    }

    /// A rotation about the origin; `degrees` is clockwise in the document's
    /// y-down coordinate system.
    #[must_use]
    pub fn rotate(degrees: f32) -> Self {
        let r = degrees.to_radians();
        let (s, c) = (r.sin(), r.cos());
        Self {
            m: [c, s, -s, c, 0.0, 0.0],
        }
    }

    /// `self` followed by `outer` (`outer ∘ self`).
    #[must_use]
    pub fn then(self, outer: Self) -> Self {
        let (a, b) = (self.m, outer.m);
        Self {
            m: [
                a[0] * b[0] + a[1] * b[2],
                a[0] * b[1] + a[1] * b[3],
                a[2] * b[0] + a[3] * b[2],
                a[2] * b[1] + a[3] * b[3],
                a[4] * b[0] + a[5] * b[2] + b[4],
                a[4] * b[1] + a[5] * b[3] + b[5],
            ],
        }
    }

    /// Applies the transform to a point.
    #[must_use]
    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        let m = self.m;
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }

    /// Applies only the linear part — for directions, not positions.
    #[must_use]
    pub fn apply_vector(self, x: f32, y: f32) -> (f32, f32) {
        let m = self.m;
        (m[0] * x + m[2] * y, m[1] * x + m[3] * y)
    }

    /// Determinant of the linear part.
    #[must_use]
    pub fn det(self) -> f32 {
        let m = self.m;
        m[0] * m[3] - m[1] * m[2]
    }

    /// The inverse transform, or `None` when the matrix is singular or any
    /// entry is non-finite (degenerate scales must never produce a silently
    /// wrong inverse).
    #[must_use]
    pub fn invert(self) -> Option<Self> {
        let m = self.m;
        let det = self.det();
        if !det.is_finite() || det.abs() < f32::EPSILON || !self.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        Some(Self {
            m: [
                m[3] * inv,
                -m[1] * inv,
                -m[2] * inv,
                m[0] * inv,
                (m[2] * m[5] - m[3] * m[4]) * inv,
                (m[1] * m[4] - m[0] * m[5]) * inv,
            ],
        })
    }

    /// True when every entry is finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// The six entries, in canvas order.
    #[must_use]
    pub const fn to_array(self) -> [f32; 6] {
        self.m
    }

    /// Builds a transform from the six canvas-order entries.
    #[must_use]
    pub const fn from_array(m: [f32; 6]) -> Self {
        Self { m }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Affine, b: Affine) {
        for (x, y) in a.m.iter().zip(b.m.iter()) {
            assert!((x - y).abs() < 1e-5, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn compose_matches_apply_order() {
        let r = Affine::rotate(90.0);
        let t = Affine::translate(10.0, -4.0);
        // `then` applies self first: rotate the point, then translate it.
        let composed = r.then(t).apply(1.0, 0.0);
        let (rx, ry) = r.apply(1.0, 0.0);
        let manual = t.apply(rx, ry);
        assert!((composed.0 - manual.0).abs() < 1e-5 && (composed.1 - manual.1).abs() < 1e-5);
        // Rotating +90° in the y-down document space sends +x to +y.
        assert!((manual.0 - 10.0).abs() < 1e-5, "{manual:?}");
        assert!((manual.1 - (-3.0)).abs() < 1e-5, "{manual:?}");
    }

    #[test]
    fn inverse_round_trips_points() {
        let m = Affine::translate(3.0, 5.0)
            .then(Affine::scale(2.0, 0.5))
            .then(Affine::rotate(37.0));
        let inv = m.invert().expect("invertible");
        let (x, y) = inv.apply(11.0, -2.0);
        let (x, y) = m.apply(x, y);
        assert!((x - 11.0).abs() < 1e-3 && (y - (-2.0)).abs() < 1e-3);
    }

    #[test]
    fn singular_and_non_finite_transforms_have_no_inverse() {
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
        assert!(!Affine::new([f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]).is_finite());
        assert!(Affine::new([f32::INFINITY, 0.0, 0.0, 1.0, 0.0, 0.0])
            .invert()
            .is_none());
    }

    #[test]
    fn canvas_argument_order_is_the_documented_one() {
        // m = [a, b, c, d, e, f] ⇒ x' = a·x + c·y + e.
        let m = Affine::new([2.0, 3.0, 5.0, 7.0, 11.0, 13.0]);
        assert_eq!(m.apply(1.0, 1.0), (18.0, 23.0));
        approx(m, Affine::from_array(m.to_array()));
    }
}
