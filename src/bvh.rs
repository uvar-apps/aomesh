//! A bounding-volume hierarchy over the mesh's triangles, queried with
//! blocked/unblocked segments.
//!
//! Per-vertex occlusion is a lot of rays: 10k vertices times 256 viewpoints is
//! 2.5M segments, and testing each against every triangle of a 20k-triangle
//! mesh is 5×10¹⁰ intersection tests. The hierarchy turns that into a depth-first
//! walk down only the boxes the segment actually crosses.
//!
//! Two properties matter for the numbers the bake reports:
//!
//! * **Triangle order is never assumed.** The hierarchy permutes triangle
//!   indices internally and the caller's [`Geometry`](crate::geometry::Geometry)
//!   stays untouched.
//! * **A hit is scale-invariant.** Both the "ray parallel to the triangle" test
//!   and the parametric hit range are ratios, so a mesh authored in microns and
//!   one authored in kilometres report the same visibility.

use crate::geometry::Geometry;

/// Triangles per leaf. Below this the traversal costs more than the brute-force
/// test it replaces.
const LEAF_SIZE: usize = 4;

/// Recursion cap. Splitting stops early on degenerate clusters; this bounds the
/// worst case so the traversal stack cannot outgrow [`STACK_CAPACITY`].
const MAX_DEPTH: usize = 40;

/// Fixed-point capacity of the traversal stack; a tree of [`MAX_DEPTH`] needs at
/// most that many pending nodes.
const STACK_CAPACITY: usize = MAX_DEPTH + 4;

/// Near-parallel rejection, squared: `|det| ≤ 1e-12 · |dir| · |e1| · |e2|`.
/// A coplanar segment and triangle produce a singular system whose barycentric
/// solution is meaningless, so it must be rejected rather than evaluated.
const PARALLEL_EPS2: f64 = 1e-24;

/// A hit strictly closer than this fraction of the segment is the surface the
/// ray started on, not an occluder.
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

    /// Slab test against the segment `origin + dir·t`, `t ∈ (t_min, t_max)`.
    ///
    /// A `dir` component of zero makes `inv_dir` infinite and the slab bounds
    /// NaN when the origin lies exactly in that slab plane — which is the
    /// everyday case for a ray starting on axis-aligned geometry. `f64::min` and
    /// `f64::max` discard a NaN operand, so such an axis simply imposes no
    /// constraint, as it should: the ray is parallel to the slab and inside it.
    fn crosses(&self, origin: [f64; 3], inv_dir: [f64; 3], t_min: f64, t_max: f64) -> bool {
        let (mut entered, mut exited) = (t_min, t_max);
        for axis in 0..3 {
            let lo = (self.min[axis] - origin[axis]) * inv_dir[axis];
            let hi = (self.max[axis] - origin[axis]) * inv_dir[axis];
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

#[derive(Clone, Copy, Debug)]
struct Node {
    bounds: Aabb,
    /// Child node indices; meaningless on a leaf.
    children: [u32; 2],
    /// `[start, start + count)` span of [`Bvh::order`]; zero on an internal node.
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
    order: Vec<u32>,
    /// Per triangle, `|e1|² · |e2|²`, for the parallel test.
    edge_scales: Vec<f64>,
    bounds: Aabb,
}

impl Default for Bvh {
    /// The hierarchy of an empty mesh: no nodes, so nothing is ever blocked.
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            order: Vec::new(),
            edge_scales: Vec::new(),
            bounds: Aabb::EMPTY,
        }
    }
}

impl Bvh {
    /// Build a hierarchy over `geo`. Cheap enough to rebuild per run; an empty
    /// mesh yields a hierarchy that blocks nothing.
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
            edge_scales: Vec::with_capacity(geo.triangles.len()),
        };
        for triangle in &geo.triangles {
            let box_ = Aabb::from_triangle(geo, *triangle);
            // Indexed by original triangle index, and it stays that way: the
            // hierarchy permutes `order`, never these tables.
            builder.bounds.push(box_);
            builder.centroids.push(box_.centroid());
            let e1 = sub(
                geo.positions[triangle[1] as usize],
                geo.positions[triangle[0] as usize],
            );
            let e2 = sub(
                geo.positions[triangle[2] as usize],
                geo.positions[triangle[0] as usize],
            );
            builder.edge_scales.push(dot(e1, e1) * dot(e2, e2));
        }
        let root = builder.split(0..builder.order.len(), 0);
        let bounds = builder.nodes[root as usize].bounds;
        Bvh {
            nodes: builder.nodes,
            order: builder.order,
            edge_scales: builder.edge_scales,
            bounds,
        }
    }

    /// Axis-aligned bounds of everything in the hierarchy.
    #[must_use]
    pub fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        (self.bounds.min, self.bounds.max)
    }

    /// True when the segment `from` → `to` crosses a triangle.
    ///
    /// Triangles that reference `ignore_vertex` are skipped. A vertex sits *on*
    /// the surface it is testing, so without this every triangle touching that
    /// vertex would shadow it at t ≈ 0 and report it fully occluded. Skipping by
    /// vertex index is both cheaper and more exact than offsetting the origin,
    /// which cannot say how far it may move without guessing.
    #[must_use]
    pub fn segment_blocked(
        &self,
        geo: &Geometry,
        from: [f64; 3],
        to: [f64; 3],
        ignore_vertex: u32,
    ) -> bool {
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
                for slot in node.start..node.start + node.count {
                    let triangle_index = self.order[slot as usize];
                    let triangle = geo.triangles[triangle_index as usize];
                    if triangle[0] == ignore_vertex
                        || triangle[1] == ignore_vertex
                        || triangle[2] == ignore_vertex
                    {
                        continue;
                    }
                    if self.crosses_triangle(geo, triangle, triangle_index, from, dir, dir2) {
                        return true;
                    }
                }
                continue;
            }
            // One pending sibling per level of the current path, and the tree is no
            // deeper than MAX_DEPTH, so two more entries always fit.
            debug_assert!(top + 2 <= STACK_CAPACITY);
            stack[top] = node.children[0];
            stack[top + 1] = node.children[1];
            top += 2;
        }
        false
    }

    /// Möller–Trumbore, two-sided: winding tells us nothing about occlusion, and
    /// `t` is a fraction of the segment because `dir` is the whole segment.
    fn crosses_triangle(
        &self,
        geo: &Geometry,
        triangle: [u32; 3],
        triangle_index: u32,
        from: [f64; 3],
        dir: [f64; 3],
        dir2: f64,
    ) -> bool {
        let p0 = geo.positions[triangle[0] as usize];
        let p1 = geo.positions[triangle[1] as usize];
        let p2 = geo.positions[triangle[2] as usize];
        let e1 = sub(p1, p0);
        let e2 = sub(p2, p0);
        let pv = cross(dir, e2);
        let det = dot(e1, pv);
        if det * det <= PARALLEL_EPS2 * dir2 * self.edge_scales[triangle_index as usize] {
            return false;
        }
        let inv_det = 1.0 / det;
        let sv = sub(from, p0);
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

/// Build state; exists so the scratch tables do not outlive the build.
struct Builder {
    nodes: Vec<Node>,
    order: Vec<u32>,
    bounds: Vec<Aabb>,
    centroids: Vec<[f64; 3]>,
    edge_scales: Vec<f64>,
}

impl Builder {
    /// Partition `range` of [`Builder::order`] and return the node index.
    /// Children are pushed after the parent, so a node's own bounds have to be
    /// written once both subtrees exist.
    fn split(&mut self, range: std::ops::Range<usize>, depth: usize) -> u32 {
        let box_ = self.merge(range.clone());
        let index = self.nodes.len() as u32;
        self.nodes.push(Node {
            bounds: box_,
            children: [0; 2],
            start: 0,
            count: 0,
        });

        if range.len() <= LEAF_SIZE || depth >= MAX_DEPTH {
            self.make_leaf(index, range);
            return index;
        }

        let axis = box_.longest_axis();
        // `select_nth_unstable_by` counts from the start of the slice it is given,
        // so the split point is a length here and only becomes an absolute index
        // when the child ranges are built.
        let offset = range.len() / 2;
        // A partition only makes progress if the centroids actually straddle the
        // pivot; a cluster sharing one position would recurse forever.
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
        // The children partition the same triangle set, so `box_` is already
        // their union.
        self.nodes[index as usize] = Node {
            bounds: box_,
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

    /// Extent of the centroid cloud on `axis`. Zero means every triangle in
    /// `range` shares that centroid coordinate, so no partition can progress.
    fn centroid_spread(&self, range: &std::ops::Range<usize>, axis: usize) -> f64 {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for &triangle in &self.order[range.clone()] {
            let c = self.centroids[triangle as usize][axis];
            lo = lo.min(c);
            hi = hi.max(c);
        }
        hi - lo
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
