//! Per-vertex ambient occlusion by cast-and-count.
//!
//! Viewpoints sit on a sphere enclosing the mesh, but a vertex is tested only
//! against the viewpoints **in front of its own surface**: those whose direction
//! lies within 180° of the vertex normal, the hemisphere it opens into. A
//! viewpoint behind the normal is inside the material rather than in the sky, so it
//! is not something that surface could ever show, and it is counted neither as seen
//! nor as hidden.
//!
//! ```text
//! visibility = times seen / viewpoints in front of the surface
//! ```
//!
//! Normalizing over the hemisphere is what makes an exposed surface read exactly
//! 1.0 — full white — because nothing stands between it and any viewpoint it can
//! address. Only real occluders darken a vertex: the far wall of a hole, the plate
//! resting above. A convex cube hides nothing from itself and so bakes entirely
//! white; the same cube wound inward, a sealed box, bakes entirely black.
//!
//! 0.0 is fully occluded (black), 1.0 fully open (white), and nothing is inverted
//! anywhere in this crate.
//!
//! Each front-facing viewpoint is weighted by `cos θ`, its angle off the normal, so
//! what comes out is the cosine-weighted hemispherical mean — the transfer integral
//! vertex AO was originally defined as, and the visibility term for irradiance from a
//! uniform sky. Weighting matters most exactly where the ray test is least reliable:
//! a direction 85° off the normal contributes little, and **one at 90° contributes
//! nothing**, so a grazing viewpoint cannot mark a vertex hidden no matter what the
//! intersection test returns. There is no band to exclude and no boundary to worry
//! about — the weight is zero there by construction.
//!
//! A fully exposed surface still reads exactly 1.0, because weighting the numerator
//! and the denominator the same way leaves the ratio untouched when every front-facing
//! viewpoint is seen.
//!
//! Normals come from objserde's binding layer. A vertex whose normal is exactly
//! zero — geometry folded so flat that its incident faces enclose no area — has no
//! front half-space to test, and reports 1.0 rather than dividing by nothing.

use rayon::prelude::*;

use crate::bvh::Bvh;
use crate::geometry::Geometry;

/// Viewpoints on the sphere per vertex. Roughly half fall behind each surface and are
/// skipped outright; of the rest, each counts in proportion to how face-on it is, so
/// viewpoints near the horizon barely register and casting them is nearly free.
pub const DEFAULT_SAMPLES: usize = 256;

/// Viewpoint sphere radius as a multiple of the mesh diagonal. Large enough that
/// every triangle lies well inside the sphere — a viewpoint is never hidden
/// inside the geometry it is meant to observe — and small enough that the
/// vertex→viewpoint segment stays numerically comfortable.
pub const VIEW_RADIUS_FACTOR: f64 = 50.0;

/// `count` unit vectors, spread over the sphere by a Fibonacci lattice.
///
/// The lattice is chosen over random sampling because it is both uniform and
/// deterministic: the same `count` always yields bit-identical directions, so a
/// bake is reproducible and its error does not wander between runs.
#[must_use]
pub fn fibonacci_sphere(count: usize) -> Vec<[f64; 3]> {
    if count == 0 {
        return Vec::new();
    }
    // Golden angle: successive points advance by the irrational step that leaves
    // the most room for the next.
    let golden = std::f64::consts::PI * (3.0 - 5.0f64.sqrt());
    let n = count as f64;
    (0..count)
        .map(|i| {
            let step = (i as f64 + 0.5) / n;
            let z = 1.0 - 2.0 * step;
            let radius = (1.0 - z * z).max(0.0).sqrt();
            let angle = golden * i as f64;
            [radius * angle.cos(), radius * angle.sin(), z]
        })
        .collect()
}

/// Visibility of every vertex, in `0.0..=1.0`, parallel to
/// [`Geometry::positions`].
///
/// Each vertex is measured against its own front hemisphere, weighted by `cos θ`, so
/// an unobstructed surface reads exactly 1.0 and only genuine occluders pull a vertex
/// down — a viewpoint at or beyond 90° to the normal carries no weight at all.
/// Vertices are independent and are distributed over rayon worker threads; each
/// accumulates into its own sums, which keeps the result free of any summation-order
/// dependence.
#[must_use]
pub fn vertex_visibility(geo: &Geometry, samples: usize) -> Vec<f64> {
    if geo.positions.is_empty() {
        return Vec::new();
    }
    let samples = samples.max(1);
    let directions = fibonacci_sphere(samples);
    let center = geo.centroid();
    // A mesh of coincident vertices has no diagonal to scale by; any positive
    // radius will do because nothing there can occlude anything.
    let radius = (geo.diagonal() * VIEW_RADIUS_FACTOR).max(1.0);
    let bvh = Bvh::build(geo);

    (0..geo.positions.len())
        .into_par_iter()
        .map(|index| {
            let from = geo.positions[index];
            let normal = geo.normals[index];
            let mut seen = 0.0f64;
            let mut total = 0.0f64;
            for direction in &directions {
                // Viewpoints are absolute positions on one sphere about the mesh, as
                // the format describes, not a per-vertex cone: what a vertex can see
                // is measured from where that vertex stands.
                let to = [
                    center[0] + radius * direction[0],
                    center[1] + radius * direction[1],
                    center[2] + radius * direction[2],
                ];
                let towards = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
                // `normal` is a unit vector, so this is cos θ for the angle between
                // the segment and the surface. It is zero at 90°, which is the point:
                // a grazing viewpoint votes, but its vote weighs nothing.
                let cos = dot(towards, normal) / dot(towards, towards).sqrt();
                if cos <= 0.0 {
                    continue;
                }
                total += cos;
                if !bvh.segment_blocked(from, to, index as u32) {
                    seen += cos;
                }
            }
            // `total` vanishes only when the vertex has no front half-space to speak
            // of — a zero-length normal from geometry folded so flat its incident
            // faces enclose no area. Such a vertex is reported open.
            if total == 0.0 { 1.0 } else { seen / total }
        })
        .collect()
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Spread of a visibility batch, for the run summary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibilityReport {
    pub min: f64,
    pub mean: f64,
    pub max: f64,
}

/// Min, mean and max of `visibility`, or all-zero for an empty slice.
#[must_use]
pub fn report(visibility: &[f64]) -> VisibilityReport {
    if visibility.is_empty() {
        return VisibilityReport {
            min: 0.0,
            mean: 0.0,
            max: 0.0,
        };
    }
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    let mut sum = 0.0f64;
    for &value in visibility {
        min = min.min(value);
        max = max.max(value);
        sum += value;
    }
    VisibilityReport {
        min,
        mean: sum / visibility.len() as f64,
        max,
    }
}
