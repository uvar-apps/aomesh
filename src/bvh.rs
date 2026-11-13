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

/// Temporary instrumentation: what one shadow ray actually walks.
#[cfg(feature = "count")]
pub mod counters {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static RAYS: AtomicU64 = AtomicU64::new(0);
    pub static POPS: AtomicU64 = AtomicU64::new(0);
    pub static BOXES_CROSSED: AtomicU64 = AtomicU64::new(0);
    pub static LEAVES: AtomicU64 = AtomicU64::new(0);
    pub static TRIS: AtomicU64 = AtomicU64::new(0);
    pub static SKIPPED_INCIDENT: AtomicU64 = AtomicU64::new(0);
    pub static MT: AtomicU64 = AtomicU64::new(0);
    pub static HITS: AtomicU64 = AtomicU64::new(0);
    pub fn add(slot: &AtomicU64, n: u64) { slot.fetch_add(n, Ordering::Relaxed); }
}

/// Triangles per leaf. Below this the traversal costs more than the brute-force
/// test it replaces.
const LEAF_SIZE: usize = 4;

/// Partition candidates per node in the surface-area-heuristic scan.
///
/// Binned rather than exact: an exact SAH sweep would sort the range at every node,
/// which costs more than the tree it buys back. Sixteen bins find a split within a
/// few percent of the optimum in one pass over the triangles, and the per-bin bounds
/// that pass builds double as the children's bounds, which removes the O(range) merge
/// the median split used to do at every node.
const BINS: usize = 16;

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
    /// `[start, start + count)` span of [`Bvh::slots`]; zero on an internal node.
    start: u32,
    count: u32,
}

impl Node {
    fn is_leaf(&self) -> bool {
        self.count > 0
    }
}

/// One triangle in the form the intersection test wants, so a leaf reads its
/// triangles straight out of this record instead of gathering them.
///
/// The traversal used to recover `p0`, `e1` and `e2` from
/// [`Geometry::positions`] on every test — three loads at three unrelated
/// addresses, chosen by a triangle index, so the fetch that a hit depends on is
/// usually a cache miss by the time the test reaches it. Storing the two edge
/// vectors alongside the origin, in leaf order, replaces those gathers with one
/// sequential read: nine `f64` plus the vertex indices, 96 bytes, and a leaf's
/// four triangles sit in six adjacent cache lines.
///
/// The values are the same ones the gathers produced — `e1` is `positions[b] -
/// positions[a]` wherever it is computed — so no intersection decision changes.
#[derive(Clone, Copy, Debug)]
struct Slot {
    p0: [f64; 3],
    e1: [f64; 3],
    e2: [f64; 3],
    /// `|e1|² · |e2|²`, for the scale-invariant parallel test.
    edge_scale: f64,
    /// The three corners, to skip the surface a ray starts on.
    verts: [u32; 3],
}

/// A hierarchy over one [`Geometry`].
#[derive(Clone, Debug)]
pub struct Bvh {
    nodes: Vec<Node>,
    /// Triangles in hierarchy order; a leaf's `start..start + count` indexes this.
    slots: Vec<Slot>,
    bounds: Aabb,
}

impl Default for Bvh {
    /// The hierarchy of an empty mesh: no nodes, so nothing is ever blocked.
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            slots: Vec::new(),
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
            bins: vec![0; geo.triangles.len()],
        };
        for triangle in &geo.triangles {
            let box_ = Aabb::from_triangle(geo, *triangle);
            // Indexed by original triangle index, and it stays that way: the
            // hierarchy permutes `order`, never these tables.
            builder.bounds.push(box_);
            builder.centroids.push(box_.centroid());
        }
        let root_box = builder.merge(0..builder.order.len());
        let root = builder.split(0..builder.order.len(), 0, root_box);
        let bounds = builder.nodes[root as usize].bounds;
        let slots = builder.reorder(geo);
        let nodes = pack_siblings(builder.nodes, root as usize);
        Bvh {
            nodes,
            slots,
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

        #[cfg(feature = "count")]
        counters::add(&counters::RAYS, 1);
        if !self.nodes[0].bounds.crosses(from, inv_dir, T_START, 1.0) {
            return false;
        }
        // The stack holds only nodes whose box the segment is known to cross: a
        // node's children are tested here, at the parent, rather than being pushed
        // blind and tested when popped. Two consequences, both measured — half as
        // many entries to push and pop, and the pair of child boxes is a pair of
        // *independent* loads issued together (adjacent in memory since
        // `pack_siblings`) instead of two dependent chases at opposite ends of the
        // walk. Of the 55 nodes a segment used to reach, 23 were popped only to be
        // rejected.
        let mut stack = [0u32; STACK_CAPACITY];
        let mut top = 1usize;
        stack[0] = 0;
        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            #[cfg(feature = "count")]
            counters::add(&counters::POPS, 1);
            if node.is_leaf() {
                #[cfg(feature = "count")]
                {
                    counters::add(&counters::LEAVES, 1);
                    counters::add(&counters::TRIS, node.count as u64);
                }
                let span = node.start as usize..(node.start + node.count) as usize;
                for slot in &self.slots[span] {
                    if slot.verts[0] == ignore_vertex
                        || slot.verts[1] == ignore_vertex
                        || slot.verts[2] == ignore_vertex
                    {
                        #[cfg(feature = "count")]
                        counters::add(&counters::SKIPPED_INCIDENT, 1);
                        continue;
                    }
                    #[cfg(feature = "count")]
                    counters::add(&counters::MT, 1);
                    if crosses_triangle(slot, from, dir, dir2) {
                        #[cfg(feature = "count")]
                        counters::add(&counters::HITS, 1);
                        return true;
                    }
                }
                continue;
            }
            // Only children the segment actually reaches are kept.
            #[cfg(feature = "count")]
            counters::add(&counters::BOXES_CROSSED, 2);
            let left = &self.nodes[node.children[0] as usize];
            let right = &self.nodes[node.children[1] as usize];
            let left_crosses = left.bounds.crosses(from, inv_dir, T_START, 1.0);
            let right_crosses = right.bounds.crosses(from, inv_dir, T_START, 1.0);
            // One pending sibling per level of the current path, and the tree is no
            // deeper than MAX_DEPTH, so two more entries always fit.
            debug_assert!(top + 2 <= STACK_CAPACITY);
            if left_crosses {
                stack[top] = node.children[0];
                top += 1;
            }
            if right_crosses {
                stack[top] = node.children[1];
                top += 1;
            }
        }
        false
    }
}

/// Möller–Trumbore, two-sided: winding tells us nothing about occlusion, and
/// `t` is a fraction of the segment because `dir` is the whole segment.
///
/// Each test bails at the first barycentric condition it fails, which is why the
/// intersection stays scalar: a shadow ray usually hits nothing, and a miss costs
/// a handful of operations here rather than the full arithmetic a batched lane
/// would have to complete. See the module documentation for what that means for
/// the vector instructions.
fn crosses_triangle(slot: &Slot, from: [f64; 3], dir: [f64; 3], dir2: f64) -> bool {
    let e1 = slot.e1;
    let e2 = slot.e2;
    let pv = cross(dir, e2);
    let det = dot(e1, pv);
    if det * det <= PARALLEL_EPS2 * dir2 * slot.edge_scale {
        return false;
    }
    let inv_det = 1.0 / det;
    let sv = sub(from, slot.p0);
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

/// Rebuild the node array depth-first with the two children of every node next to
/// each other.
///
/// `Builder::split` lays the tree out in the order the recursion creates it, which
/// puts an internal node's *left* child immediately after itself and its *right*
/// child after everything in the left subtree — the one index the traversal cannot
/// prefetch from the parent. Each `Node` is exactly 64 bytes, one cache line, so a
/// pair stored side by side is the two lines a descending segment will want, in the
/// order it wants them.
///
/// Slot numbers change; nothing else does. Every node keeps its box, its count and
/// its span of [`Bvh::slots`], and the links are rewritten from the same placement
/// table, so a segment reaches exactly the triangles it reached before.
fn pack_siblings(nodes: Vec<Node>, root: usize) -> Vec<Node> {
    // New index of each old node, filled as slots are handed out.
    let mut placed = vec![u32::MAX; nodes.len()];
    let mut packed: Vec<Node> = Vec::with_capacity(nodes.len());
    // Old index of each packed entry, so the links can be resolved afterwards: a
    // node's children are usually placed after it, so their new numbers are not
    // known at the moment it is copied.
    let mut origin: Vec<u32> = Vec::with_capacity(nodes.len());

    placed[root] = 0;
    packed.push(nodes[root]);
    origin.push(root as u32);

    // Pending internal nodes. Popping one reserves two adjacent slots for its
    // children and queues them, left last so the left subtree is packed next.
    let mut pending = vec![root];
    while let Some(old) = pending.pop() {
        let node = nodes[old];
        if node.is_leaf() {
            continue;
        }
        for &child in &node.children {
            let child = child as usize;
            placed[child] = packed.len() as u32;
            packed.push(nodes[child]);
            origin.push(child as u32);
        }
        pending.push(node.children[1] as usize);
        pending.push(node.children[0] as usize);
    }

    debug_assert_eq!(packed.len(), nodes.len(), "every node placed once");
    for index in 0..packed.len() {
        let old = nodes[origin[index] as usize];
        if old.is_leaf() {
            continue;
        }
        packed[index].children = [
            placed[old.children[0] as usize],
            placed[old.children[1] as usize],
        ];
    }
    packed
}

/// Build state; exists so the scratch tables do not outlive the build.
struct Builder {
    nodes: Vec<Node>,
    order: Vec<u32>,
    bounds: Vec<Aabb>,
    centroids: Vec<[f64; 3]>,
    /// Bin index of each triangle, written by [`Builder::bin`] and read by the
    /// partition that follows it; a stale label is never read, because every node
    /// labels its own triangles before moving them.
    bins: Vec<u8>,
}

impl Builder {
    /// Copy the triangles into hierarchy order as [`Slot`] records.
    ///
    /// Done once, after the tree is finished: the partition moves indices, which
    /// is cheap, and this pass turns them into bytes, which is not — so the
    /// records are written exactly once in the order the traversal will read them.
    fn reorder(&self, geo: &Geometry) -> Vec<Slot> {
        self.order
            .iter()
            .map(|&index| {
                let [a, b, c] = geo.triangles[index as usize];
                let p0 = geo.positions[a as usize];
                let e1 = sub(geo.positions[b as usize], p0);
                let e2 = sub(geo.positions[c as usize], p0);
                Slot {
                    p0,
                    e1,
                    e2,
                    edge_scale: dot(e1, e1) * dot(e2, e2),
                    verts: [a, b, c],
                }
            })
            .collect()
    }

    /// Partition `range` of [`Builder::order`] and return the node index.
    ///
    /// `box_` is the union of the range's triangles, handed down from the parent's bin
    /// aggregates rather than recomputed here. Children are pushed after the parent, so
    /// a node's own entry is finished once both subtrees exist.
    fn split(&mut self, range: std::ops::Range<usize>, depth: usize, box_: Aabb) -> u32 {
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
        let Some((low, width)) = self.centroid_extent(&range, axis) else {
            // Every centroid shares this coordinate, so no split on this axis can
            // make progress; the node keeps its triangles.
            self.make_leaf(index, range);
            return index;
        };

        let partition = self.bin(range.clone(), axis, low, width, box_);
        let Some((left_count, left, right)) = partition else {
            self.make_leaf(index, range);
            return index;
        };

        // Move the cheapest cut's triangles to the front by rank: `left_count` is the
        // number of triangles in the bins at or below the cut, so ordering by bin index
        // and stopping there puts exactly those in the left child.
        let Builder {
            order, bins, ..
        } = self;
        order[range.clone()].select_nth_unstable_by(left_count, |a, b| {
            bins[*a as usize].cmp(&bins[*b as usize])
        });
        let pivot = range.start + left_count;
        debug_assert!(left_count > 0 && left_count < range.len(), "split makes progress");

        let left_child = self.split(range.start..pivot, depth + 1, left);
        let right_child = self.split(pivot..range.end, depth + 1, right);
        // The children partition the same triangle set, so `box_` is already their union.
        self.nodes[index as usize] = Node {
            bounds: box_,
            children: [left_child, right_child],
            start: 0,
            count: 0,
        };
        index
    }

    /// Bin `range` along `axis` and cost every interior cut.
    ///
    /// Returns how many triangles the best cut puts on the left, with the bounds of the
    /// two sides, or `None` when no cut beats keeping the triangles in one leaf. One pass fills the bins, a second
    /// sweeps the left side, a third the right; each bin carries the count and bounds of
    /// the triangles that fell in it, which is all the heuristic reads.
    fn bin(
        &mut self,
        range: std::ops::Range<usize>,
        axis: usize,
        low: f64,
        width: f64,
        parent: Aabb,
    ) -> Option<(usize, Aabb, Aabb)> {
        let mut counts = [0u32; BINS];
        let mut minimum = [[f64::MAX; 3]; BINS];
        let mut maximum = [[f64::MIN; 3]; BINS];

        for position in range.clone() {
            let triangle = self.order[position] as usize;
            let centroid = self.centroids[triangle][axis];
            let bin = (((centroid - low) / width) as usize).min(BINS - 1);
            // Labelled by triangle, not by position: the partition below compares the
            // triangles it moves, and a triangle appears exactly once in `order`, so
            // its label follows it wherever it is swapped to.
            self.bins[triangle] = bin as u8;
            counts[bin] += 1;
            let source = &self.bounds[triangle];
            for coordinate in 0..3 {
                minimum[bin][coordinate] = minimum[bin][coordinate].min(source.min[coordinate]);
                maximum[bin][coordinate] = maximum[bin][coordinate].max(source.max[coordinate]);
            }
        }

        /// Surface area, the SA in SAH. A box's own area normalises the split cost, so
        /// what is compared is the fraction of the parent's surface each child covers,
        /// weighted by how many triangles it holds.
        fn area(min: [f64; 3], max: [f64; 3]) -> f64 {
            let span = [
                (max[0] - min[0]).max(0.0),
                (max[1] - min[1]).max(0.0),
                (max[2] - min[2]).max(0.0),
            ];
            2.0 * (span[0] * span[1] + span[1] * span[2] + span[2] * span[0])
        }

        // Right-hand aggregates, so the sweep over cuts keeps only the left running.
        let mut right_counts = [0u32; BINS + 1];
        let mut right_min = [[f64::MAX; 3]; BINS + 1];
        let mut right_max = [[f64::MIN; 3]; BINS + 1];
        for bin in (0..BINS).rev() {
            right_counts[bin] = right_counts[bin + 1] + counts[bin];
            for coordinate in 0..3 {
                right_min[bin][coordinate] = right_min[bin + 1][coordinate]
                    .min(if counts[bin] > 0 { minimum[bin][coordinate] } else { f64::MAX });
                right_max[bin][coordinate] = right_max[bin + 1][coordinate]
                    .max(if counts[bin] > 0 { maximum[bin][coordinate] } else { f64::MIN });
            }
        }

        let mut left_counts = 0u32;
        let mut left_min = [f64::MAX; 3];
        let mut left_max = [f64::MIN; 3];
        let mut best = f64::INFINITY;
        let mut best_cut = None;
        for cut in 0..BINS - 1 {
            if counts[cut] == 0 {
                continue;
            }
            left_counts += counts[cut];
            for coordinate in 0..3 {
                left_min[coordinate] = left_min[coordinate].min(minimum[cut][coordinate]);
                left_max[coordinate] = left_max[coordinate].max(maximum[cut][coordinate]);
            }
            let right_count = right_counts[cut + 1];
            if right_count == 0 {
                continue;
            }
            // Costed as area x triangles for both children together, in the same units
            // as the leaf comparison below. A child's area over the parent's is the
            // chance a segment that reached the parent reaches it, so the sum estimates
            // how many triangles a random segment will have to test.
            let cost_unnormalised = area(left_min, left_max) * left_counts as f64
                + area(right_min[cut + 1], right_max[cut + 1]) * right_count as f64;
            if cost_unnormalised < best {
                best = cost_unnormalised;
                best_cut = Some((left_counts as usize,
                    Aabb {
                        min: left_min,
                        max: left_max,
                    },
                    Aabb {
                        min: right_min[cut + 1],
                        max: right_max[cut + 1],
                    },
                ));
            }
        }

        let (count, left, right) = match best_cut {
            Some(best) => best,
            None => return None,
        };
        // The same estimate for a leaf is the parent's area times every triangle in it:
        // a segment that reaches the node then tests all of them. Splitting has to beat
        // that, or the extra box tests would cost more than they save.
        let leaf_cost = area(parent.min, parent.max) * range.len() as f64;
        if best >= leaf_cost {
            return None;
        }
        Some((count, left, right))
    }

    /// `low` and the bin width spanning this range's centroids on `axis`, or `None` if
    /// they all coincide and no partition could separate them.
    fn centroid_extent(&self, range: &std::ops::Range<usize>, axis: usize) -> Option<(f64, f64)> {
        let mut low = f64::MAX;
        let mut high = f64::MIN;
        for &triangle in &self.order[range.clone()] {
            let centre = self.centroids[triangle as usize][axis];
            low = low.min(centre);
            high = high.max(centre);
        }
        let span = high - low;
        if span > 0.0 && span.is_finite() {
            Some((low, span / BINS as f64))
        } else {
            None
        }
    }

    fn make_leaf(&mut self, index: u32, range: std::ops::Range<usize>) {
        self.nodes[index as usize].start = range.start as u32;
        self.nodes[index as usize].count = (range.end - range.start) as u32;
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
mod validity {
    use super::*;

    /// Floor plus an overhead octagon: enough triangles to split, few enough to read.
    fn fixture() -> Geometry {
        let mut positions = vec![
            [-8.0, -8.0, 0.0],
            [8.0, -8.0, 0.0],
            [8.0, 8.0, 0.0],
            [-8.0, 8.0, 0.0],
        ];
        let mut triangles = vec![[0u32, 1, 2], [0, 2, 3]];
        let centre = positions.len() as u32;
        positions.push([0.0, 0.0, 4.0]);
        for step in 0..8 {
            let a = positions.len() as u32;
            let first = step as f64 * std::f64::consts::FRAC_PI_4;
            positions.push([3.0 * first.cos(), 3.0 * first.sin(), 4.0]);
            let second = a + 1;
            positions.push([
                3.0 * ((step + 1) as f64 * std::f64::consts::FRAC_PI_4).cos(),
                3.0 * ((step + 1) as f64 * std::f64::consts::FRAC_PI_4).sin(),
                4.0,
            ]);
            triangles.push([centre, a, second]);
        }
        let count = positions.len();
        Geometry {
            positions,
            normals: vec![[0.0, 0.0, 1.0]; count],
            triangles,
        }
    }

    fn triangle_box(geo: &Geometry, slot: &Slot) -> Aabb {
        let mut box_ = Aabb::EMPTY;
        for index in slot.verts {
            box_ = box_.include(geo.positions[index as usize]);
        }
        box_
    }

    /// The invariant a hierarchy must hold for a box rejection to be sound: whatever a
    /// node claims to cover contains every triangle below it, and every triangle below
    /// it is claimed exactly once.
    #[test]
    fn every_triangle_sits_inside_every_box_above_it() {
        let geo = fixture();
        let bvh = Bvh::build(&geo);
        let mut seen = vec![0usize; geo.triangles.len()];
        let mut total = 0usize;
        let mut stack = vec![(0usize, Aabb::EMPTY, true)];
        while let Some((index, parent, is_root)) = stack.pop() {
            let node = &bvh.nodes[index];
            if !is_root {
                for coordinate in 0..3 {
                    assert!(
                        node.bounds.min[coordinate] >= parent.min[coordinate] - 1e-9
                            && node.bounds.max[coordinate] <= parent.max[coordinate] + 1e-9,
                        "node {index} box {:?}..{:?} escapes its parent {:?}..{:?}",
                        node.bounds.min,
                        node.bounds.max,
                        parent.min,
                        parent.max
                    );
                }
            }
            if node.is_leaf() {
                total += node.count as usize;
                for slot in bvh.slots[node.start as usize..(node.start + node.count) as usize].iter()
                {
                    let box_ = triangle_box(&geo, slot);
                    for coordinate in 0..3 {
                        assert!(
                            box_.min[coordinate] >= node.bounds.min[coordinate] - 1e-9
                                && box_.max[coordinate] <= node.bounds.max[coordinate] + 1e-9,
                            "triangle {:?} box {:?}..{:?} escapes leaf {index} box {:?}..{:?}",
                            slot.verts,
                            box_.min,
                            box_.max,
                            node.bounds.min,
                            node.bounds.max
                        );
                    }
                    let found = geo
                        .triangles
                        .iter()
                        .position(|t| {
                            t[0] == slot.verts[0]
                                && t[1] == slot.verts[1]
                                && t[2] == slot.verts[2]
                        })
                        .expect("slot is not a mesh triangle");
                    seen[found] += 1;
                }
            } else {
                stack.push((node.children[0] as usize, node.bounds, false));
                stack.push((node.children[1] as usize, node.bounds, false));
            }
        }
        assert_eq!(total, geo.triangles.len(), "each triangle in exactly one leaf");
        for (index, count) in seen.iter().enumerate() {
            assert_eq!(*count, 1, "triangle {index} appears {count} times");
        }
    }
}
