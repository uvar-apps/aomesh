//! Mesh fixtures.
//!
//! Three of them, and between them they bracket the whole behaviour:
//!
//! * **A convex cube, wound outward** — every surface faces open sky. Nothing it
//!   can see is blocked, so every vertex must read **1.0**, white, including the
//!   corners and the middle of a face. This is the fixture that catches an AO solver
//!   that darkens the un-hidden: any normalization that counts viewpoints a surface
//!   cannot address puts a flat face at half brightness, which is wrong.
//! * **The same cube wound inward** — a sealed box. Every vertex's hemisphere now
//!   opens into a cavity whose walls close over it completely, so every vertex must
//!   read **0.0**, black. Only rays that genuinely stop at a wall produce that, so
//!   it fails if occlusion is being skipped.
//! * **Two plates with a narrow gap** — the in-between case: the outer surfaces must
//!   still read 1.0 while the two facing surfaces go dark, in one mesh at once.
//!
//! Vertex categories (corner / edge / face interior) are reported by the cube
//! fixtures so a test can state that all three are present rather than silently
//! measuring one kind.

// Shared helpers: each test binary compiles this file on its own and uses a subset
// of it, which would otherwise read as dead code.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where a cube vertex sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Three faces meet.
    Corner,
    /// Two faces meet.
    Edge,
    /// A single face.
    Face,
}

/// A closed cube on an `(n+1)³` lattice boundary with the category of every vertex in
/// `v`-line order.
///
/// Wound outward it is convex and fully exposed — every vertex must read 1.0. Wound
/// inward it is a sealed cavity, and each category then has its own closed form (see
/// [`crate`]'s module documentation and `ao_values.rs`): the walls are zero-thickness,
/// so a hemisphere aimed into the box still leaks past the very walls its vertex sits
/// on — 0.75 at a corner, 0.50 along an edge, 0.00 in the middle of a face.
#[must_use]
pub fn box_with_kinds(n: usize, inward: bool) -> (String, Vec<Kind>) {
    assert!(n >= 2, "n = 1 has no face-interior or edge vertices");
    let mut text = if inward {
        String::from("# lattice-boundary cube, wound inward: a sealed cavity\n")
    } else {
        String::from("# lattice-boundary cube, wound outward\n")
    };
    let mut kinds = Vec::new();
    let side = n as f64;
    append_box(
        &mut text,
        [0.0, 0.0, 0.0],
        [side, side, side],
        n,
        inward,
        Some(&mut kinds),
    );
    (text, kinds)
}

/// A convex cube, wound outward, with categories. Every vertex must read 1.0.
#[must_use]
pub fn cube_with_kinds(n: usize) -> (String, Vec<Kind>) {
    box_with_kinds(n, false)
}

/// The same cube wound inward: a sealed box.
#[must_use]
pub fn room(n: usize) -> String {
    box_with_kinds(n, true).0
}

/// Two `10 × 10 × 0.5` slabs separated by a `0.5` gap. Outer faces stay bright, the
/// two faces turned towards each other go dark.
#[must_use]
pub fn plates() -> String {
    let mut text = String::from("# two slabs, a narrow gap between them\n");
    append_box(
        &mut text,
        [0.0, 0.0, 0.0],
        [10.0, 10.0, 0.5],
        6,
        false,
        None,
    );
    append_box(
        &mut text,
        [0.0, 0.0, 1.0],
        [10.0, 10.0, 0.5],
        6,
        false,
        None,
    );
    text
}

/// A single flat `n × n` grid of quads in the plane `z = 0`.
///
/// Nothing can occlude it: a ray that leaves the plane never returns to it, and a ray
/// lying in it is parallel to every triangle there. Every vertex should read 1.0 —
/// and this is the flat-square case that must *not* darken in the middle.
#[must_use]
pub fn plane(n: usize) -> String {
    assert!(n >= 1);
    let mut text = String::from("# flat grid plane\n");
    let one = |x: usize, y: usize| -> u32 { (y * (n + 1) + x + 1) as u32 };
    for y in 0..=n {
        for x in 0..=n {
            writeln!(text, "v {x} {y} 0").expect("writing to a String");
        }
    }
    for y in 0..n {
        for x in 0..n {
            writeln!(
                text,
                "f {} {} {} {}",
                one(x, y),
                one(x + 1, y),
                one(x + 1, y + 1),
                one(x, y + 1)
            )
            .expect("writing to a String");
        }
    }
    text
}

/// Append a welded box spanning `origin .. origin + extents`, subdivided `n` times
/// per edge, faces outward or inward.
///
/// Vertices weld at the seams, so a corner belongs to three faces and inherits the
/// bisecting normal — the case a naive solver most often gets wrong.
fn append_box(
    text: &mut String,
    origin: [f64; 3],
    extents: [f64; 3],
    n: usize,
    inward: bool,
    mut kinds: Option<&mut Vec<Kind>>,
) {
    let edge = n as i64;
    let base = vertex_count(text);
    let mut index: HashMap<[i64; 3], u32> = HashMap::new();

    for k in 0..=edge {
        for m in 0..=edge {
            for p in 0..=edge {
                let extremes = [k, m, p].iter().filter(|&&c| c == 0 || c == edge).count();
                if extremes == 0 {
                    continue;
                }
                index.insert([k, m, p], (base + index.len()) as u32);
                if let Some(kinds) = &mut kinds {
                    kinds.push(match extremes {
                        3 => Kind::Corner,
                        2 => Kind::Edge,
                        _ => Kind::Face,
                    });
                }
                let coordinate = |axis: usize, lattice: i64| {
                    origin[axis] + extents[axis] * lattice as f64 / edge as f64
                };
                writeln!(
                    text,
                    "v {} {} {}",
                    coordinate(0, k),
                    coordinate(1, m),
                    coordinate(2, p)
                )
                .expect("writing to a String");
            }
        }
    }

    let mut quad = |a: [i64; 3], b: [i64; 3], c: [i64; 3], d: [i64; 3]| {
        let one = |p: [i64; 3]| index[&p] + 1;
        // Reversing the corner order reverses the geometric normal, which is how the
        // sealed-cavity fixture turns a convex cube inside out.
        let face = if inward {
            format!("f {} {} {} {}", one(d), one(c), one(b), one(a))
        } else {
            format!("f {} {} {} {}", one(a), one(b), one(c), one(d))
        };
        text.push_str(&face);
        text.push('\n');
    };

    for u in 0..edge {
        for v in 0..edge {
            quad([u, v + 1, 0], [u + 1, v + 1, 0], [u + 1, v, 0], [u, v, 0]); // −z
            quad(
                [u, v, edge],
                [u + 1, v, edge],
                [u + 1, v + 1, edge],
                [u, v + 1, edge],
            ); // +z
            quad([u, 0, v], [u + 1, 0, v], [u + 1, 0, v + 1], [u, 0, v + 1]); // −y
            quad(
                [u, edge, v],
                [u, edge, v + 1],
                [u + 1, edge, v + 1],
                [u + 1, edge, v],
            ); // +y
            quad([0, u, v], [0, u, v + 1], [0, u + 1, v + 1], [0, u + 1, v]); // −x
            quad(
                [edge, u, v],
                [edge, u + 1, v],
                [edge, u + 1, v + 1],
                [edge, u, v + 1],
            ); // +x
        }
    }
}

fn vertex_count(text: &str) -> usize {
    text.lines().filter(|line| line.starts_with("v ")).count()
}

/// A floor grid in `z = 0` centred on the origin, plus a regular `sides`-gon disk of
/// radius `r` at height `h` centred directly above it. Returns the mesh and the index
/// of the floor vertex at the origin.
///
/// That vertex is hidden by the disk over a cone of half-angle `atan(r/h)`, and by
/// nothing else, so its cosine-weighted visibility is exactly `1 − r²/(r²+h²)` —
/// whereas unweighted coverage would give `h/√(r²+h²)`. The two differ enough that the
/// fixture says which of them the solver performed. The polygon stands in for a circle,
/// which costs about `1e-2` for the sizes used here.
#[must_use]
pub fn disk_over_floor(r: f64, h: f64, sides: usize, floor_half: usize) -> (String, usize) {
    assert!(
        floor_half % 2 == 0,
        "the floor needs a vertex at the origin"
    );
    let mut text = String::from("# floor grid with a disk overhead\n");
    let span = 2 * floor_half + 1;
    for y in 0..span {
        for x in 0..span {
            let px = x as f64 - floor_half as f64;
            let py = y as f64 - floor_half as f64;
            writeln!(text, "v {px} {py} 0").expect("writing to a String");
        }
    }
    let one = |x: usize, y: usize| -> u32 { (y * span + x + 1) as u32 };
    for y in 0..span - 1 {
        for x in 0..span - 1 {
            writeln!(
                text,
                "f {} {} {} {}",
                one(x, y),
                one(x + 1, y),
                one(x + 1, y + 1),
                one(x, y + 1)
            )
            .expect("writing to a String");
        }
    }

    // One polygonal face, corners in order: fan-triangulation handles it, which also
    // exercises the n-gon path rather than only triangles and quads.
    let ring_start = span * span;
    let mut face = String::from("f");
    for k in 0..sides {
        let angle = 2.0 * std::f64::consts::PI * k as f64 / sides as f64;
        let px = r * angle.cos();
        let py = r * angle.sin();
        writeln!(text, "v {px} {py} {h}").expect("writing to a String");
        write!(face, " {}", ring_start + k + 1).expect("writing to a String");
    }
    face.push('\n');
    text.push_str(&face);

    let centre = floor_half * span + floor_half;
    (text, centre)
}

/// Write `text` into `directory` as `name` and return its path.
pub fn write_mesh(directory: &Path, name: &str, text: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, text).expect("fixture writes");
    path
}
