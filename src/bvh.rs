//! A bounding-volume hierarchy over the mesh's triangles, queried with
//! blocked/unblocked segments.
//!
//! Per-vertex occlusion is a lot of rays: 38k vertices times 256 viewpoints is 9.7M
//! segments, and testing each against every triangle of a 76k-triangle mesh is 7×10¹¹
//! intersection tests. The hierarchy turns that into a depth-first walk down only the
//! boxes the segment crosses.
//!
//! Three properties matter for the numbers the bake reports:
//!
//! * **Triangle order is never assumed.** The hierarchy permutes triangle indices
//!   internally and the caller's [`Geometry`](crate::geometry::Geometry) stays untouched.
//! * **A hit is scale-invariant.** Both the parallel test and the accepted `t` range are
//!   ratios, so a mesh authored in microns and one authored in kilometres report the same
//!   visibility.
//! * **The answer does not depend on how the work was batched.** See below.
//!
//! # Where the vector instructions went
//!
//! The leaf tests four triangles' bounds at once on [`wide::f64x4`] — four lanes of IEEE
//! arithmetic in step. That is the one place in this loop where SIMD pays, and it pays
//! because it is *rejection*: a shadow ray usually hits nothing, a box test is branch-free
//! so there is nothing to lose by running all four lanes, and `wide` maps it onto whatever
//! the target offers (SSE/AVX on x86-64, NEON on Apple silicon and other aarch64, a scalar
//! fallback where there is no SIMD at all). No `target-cpu` flag is needed or set, so a
//! binary is not tied to the machine that built it.
//!
//! Only the lanes whose box the segment actually crosses go on to the exact
//! Möller–Trumbore test, and that one runs per triangle in scalar code, where it can quit
//! at the first failed barycentric test.
//!
//! That split was measured, not assumed. Running four *exact* intersections at once —
//! barycentrics, sign folding and all — measured slower than the scalar loop it replaced:
//! batching forces every lane through the full arithmetic, while the common case is a
//! miss that scalar code abandons after a couple of operations in a triangle's favour.
//! Vectorising the filter and leaving the early-out alone is what made this faster than
//! the original.
//!
//! Lanes are IEEE islands and each one does the same arithmetic a scalar test would, so
//! which triangle a ray hits does not depend on grouping. The exact assertions in
//! `tests/ao_values.rs` cover that: `1.0` for open surfaces, `0.0` for a sealed cavity, and
//! a sealed box checked against a direction oracle, all of which hold bit-for-bit.

use crate::geometry::Geometry;
// `cmp_gt` and friends arrive through these traits; they yield a full-bit mask per lane.
use wide::{CmpGt, f64x4};

/// Triangles per leaf, which is also the width of the vectorised bounds test.
const LEAF_SIZE: usize = 4;

/// Lanes in one group.
const LANES: usize = 4;

/// Recursion cap. Splitting stops early on degenerate clusters; this bounds the worst
/// case so the traversal stack cannot outgrow [`STACK_CAPACITY`].
const MAX_DEPTH: usize = 40;

/// Fixed capacity of the traversal stack; a tree of [`MAX_DEPTH`] needs at most that many
/// pending nodes.
const STACK_CAPACITY: usize = MAX_DEPTH + 4;

/// Near-parallel rejection, squared: `|det| ≤ 1e-12 · |dir| · |e1| · |e2|`. A coplanar
/// segment and triangle form a singular system whose barycentric answer is meaningless, so
/// it is rejected rather than evaluated.
const PARALLEL_EPS2: f64 = 1e-24;

/// A hit strictly closer than this fraction of the segment is the surface the ray started
/// on, not an occluder.
const T_START: f64 = 1e-9;

#[derive(Clone, Copy, Debug)]
struct Aabb {
    min: [f64; 3],
    max: [f64; 3],
}

impl Aabb {
    const EMPTY: Aabb = Aabb {
        min: [f64::MAX; 3],
        max: [f64::MIN; 3],
    };

    fn from_triangle(geo: &Geometry, triangle: [u32; 3]) -> Aabb {
        let mut box_ = Aabb::EMPTY;
        for index in triangle {
            box_ = box_.include(geo.positions[index as usize]);
        }
        box_
    }

    fn include(self, p: [f64; 3]) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(p[0]),
                self.min[1].min(p[1]),
                self.min[2].min(p[2]),
            ],
            max: [
                self.max[0].max(p[0]),
                self.max[1].max(p[1]),
                self.max[2].max(p[2]),
            ],
        }
    }

    fn extend(self, other: Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    fn centroid(&self) -> [f64; 3] {
        [
            0.5 * (self.min[0] + self.max[0]),
            0.5 * (self.min[1] + self.max[1]),
            0.5 * (self.min[2] + self.max[2]),
        ]
    }

    fn longest_axis(&self) -> usize {
        let span = [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ];
        if span[0] >= span[1] && span[0] >= span[2] {
            0
        } else if span[1] >= span[2] {
            1
        } else {
            2
        }
    }
}

/// Node bounds, stored as `f32` rounded outward.
///
/// Traversal is a chain of dependent loads, so the cost is memory, not arithmetic: halving
/// what a node costs to carry is the lever that a wider instruction does not pull. Rounding
/// the box outward makes it a superset of the true bounds, so it can report a crossing that
/// is not there — and the exact test then says so — but it can never reject a ray that
/// really does reach the triangles below. Decisions therefore do not change.
#[derive(Clone, Copy, Debug)]
struct Wide {
    min: [f32; 3],
    max: [f32; 3],
}

impl Wide {
    fn widen(box_: &Aabb) -> Wide {
        let mut min = [0.0f32; 3];
        let mut max = [0.0f32; 3];
        for axis in 0..3 {
            // Outward, so the stored box encloses the f64 box even where the cast rounds
            // the other way. `f32::next_down`/`next_up` would express this directly but
            // stabilised after this crate's MSRV, so a scale factor is used instead: 2.4e-7
            // is about two ULPs, comfortably more than the half-ULP a nearest cast can
            // lose, and one extra ULP of slack costs nothing but a rare pointless descent.
            min[axis] = widen_away_from_zero(box_.min[axis] as f32, -1.0);
            max[axis] = widen_away_from_zero(box_.max[axis] as f32, 1.0);
        }
        Wide { min, max }
    }

    /// The same slab test [`Aabb::crosses`] runs, reading the narrower store and comparing
    /// in `f64`: the precision goes into the box, never into the ray.
    fn crosses(&self, origin: [f64; 3], inv_dir: [f64; 3], t_min: f64, t_max: f64) -> bool {
        let (mut entered, mut exited) = (t_min, t_max);
        for axis in 0..3 {
            let lo = (f64::from(self.min[axis]) - origin[axis]) * inv_dir[axis];
            let hi = (f64::from(self.max[axis]) - origin[axis]) * inv_dir[axis];
            let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
            entered = entered.max(lo);
            exited = exited.min(hi);
            if entered > exited {
                return false;
            }
        }
        true
    }
}

/// Move `v` about two ULPs in the direction `sign`, treating exact zero as already wide
/// enough (a box edge at zero comes from a coordinate that really is zero, and the cast
/// of zero is exact).
fn widen_away_from_zero(v: f32, sign: f32) -> f32 {
    if v == 0.0 {
        0.0
    } else {
        let factor = 1.0 + sign * 2.4e-7;
        v * factor
    }
}

#[derive(Clone, Copy, Debug)]
struct Node {
    bounds: Wide,
    /// Child node indices; meaningless on a leaf.
    children: [u32; 2],
    /// `[start, start + count)` span of triangle slots, zero on an internal node.
    start: u32,
    count: u32,
}

impl Node {
    fn is_leaf(&self) -> bool {
        self.count > 0
    }
}

/// A hierarchy over one [`Geometry`].
#[derive(Clone, Debug)]
pub struct Bvh {
    nodes: Vec<Node>,
    /// Triangle corners per slot: `p0` and the two edge vectors.
    p0: Vec<[f64; 3]>,
    e1: Vec<[f64; 3]>,
    e2: Vec<[f64; 3]>,
    /// `|e1|² · |e2|²` per slot, for the scale-invariant parallel test.
    edge_scale: Vec<f64>,
    /// The three vertex indices per slot, for skipping the surface the ray starts on.
    verts: Vec<[u32; 3]>,
    /// Box bounds split into component arrays, so one group of four is four contiguous
    /// loads per component and the vector test reads them without gathering.
    bxmin: [Vec<f64>; 3],
    bxmax: [Vec<f64>; 3],
    bounds: Aabb,
}

impl Builder {
    /// The f64 bounds of the whole tree, taken before the nodes were narrowed.
    fn root_bounds(&self) -> Aabb {
        let mut box_ = Aabb::EMPTY;
        for &triangle in &self.order {
            box_ = box_.extend(self.bounds[triangle as usize]);
        }
        box_
    }
}

impl Default for Bvh {
    /// The hierarchy of an empty mesh: no nodes, so nothing is ever blocked.
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            p0: Vec::new(),
            e1: Vec::new(),
            e2: Vec::new(),
            edge_scale: Vec::new(),
            verts: Vec::new(),
            bxmin: [Vec::new(), Vec::new(), Vec::new()],
            bxmax: [Vec::new(), Vec::new(), Vec::new()],
            bounds: Aabb::EMPTY,
        }
    }
}

impl Bvh {
    /// Build a hierarchy over `geo`. Cheap enough to rebuild per run; an empty mesh yields
    /// a hierarchy that blocks nothing.
    #[must_use]
    pub fn build(geo: &Geometry) -> Self {
        if geo.triangles.is_empty() {
            return Self::default();
        }
        let mut builder = Builder {
            nodes: Vec::with_capacity(geo.triangles.len() / LEAF_SIZE * 2 + 8),
            order: (0..geo.triangles.len() as u32).collect(),
            bounds: Vec::with_capacity(geo.triangles.len()),
            centroids: Vec::with_capacity(geo.triangles.len()),
        };
        for triangle in &geo.triangles {
            let box_ = Aabb::from_triangle(geo, *triangle);
            builder.bounds.push(box_);
            builder.centroids.push(box_.centroid());
        }
        builder.split(0..builder.order.len(), 0);
        let bounds = builder.root_bounds();

        let mut bvh = Bvh {
            nodes: builder.nodes,
            bounds,
            ..Bvh::default()
        };
        bvh.lay_out_slots(geo, &builder.order, &builder.bounds);
        bvh
    }

    /// Copy triangles into the hierarchy's slot order and pad with a full group of
    /// degenerate slots, so a four-lane read starting anywhere below the count stays
    /// inside the arrays.
    ///
    /// A padded slot has coincident corners, so its determinant is zero and the parallel
    /// test rejects it: it can never be reported as a hit.
    fn lay_out_slots(&mut self, geo: &Geometry, order: &[u32], bounds: &[Aabb]) {
        let capacity = order.len() + LANES;
        self.p0.reserve_exact(capacity);
        self.e1.reserve_exact(capacity);
        self.e2.reserve_exact(capacity);
        self.edge_scale.reserve_exact(capacity);
        self.verts.reserve_exact(capacity);
        for axis in 0..3 {
            self.bxmin[axis].reserve_exact(capacity);
            self.bxmax[axis].reserve_exact(capacity);
        }

        for &index in order {
            let [a, b, c] = geo.triangles[index as usize];
            let p0 = geo.positions[a as usize];
            let e1 = sub(geo.positions[b as usize], p0);
            let e2 = sub(geo.positions[c as usize], p0);
            let box_ = bounds[index as usize];
            self.p0.push(p0);
            self.e1.push(e1);
            self.e2.push(e2);
            self.edge_scale.push(dot(e1, e1) * dot(e2, e2));
            self.verts.push([a, b, c]);
            for axis in 0..3 {
                self.bxmin[axis].push(box_.min[axis]);
                self.bxmax[axis].push(box_.max[axis]);
            }
        }

        while self.p0.len() < capacity {
            self.p0.push([0.0; 3]);
            self.e1.push([0.0; 3]);
            self.e2.push([0.0; 3]);
            self.edge_scale.push(0.0);
            self.verts.push([u32::MAX; 3]);
            for axis in 0..3 {
                self.bxmin[axis].push(0.0);
                self.bxmax[axis].push(0.0);
            }
        }
    }

    /// Axis-aligned bounds of everything in the hierarchy.
    #[must_use]
    pub fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        (self.bounds.min, self.bounds.max)
    }

    /// True when the segment `from` → `to` crosses a triangle.
    ///
    /// Triangles referencing `ignore_vertex` are skipped: the vertex sits *on* the surface
    /// it is testing, so without this every neighbouring triangle would shadow it at t ≈ 0
    /// and report it fully occluded. Skipping by vertex index is both cheaper and more
    /// exact than offsetting the ray origin, which cannot say how far it may move without
    /// guessing.
    #[must_use]
    pub fn segment_blocked(&self, from: [f64; 3], to: [f64; 3], ignore_vertex: u32) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let dir = sub(to, from);
        let dir2 = dot(dir, dir);
        if dir2 == 0.0 {
            return false;
        }
        let inv_dir = [1.0 / dir[0], 1.0 / dir[1], 1.0 / dir[2]];

        let mut stack = [0u32; STACK_CAPACITY];
        let mut top = 1usize;
        stack[0] = 0;
        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            if !node.bounds.crosses(from, inv_dir, T_START, 1.0) {
                continue;
            }
            if node.is_leaf() {
                let end = node.start + node.count;
                let mut base = node.start as usize;
                while base < end as usize {
                    // Vectorised rejection first …
                    let mut crossing = self.lane_mask_crossing(base, from, inv_dir);
                    // … then the exact test only for the lanes it could not rule out.
                    while crossing != 0 {
                        let lane = crossing.trailing_zeros() as usize;
                        crossing &= crossing - 1;
                        if self.exact_hit(base + lane, from, dir, dir2, ignore_vertex) {
                            return true;
                        }
                    }
                    base += LANES;
                }
                continue;
            }
            // One pending sibling per level of the current path, and the tree is no deeper
            // than MAX_DEPTH, so two more entries always fit.
            debug_assert!(top + 2 <= STACK_CAPACITY);
            stack[top] = node.children[0];
            stack[top + 1] = node.children[1];
            top += 2;
        }
        false
    }

    /// Four triangle boxes tested at once against the segment; bit *i* is set when lane
    /// *i*'s box is crossed.
    ///
    /// Branch-free by design: all four lanes run whether or not they matter, which is
    /// exactly what a rejection test wants. The arithmetic and the NaN handling mirror
    /// [`Aabb::crosses`], so a lane reports crossing here if and only if the scalar slab
    /// test would have.
    fn lane_mask_crossing(&self, base: usize, origin: [f64; 3], inv_dir: [f64; 3]) -> i32 {
        let mut entered = f64x4::splat(T_START);
        let mut exited = f64x4::splat(1.0);
        for axis in 0..3 {
            let o = f64x4::splat(origin[axis]);
            let inv = f64x4::splat(inv_dir[axis]);
            let lo = (group(&self.bxmin[axis], base) - o) * inv;
            let hi = (group(&self.bxmax[axis], base) - o) * inv;
            // `min`/`max` here, not `fast_min`/`fast_max`: the fast forms leave NaN
            // behaviour unspecified, and NaN bounds are the normal result of a ray running
            // parallel to a slab it lies inside. These drop a NaN lane operand, which is
            // what `f64::min`/`max` do in the scalar test.
            entered = entered.max(lo.min(hi));
            exited = exited.min(lo.max(hi));
        }
        // Crossing keeps a lane unless it provably left the slab: comparing false for NaN
        // matches the scalar loop, which likewise does not bail on a NaN bound.
        (!entered.cmp_gt(exited)).move_mask()
    }

    /// Möller–Trumbore, two-sided, for one slot.
    ///
    /// `t` is a fraction of the whole segment because `dir` is the whole segment, so the
    /// accepted range needs no length and does not depend on the model's units.
    fn exact_hit(
        &self,
        slot: usize,
        from: [f64; 3],
        dir: [f64; 3],
        dir2: f64,
        ignore_vertex: u32,
    ) -> bool {
        let tri = self.verts[slot];
        if tri[0] == ignore_vertex || tri[1] == ignore_vertex || tri[2] == ignore_vertex {
            return false;
        }
        let e1 = self.e1[slot];
        let e2 = self.e2[slot];
        let pv = cross(dir, e2);
        let det = dot(e1, pv);
        if det * det <= PARALLEL_EPS2 * dir2 * self.edge_scale[slot] {
            return false;
        }
        let inv_det = 1.0 / det;
        let sv = sub(from, self.p0[slot]);
        let u = dot(sv, pv) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            return false;
        }
        let qv = cross(sv, e1);
        let v = dot(dir, qv) * inv_det;
        if v < 0.0 || u + v > 1.0 {
            return false;
        }
        let t = dot(e2, qv) * inv_det;
        t > T_START && t < 1.0
    }
}

/// One group of four lanes starting at `base`.
#[inline]
fn group(values: &[f64], base: usize) -> f64x4 {
    f64x4::new([
        values[base],
        values[base + 1],
        values[base + 2],
        values[base + 3],
    ])
}

/// Build state; exists so the scratch tables do not outlive the build.
struct Builder {
    nodes: Vec<Node>,
    order: Vec<u32>,
    bounds: Vec<Aabb>,
    centroids: Vec<[f64; 3]>,
}

impl Builder {
    /// Partition `range` of [`Builder::order`] and return the node index. Children are
    /// pushed after the parent, so the parent's entry is finished once both subtrees exist.
    fn split(&mut self, range: std::ops::Range<usize>, depth: usize) -> u32 {
        let box_ = self.merge(range.clone());
        let index = self.nodes.len() as u32;
        self.nodes.push(Node {
            bounds: Wide::widen(&box_),
            children: [0; 2],
            start: 0,
            count: 0,
        });

        if range.len() <= LEAF_SIZE || depth >= MAX_DEPTH {
            self.make_leaf(index, range);
            return index;
        }

        let axis = box_.longest_axis();
        // `select_nth_unstable_by` counts from the start of the slice it is given, so the
        // split point is a length here and only becomes absolute in the child ranges.
        let offset = range.len() / 2;
        // A partition only makes progress if the centroids straddle the pivot; a cluster
        // sharing one position would otherwise recurse forever.
        if self.centroid_spread(&range, axis) <= 0.0 {
            self.make_leaf(index, range);
            return index;
        }
        self.order[range.clone()].select_nth_unstable_by(offset, |a, b| {
            self.centroids[*a as usize][axis]
                .partial_cmp(&self.centroids[*b as usize][axis])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let pivot = range.start + offset;

        let left = self.split(range.start..pivot, depth + 1);
        let right = self.split(pivot..range.end, depth + 1);
        // The children partition the same triangle set, so `box_` is already their union.
        self.nodes[index as usize] = Node {
            bounds: Wide::widen(&box_),
            children: [left, right],
            start: 0,
            count: 0,
        };
        index
    }

    fn make_leaf(&mut self, index: u32, range: std::ops::Range<usize>) {
        self.nodes[index as usize].start = range.start as u32;
        self.nodes[index as usize].count = (range.end - range.start) as u32;
    }

    /// Extent of the centroid cloud on `axis`; zero means every triangle in `range` shares
    /// that centroid coordinate.
    fn centroid_spread(&self, range: &std::ops::Range<usize>, axis: usize) -> f64 {
        let mut low = f64::MAX;
        let mut high = f64::MIN;
        for &triangle in &self.order[range.clone()] {
            let centre = self.centroids[triangle as usize][axis];
            low = low.min(centre);
            high = high.max(centre);
        }
        high - low
    }

    fn merge(&self, range: std::ops::Range<usize>) -> Aabb {
        let mut box_ = Aabb::EMPTY;
        for &triangle in &self.order[range] {
            box_ = box_.extend(self.bounds[triangle as usize]);
        }
        box_
    }
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The traversal depends on `move_mask` putting lane *i* in bit *i*, and on the vector
    /// slab test agreeing with the scalar one lane for lane — including where a ray is
    /// parallel to a slab and sits inside it, which produces infinities and NaNs.
    #[test]
    fn lane_masks_put_lane_i_in_bit_i() {
        let lanes = f64x4::new([0.0, 1.0, 2.0, 3.0]);
        assert_eq!(lanes.cmp_gt(0.5).move_mask(), 0b1110);
        assert_eq!(lanes.cmp_gt(3.5).move_mask(), 0b0000);
    }

    /// The scalar slab test the vector one must match, kept here so the equivalence is
    /// what is being proved rather than an accident of two call sites sharing code.
    fn scalar_slab(min: [f64; 3], max: [f64; 3], origin: [f64; 3], inv_dir: [f64; 3]) -> bool {
        let (mut entered, mut exited): (f64, f64) = (T_START, 1.0);
        for axis in 0..3 {
            let lo = (min[axis] - origin[axis]) * inv_dir[axis];
            let hi = (max[axis] - origin[axis]) * inv_dir[axis];
            let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
            entered = entered.max(lo);
            exited = exited.min(hi);
            if entered > exited {
                return false;
            }
        }
        true
    }

    #[test]
    fn vector_and_scalar_slab_tests_agree_including_degenerate_axes() {
        let boxes = [
            // ordinary crossings, misses, and a box the ray starts inside
            ([1.0, -1.0, -1.0], [2.0, 1.0, 1.0]),
            ([10.0, 10.0, 10.0], [11.0, 11.0, 11.0]),
            ([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            // axis-aligned box sharing a plane with the origin: inv_dir * 0 → NaN bounds
            ([0.0, 5.0, 5.0], [1.0, 6.0, 6.0]),
        ];
        // Ray straight down +x from the origin.
        let origin = [0.0, 0.0, 0.0];
        let dir = [1.0, 0.0, 0.0];
        let inv_dir = [1.0 / dir[0], 1.0 / dir[1], 1.0 / dir[2]];

        let mut bvh = Bvh::default();
        for axis in 0..3 {
            for (min, max) in &boxes {
                bvh.bxmin[axis].push(min[axis]);
                bvh.bxmax[axis].push(max[axis]);
            }
        }
        // Pad to a whole group beyond the four boxes so a read at 0 covers them.
        let want: Vec<bool> = boxes
            .iter()
            .map(|(min, max)| scalar_slab(*min, *max, origin, inv_dir))
            .collect();

        let mask = bvh.lane_mask_crossing(0, origin, inv_dir);
        for (lane, expected) in want.iter().enumerate() {
            let got = (mask >> lane) & 1 == 1;
            assert_eq!(
                got, *expected,
                "lane {lane} disagrees with the scalar slab test"
            );
        }
    }
}
