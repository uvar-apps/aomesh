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
//! * **The answer does not depend on how the work was arranged.** A query asks whether
//!   *any* triangle crosses the segment, so the tree may be shaped, packed, split and
//!   threaded however it likes: see `the_parallel_build_and_the_serial_one_agree`, and
//!   note that the outputs of every change below were byte-identical to the ones the
//!   original median-split walk produced.
//!
//! # Where the time goes, and what was measured not to help
//!
//! On a 37,930-vertex, 75,852-triangle mesh at `-s 256` on an M4, a bake spends about 46 ms
//! on fixed work and 205 ms in this walk. Almost all of the fixed work is objserde — 16 ms
//! to parse the 5.3 MB input and 17 ms to format the 7.8 MB output — and the filesystem
//! itself takes 1.4 ms. The `count` feature prints what a single segment reaches: 25 nodes,
//! 14 triangles, 10.4 Möller–Trumbore tests, blocked 29.9% of the time.
//!
//! **Vector instructions do not pay on this workload, and three measurements say so.**
//! Batching four triangles through one `f64x4` intersection measured 1.02–1.34x *slower*
//! than the scalar loop it replaced: a shadow ray usually hits nothing, scalar code drops a
//! miss after a couple of operations in that triangle's favour, and a lane has to finish the
//! arithmetic regardless. Vectorising only the rejection filter — the one part of that batch
//! that is branch-free — came in at parity, not a win. And the loop where SIMD plainly fits,
//! a vertex's viewpoint weights, was measured by stubbing this walk out entirely: all of that
//! arithmetic is 1.9 ms out of 207 ms of casting, so a perfect 4x on it would move the bake
//! by 0.6%.
//!
//! Shrinking the node also lost twice for different reasons. Storing boxes as outward-rounded
//! `f32` halves the bytes per visit and 32-byte nodes were measured at 1.17x over the
//! original where the same code with 64-byte nodes reached 1.33x: the widening conversions
//! and the lost per-axis early-out cost more than the fetched lines save.
//!
//! What paid, in rough order of size: the binned SAH split, eight triangles per leaf, testing
//! a node's two children at their parent instead of pushing them blind, one packed record per
//! triangle so a leaf reads sequentially instead of gathering corners from three unrelated
//! addresses, and building the hierarchy across threads.

use crate::geometry::Geometry;

/// Per-query counters, behind the `count` feature, for measuring the walk.
///
/// `cargo build --release --features count` then `RAYON_NUM_THREADS=1 target/release/aomesh
/// mesh.obj -s 64 --no-preview` prints, besides the phase times: how many segments were
/// cast, how many nodes and triangles each reaches, and the share that turn out blocked.
/// Every number quoted in this module's documentation came from here.
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
    pub fn add(slot: &AtomicU64, n: u64) {
        slot.fetch_add(n, Ordering::Relaxed);
    }
}

/// Triangles in a leaf.
///
/// Chosen by measurement, not by taste: on a 76k-triangle mesh, eight reaches the same
/// triangles through a third fewer nodes than four and runs 3–6% faster, while the two
/// small sample meshes lose about 2% because their walks are short enough that the extra
/// triangle tests in a fat leaf cost more than the descent they skip. Sixteen is worse
/// than eight on both. The heuristic's own stop test (`Builder::bin`) can still leave a
/// node with fewer than this, and does wherever splitting cannot pay for itself.
const LEAF_SIZE: usize = 8;

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

#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq)]
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
        let measured = measure(geo);
        let mut order: Vec<u32> = (0..geo.triangles.len() as u32).collect();
        let plan = Plan {
            measured: &measured,
            grain: PARALLEL_GRAIN,
        };
        let root_box = merge(&order, &plan);
        let nodes = build(&mut order, 0, 0, root_box, &plan);
        let slots = reorder(geo, &order);
        Bvh {
            nodes: pack_siblings(nodes, 0),
            slots,
            bounds: root_box,
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

/// What the build knows about one triangle: its own box, and the point that partitions it.
#[derive(Clone, Copy)]
struct Measure {
    bounds: Aabb,
    centroid: [f64; 3],
}

/// What the build shares with every subtree: the triangles' measures, and the grain below
/// which it stops handing work to the pool.
struct Plan<'a> {
    measured: &'a [Measure],
    grain: usize,
}

/// One box and centroid per triangle, in file order.
///
/// Partitioning and traversal both need these, and a triangle's measure does not depend on
/// any other triangle's, so the pass runs across the pool before the tree recursion starts.
fn measure(geo: &Geometry) -> Vec<Measure> {
    use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
    geo.triangles
        .par_iter()
        .map(|&triangle| {
            let bounds = Aabb::from_triangle(geo, triangle);
            Measure {
                bounds,
                centroid: bounds.centroid(),
            }
        })
        .collect()
}

/// Triangles below which a subtree is finished on whichever thread reached it.
///
/// Handing work to the pool is not free, and a partition of a few thousand triangles takes
/// about as long as the handover, so the tree splits across threads only while the pieces
/// are big enough to pay for it. The result is the same tree either way — see
/// [`the_parallel_build_and_the_serial_one_agree`].
const PARALLEL_GRAIN: usize = 4096;

/// Build the subtree over `order`, whose first triangle sits at absolute position `base`.
///
/// A range at least [`PARALLEL_GRAIN`] wide is divided here and its halves handed to
/// [`rayon::join`], each owning its own node vector; anything smaller is finished by
/// [`fill`] in a single one. The split is deliberate. Owning a subtree is what lets a thread
/// build one with nothing to lock — but a join also copies its children's nodes to splice
/// them in, and below a few thousand triangles the copying costs more than the work it
/// parallelises.
///
/// Each returned vector holds its subtree with the root at index `0` and child links
/// relative to that vector, so a subtree can be spliced anywhere by shifting its links.
fn build(order: &mut [u32], base: usize, depth: usize, box_: Aabb, plan: &Plan<'_>) -> Vec<Node> {
    let mut nodes = Vec::new();
    // Whether this node is a leaf is the tree's question, not the scheduler's, so the same
    // three tests gate both paths: too few triangles to split, or the depth the traversal's
    // stack is sized for, or too small a range to be worth a handover. `fill` applies the
    // first two again on the way down; a `build` that divided past `MAX_DEPTH` would grow a
    // tree deeper than the walk has room to descend, and one that divided a node the heuristic
    // would keep would not be the same hierarchy.
    if order.len() <= LEAF_SIZE || depth >= MAX_DEPTH || order.len() < plan.grain {
        fill(order, base, depth, box_, plan, &mut nodes);
        return nodes;
    }

    let Some((pivot, left_box, right_box)) = divide(order, &box_, plan) else {
        fill(order, base, depth, box_, plan, &mut nodes);
        return nodes;
    };
    // `divide` already moved the triangles, so the boxes it returns describe the two sides
    // as the traversal will find them.
    let (left_slice, right_slice) = order.split_at_mut(pivot);

    let (left_nodes, right_nodes) = rayon::join(
        || build(left_slice, base, depth + 1, left_box, plan),
        || build(right_slice, base + pivot, depth + 1, right_box, plan),
    );

    nodes.reserve(1 + left_nodes.len() + right_nodes.len());
    let right_link = 1 + left_nodes.len() as u32;
    nodes.push(Node {
        bounds: box_,
        children: [1, right_link],
        start: 0,
        count: 0,
    });
    nodes.extend(left_nodes.into_iter().map(|node| shift(node, 1)));
    nodes.extend(right_nodes.into_iter().map(|node| shift(node, right_link)));
    nodes
}

/// A node with its child links moved by `delta`. A leaf's numbers are a span of triangles,
/// not links, so they stay put.
fn shift(node: Node, delta: u32) -> Node {
    if node.count > 0 {
        node
    } else {
        Node {
            children: [node.children[0] + delta, node.children[1] + delta],
            ..node
        }
    }
}

/// Build a subtree into a shared `nodes` vector, depth-first.
///
/// A node is pushed before its children exist so it can note its own index, then patched
/// once both subtrees have been appended: the left child always lands immediately after its
/// parent, and the right after everything the left subtree occupies.
fn fill(
    order: &mut [u32],
    base: usize,
    depth: usize,
    box_: Aabb,
    plan: &Plan<'_>,
    nodes: &mut Vec<Node>,
) {
    let index = nodes.len();
    nodes.push(Node {
        bounds: box_,
        children: [0; 2],
        start: 0,
        count: 0,
    });

    if order.len() <= LEAF_SIZE || depth >= MAX_DEPTH {
        nodes[index].start = base as u32;
        nodes[index].count = order.len() as u32;
        return;
    }
    let Some((pivot, left_box, right_box)) = divide(order, &box_, plan) else {
        nodes[index].start = base as u32;
        nodes[index].count = order.len() as u32;
        return;
    };

    let left_link = nodes.len() as u32;
    let (left_slice, right_slice) = order.split_at_mut(pivot);
    fill(left_slice, base, depth + 1, left_box, plan, nodes);
    let right_link = nodes.len() as u32;
    fill(right_slice, base + pivot, depth + 1, right_box, plan, nodes);
    nodes[index].children = [left_link, right_link];
}

/// Choose the best cut and apply it, reporting the pivot and the two sides' boxes.
///
/// `None` means the triangles stay where they are: no axis separates their centroids, no
/// cut beats testing them all in one leaf, or the best cut moved nothing.
fn divide(order: &mut [u32], box_: &Aabb, plan: &Plan<'_>) -> Option<(usize, Aabb, Aabb)> {
    let axis = box_.longest_axis();
    let (low, width) = centroid_extent(order, axis, plan)?;
    let split = cheapest_cut(order, axis, low, width, box_, plan)?;
    let pivot = partition(order, split.threshold, axis, plan);
    if pivot == 0 || pivot == order.len() {
        return None;
    }
    Some((
        pivot,
        merge(&order[..pivot], plan),
        merge(&order[pivot..], plan),
    ))
}

/// Move the triangles left of `threshold` to the front and report how many there are.
fn partition(order: &mut [u32], threshold: f64, axis: usize, plan: &Plan<'_>) -> usize {
    let mut left = 0usize;
    let mut right = order.len();
    while left < right {
        let centre = plan.measured[order[left] as usize].centroid[axis];
        // A non-finite centroid cannot be ordered, and the binning put it at the start;
        // sending it the same way here keeps the two views of "left" in step.
        if centre < threshold || !centre.is_finite() {
            left += 1;
        } else {
            right -= 1;
            order.swap(left, right);
        }
    }
    left
}

/// The cut with the lowest expected cost, or `None` when no cut beats a leaf.
///
/// One pass bins the centroids along `axis` and records each bin's count and bounds; a
/// sweep over the fifteen interior boundaries costs each one as the chance a segment that
/// reached this node reaches a child, times the triangles waiting below it. Binned rather
/// than exact: an exact sweep would sort the range at every node, which costs more than the
/// tree it buys, and sixteen bins come within a few percent of the optimum in one pass.
fn cheapest_cut(
    order: &[u32],
    axis: usize,
    low: f64,
    width: f64,
    parent: &Aabb,
    plan: &Plan<'_>,
) -> Option<Split> {
    let mut counts = [0u32; BINS];
    let mut minimum = [[f64::MAX; 3]; BINS];
    let mut maximum = [[f64::MIN; 3]; BINS];

    for &triangle in order {
        let centre = plan.measured[triangle as usize].centroid[axis];
        let bin = (((centre - low) / width) as usize).min(BINS - 1);
        counts[bin] += 1;
        let source = &plan.measured[triangle as usize].bounds;
        for coordinate in 0..3 {
            minimum[bin][coordinate] = minimum[bin][coordinate].min(source.min[coordinate]);
            maximum[bin][coordinate] = maximum[bin][coordinate].max(source.max[coordinate]);
        }
    }

    // Right-hand totals, so trying each boundary in turn keeps only the left running.
    let mut right_counts = [0u32; BINS + 1];
    let mut right_min = [[f64::MAX; 3]; BINS + 1];
    let mut right_max = [[f64::MIN; 3]; BINS + 1];
    for bin in (0..BINS).rev() {
        right_counts[bin] = right_counts[bin + 1] + counts[bin];
        for coordinate in 0..3 {
            let (bin_min, bin_max) = if counts[bin] > 0 {
                (minimum[bin][coordinate], maximum[bin][coordinate])
            } else {
                (f64::MAX, f64::MIN)
            };
            right_min[bin][coordinate] = right_min[bin + 1][coordinate].min(bin_min);
            right_max[bin][coordinate] = right_max[bin + 1][coordinate].max(bin_max);
        }
    }

    let mut left_counts = 0u32;
    let mut best = f64::INFINITY;
    let mut best_bin = None;
    let mut left_min = [f64::MAX; 3];
    let mut left_max = [f64::MIN; 3];
    for cut in 0..BINS - 1 {
        if counts[cut] == 0 {
            continue;
        }
        left_counts += counts[cut];
        for coordinate in 0..3 {
            left_min[coordinate] = left_min[coordinate].min(minimum[cut][coordinate]);
            left_max[coordinate] = left_max[coordinate].max(maximum[cut][coordinate]);
        }
        if right_counts[cut + 1] == 0 {
            continue;
        }
        let cost = area(left_min, left_max) * left_counts as f64
            + area(right_min[cut + 1], right_max[cut + 1]) * right_counts[cut + 1] as f64;
        if cost < best {
            best = cost;
            best_bin = Some(cut);
        }
    }

    let cut = best_bin?;
    // A leaf costs the parent's area times every triangle in it — a segment that arrives
    // tests all of them. A split has to beat that or its extra box tests cost more than the
    // triangle tests they avoid.
    if best >= area(parent.min, parent.max) * order.len() as f64 {
        return None;
    }
    Some(Split {
        threshold: low + (cut as f64 + 1.0) * width,
    })
}

/// Where the best cut falls: a value on the split axis, because the partition compares
/// centroids against it rather than trusting a bin number assigned earlier.
struct Split {
    threshold: f64,
}

/// `low` and the bin width spanning this range's centroids on `axis`, or `None` if they
/// all coincide and no partition could separate them.
fn centroid_extent(order: &[u32], axis: usize, plan: &Plan<'_>) -> Option<(f64, f64)> {
    let mut low = f64::MAX;
    let mut high = f64::MIN;
    for &triangle in order {
        let centre = plan.measured[triangle as usize].centroid[axis];
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

/// Surface area — the SA in the heuristic — clamped at zero so a degenerate slab costs
/// nothing rather than going negative through rounding.
fn area(min: [f64; 3], max: [f64; 3]) -> f64 {
    let span = [
        (max[0] - min[0]).max(0.0),
        (max[1] - min[1]).max(0.0),
        (max[2] - min[2]).max(0.0),
    ];
    2.0 * (span[0] * span[1] + span[1] * span[2] + span[2] * span[0])
}

/// The union of the boxes of the triangles in `order`.
fn merge(order: &[u32], plan: &Plan<'_>) -> Aabb {
    let mut box_ = Aabb::EMPTY;
    for &triangle in order {
        box_ = box_.extend(plan.measured[triangle as usize].bounds);
    }
    box_
}

/// Copy the triangles into hierarchy order as [`Slot`] records.
///
/// Done once, after the tree is finished: the partition moves indices, which is cheap, and
/// this pass turns them into bytes, which is not — so each record is written exactly once,
/// in the order the traversal will read it.
fn reorder(geo: &Geometry, order: &[u32]) -> Vec<Slot> {
    order
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
                for slot in
                    bvh.slots[node.start as usize..(node.start + node.count) as usize].iter()
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
                            t[0] == slot.verts[0] && t[1] == slot.verts[1] && t[2] == slot.verts[2]
                        })
                        .expect("slot is not a mesh triangle");
                    seen[found] += 1;
                }
            } else {
                stack.push((node.children[0] as usize, node.bounds, false));
                stack.push((node.children[1] as usize, node.bounds, false));
            }
        }
        assert_eq!(
            total,
            geo.triangles.len(),
            "each triangle in exactly one leaf"
        );
        for (index, count) in seen.iter().enumerate() {
            assert_eq!(*count, 1, "triangle {index} appears {count} times");
        }
    }
}

#[cfg(test)]
mod build_equivalence {
    use super::*;

    /// A closed `side × side` grid of quads, wrinkled so the heuristic has real work to do.
    fn grid(side: usize) -> Geometry {
        let mut positions = Vec::with_capacity((side + 1) * (side + 1));
        for y in 0..=side {
            for x in 0..=side {
                positions.push([x as f64, y as f64, ((x * y) % 7) as f64 * 0.25]);
            }
        }
        let row = side + 1;
        let mut triangles = Vec::with_capacity(2 * side * side);
        for y in 0..side {
            for x in 0..side {
                let a = (y * row + x) as u32;
                triangles.push([a, a + 1, a + row as u32]);
                triangles.push([a + 1, a + row as u32 + 1, a + row as u32]);
            }
        }
        let vertices = positions.len();
        Geometry {
            positions,
            normals: vec![[0.0, 0.0, 1.0]; vertices],
            triangles,
        }
    }

    /// One build with the grain pinned, so the two code paths can be compared directly.
    fn with_grain(geo: &Geometry, grain: usize) -> Bvh {
        let measured = measure(geo);
        let mut order: Vec<u32> = (0..geo.triangles.len() as u32).collect();
        let plan = Plan {
            measured: &measured,
            grain,
        };
        let root_box = merge(&order, &plan);
        let nodes = build(&mut order, 0, 0, root_box, &plan);
        Bvh {
            nodes: pack_siblings(nodes, 0),
            slots: reorder(geo, &order),
            bounds: root_box,
        }
    }

    /// The grain and the depth cap are scheduling decisions, not part of the answer: a bake
    /// must not depend on how many cores were free while its hierarchy was being built. So
    /// building every subtree in one shared vector (`fill`) and joining at every level
    /// (`build`) have to yield the same nodes, the same spans, and the same hierarchy.
    #[test]
    fn the_parallel_build_and_the_serial_one_agree() {
        let geo = grid(96);
        assert!(
            geo.triangles.len() > 4 * PARALLEL_GRAIN,
            "the fixture has to be big enough to divide both ways"
        );
        let shared = with_grain(&geo, usize::MAX);
        let divided = with_grain(&geo, 1);
        let shipped = Bvh::build(&geo);

        assert_eq!(shared.nodes.len(), divided.nodes.len(), "same node count");
        for (index, (a, b)) in shared.nodes.iter().zip(divided.nodes.iter()).enumerate() {
            assert_eq!(a, b, "node {index} differs between the two builds");
        }
        assert_eq!(
            shared.slots, divided.slots,
            "same triangles in the same order"
        );
        assert_eq!(shipped.nodes, shared.nodes, "and the shipped grain agrees");
    }
}
