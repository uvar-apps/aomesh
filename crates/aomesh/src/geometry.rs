//! The triangle soup the occlusion tests run against.
//!
//! [`Geometry`] is a read-only projection of an [`objserde::Obj`] onto the three
//! things ray casting needs — positions, the orientation of each surface point,
//! and position-index triples. Materials, texture coordinates and trailing vertex
//! fields stay behind in the `Obj`, so nothing can accidentally round-trip through
//! a lossy intermediate.

use objserde::Obj;

/// A mesh as positions, surface orientations, and triangles that index them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// One `[x, y, z]` per `v` line, in file order.
    pub positions: Vec<[f64; 3]>,
    /// One unit `[x, y, z]` per position, from objserde's binding layer: the `vn`
    /// bound to that position, or the area-weighted normal of its incident faces
    /// when nothing was bound. Exactly `[0.0, 0.0, 0.0]` where the incident faces
    /// enclose no area at all, which is a genuine answer for folded geometry and
    /// is kept rather than smoothed over.
    ///
    /// This is what decides which half of the viewpoint sphere a vertex can see.
    pub normals: Vec<[f64; 3]>,
    /// One `[i, j, k]` per triangle, indexing [`positions`](Self::positions).
    /// Polygon faces are fan-triangulated; triangles whose three corners do not
    /// address three distinct positions are dropped, because they enclose no
    /// area and so can never occlude anything.
    pub triangles: Vec<[u32; 3]>,
}

impl Geometry {
    /// Project an OBJ onto ray-casting geometry.
    #[must_use]
    pub fn from_obj(obj: &Obj) -> Self {
        // Positions and normals both come out of `bind`, which guarantees they are
        // parallel — one pair per vertex, in file order.
        let bound = obj.bind();
        let mut triangles = Vec::with_capacity(obj.faces.len());
        for triangle in obj.triangulate() {
            let [a, b, c] = [
                triangle.corners[0].v,
                triangle.corners[1].v,
                triangle.corners[2].v,
            ];
            if a == b || b == c || a == c {
                continue;
            }
            triangles.push([a, b, c]);
        }
        Self {
            positions: bound.positions,
            normals: bound.unit_normals,
            triangles,
        }
    }

    /// True when there is nothing to shadow against.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// Axis-aligned bounds of the referenced positions, or `None` for a mesh
    /// with no vertex to measure.
    #[must_use]
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        if self.positions.is_empty() {
            return None;
        }
        let mut min = [f64::MAX; 3];
        let mut max = [f64::MIN; 3];
        for p in &self.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        Some((min, max))
    }

    /// Mean of all positions — the centre the viewpoint sphere is built around.
    #[must_use]
    pub fn centroid(&self) -> [f64; 3] {
        if self.positions.is_empty() {
            return [0.0; 3];
        }
        let mut sum = [0.0f64; 3];
        for p in &self.positions {
            for axis in 0..3 {
                sum[axis] += p[axis];
            }
        }
        let n = self.positions.len() as f64;
        [sum[0] / n, sum[1] / n, sum[2] / n]
    }

    /// Corner-to-corner span of the bounds. Everything that must scale with the
    /// mesh — the viewpoint radius, and nothing else here — derives from it.
    #[must_use]
    pub fn diagonal(&self) -> f64 {
        let Some((min, max)) = self.bounds() else {
            return 0.0;
        };
        let d = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }
}
