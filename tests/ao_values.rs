//! What ambient occlusion must and must not do.
//!
//! The headline requirement: **a surface with nothing in front of it stays fully
//! bright.** Each vertex is measured against the 180° span its normal opens into,
//! weighted by `cos θ`, so openness normalizes to 1.0 and only real occluders pull a
//! vertex down — and a viewpoint at exactly 90° carries zero weight, so nothing grazing
//! can ever call a vertex hidden.
//!
//! The tests are exact on purpose. Where a continuum formula can be fooled by an
//! assumption (see the sealed box, where the corner normal is *not* the naive bisector),
//! the expectation is recomputed from the directions themselves, independently of any ray
//! casting. Vague tolerances are what hide the two failure modes this crate has already
//! had: counting viewpoints a surface cannot see at all, which parks an exposed flat face
//! at half brightness and leaves a bake with no white anywhere; and counting them without
//! the cosine term, which reads 0.80 where a disk overhead says 0.64.

mod common;

use common::Kind;

use aomesh::ao::{VIEW_RADIUS_FACTOR, fibonacci_sphere, vertex_visibility};
use aomesh::geometry::Geometry;

/// Viewpoints on the sphere. Roughly half fall behind each surface.
const SAMPLES: usize = 4096;

/// Ratios of identically accumulated sums are bit-exact, so this only has to absorb
/// nothing at all.
const EXACT: f64 = 1e-12;

fn solve(text: &str) -> Geometry {
    let obj = objserde::parse(text).expect("fixture parses");
    Geometry::from_obj(&obj)
}

#[test]
fn a_convex_cube_hides_nothing_and_bakes_pure_white() {
    let (text, kinds) = common::cube_with_kinds(3);
    let visibility = vertex_visibility(&solve(&text), SAMPLES);
    assert_eq!(visibility.len(), kinds.len());

    for kind in [Kind::Corner, Kind::Edge, Kind::Face] {
        let measured: Vec<f64> = kinds
            .iter()
            .zip(&visibility)
            .filter(|(observed, _)| **observed == kind)
            .map(|(_, value)| *value)
            .collect();
        assert!(
            !measured.is_empty(),
            "the fixture has no {kind:?} vertices to test"
        );
        for value in &measured {
            assert!(
                (value - 1.0).abs() < EXACT,
                "a {kind:?} vertex of a convex cube faced open sky and measured \
                 {value}; it must be exactly 1.0 — anything less is a surface darkened \
                 by nothing"
            );
        }
    }
}

#[test]
fn a_flat_square_stays_bright_across_its_whole_middle() {
    let visibility = vertex_visibility(&solve(&common::plane(8)), SAMPLES);
    assert_eq!(visibility.len(), 81);
    for (index, value) in visibility.iter().enumerate() {
        assert!(
            (*value - 1.0).abs() < EXACT,
            "vertex {index} of a lone flat plane measured {value}; the middle of a flat \
             square has nothing above it and must not darken"
        );
    }
}

#[test]
fn a_sealed_box_darkens_exactly_where_geometry_says() {
    // The same lattice cube with its faces wound inward: a sealed cavity. For a box this
    // simple the blocked set has an exact description in *directions* — a ray from a
    // point on the shell is hidden unless it leaves the box at once through a wall plane
    // that vertex sits on — so the expectation is computed over the very viewpoint
    // lattice the solver walks, with no ray casting anywhere near it.
    //
    // That replaced a hand-derived closed form which assumed the corner normal bisects
    // the three walls. It does not: objserde weights a geometric normal by incident
    // *area*, and at a welded corner those areas are unequal — it comes out
    // [0.4082, 0.4082, 0.8165], not [0.5774, …]. The oracle takes the normal the binding
    // layer actually produced, so it is exact rather than idealised, and it agrees with
    // the solver bit for bit.
    //
    // Where that lands by category: 0.00 mid-face (a cavity with nothing in it reads
    // black), ≈0.29 along an edge, ≈0.59 at a corner.
    let side = 3.0_f64;
    let (text, kinds) = common::box_with_kinds(3, true);
    let geometry = solve(&text);
    let visibility = vertex_visibility(&geometry, SAMPLES);

    for (index, kind) in kinds.iter().enumerate() {
        let expected = sealed_box_expectation(&geometry, index, side, SAMPLES);
        assert!(
            (visibility[index] - expected).abs() < EXACT,
            "a {kind:?} vertex at {:?} measured {}, but the directions alone say \
             {expected}",
            geometry.positions[index],
            visibility[index]
        );
    }

    let mean_of = |wanted: Kind| -> f64 {
        let values: Vec<f64> = kinds
            .iter()
            .zip(&visibility)
            .filter(|(observed, _)| **observed == wanted)
            .map(|(_, value)| *value)
            .collect();
        values.iter().sum::<f64>() / values.len() as f64
    };
    assert_eq!(
        mean_of(Kind::Face),
        0.0,
        "mid-face vertices of a sealed box see nothing at all"
    );
    assert!(
        (mean_of(Kind::Edge) - 0.29).abs() < 0.01,
        "edge mean {}",
        mean_of(Kind::Edge)
    );
    assert!(
        (mean_of(Kind::Corner) - 0.59).abs() < 0.01,
        "corner mean {}",
        mean_of(Kind::Corner)
    );
}

/// The expected visibility of a sealed-box vertex, computed from the directions the
/// solver would walk.
///
/// The box spans `0 .. side` on every axis. A ray from the shell is hidden unless some
/// component carries it out through a wall plane the vertex lies on, and those are the
/// only ways out, because each wall patch is confined to the box's own span.
fn sealed_box_expectation(geometry: &Geometry, index: usize, side: f64, samples: usize) -> f64 {
    let from = geometry.positions[index];
    let normal = geometry.normals[index];
    let center = geometry.centroid();
    let radius = (geometry.diagonal() * VIEW_RADIUS_FACTOR).max(1.0);

    let mut seen = 0.0f64;
    let mut total = 0.0f64;
    for direction in fibonacci_sphere(samples) {
        let towards = [
            center[0] + radius * direction[0] - from[0],
            center[1] + radius * direction[1] - from[1],
            center[2] + radius * direction[2] - from[2],
        ];
        let cos = dot(towards, normal) / dot(towards, towards).sqrt();
        if cos <= 0.0 {
            continue;
        }
        total += cos;
        let escapes = towards.iter().enumerate().any(|(axis, &component)| {
            (from[axis] == 0.0 && component < 0.0) || (from[axis] == side && component > 0.0)
        });
        if escapes {
            seen += cos;
        }
    }
    if total == 0.0 { 1.0 } else { seen / total }
}

#[test]
fn plates_darken_where_they_face_each_other_and_stay_white_outside() {
    let visibility = vertex_visibility(&solve(&common::plates()), SAMPLES);
    let (mut min, mut max) = (f64::MAX, f64::MIN);
    for &value in &visibility {
        min = min.min(value);
        max = max.max(value);
    }
    assert!(
        (max - 1.0).abs() < EXACT,
        "the outer faces of the plates face open sky and must reach 1.0; the brightest \
         vertex measured {max}"
    );
    assert!(
        min < 0.2,
        "the facing surfaces sit half a unit apart across a ten-unit span and should be \
         nearly closed off; the darkest vertex measured {min}"
    );
}

#[test]
fn a_disk_blocks_exactly_its_cosine_weighted_solid_angle() {
    // The fixture that says which integral was actually performed. A disk of radius r at
    // height h covers a cone of half-angle atan(r/h) over the floor vertex beneath its
    // centre, so that vertex should read:
    //
    //   cos-weighted:  1 − sin²α = 1 − r²/(r²+h²)   ← what ambient occlusion is
    //   unweighted:    1 − (1 − cos α) = h/√(r²+h²)
    //
    // For r = 3, h = 4 those are 0.64 and 0.80 — far enough apart that a solver which
    // quietly dropped the weighting cannot pass this.
    let (text, centre) = common::disk_over_floor(3.0, 4.0, 64, 8);
    let visibility = vertex_visibility(&solve(&text), SAMPLES);
    let measured = visibility[centre];

    let expected = 1.0 - 9.0 / 25.0;
    assert!(
        (measured - expected).abs() < 1e-2,
        "a 3-unit disk 4 units overhead hides 9/25 of the weighted hemisphere, so the \
         vertex beneath it should read {expected}, not {measured} (0.80 would mean the \
         cosine weighting was never applied)"
    );
    // Directly under the disk is the darkest point of the mesh…
    let darkest = visibility.iter().copied().fold(f64::MAX, f64::min);
    assert!(
        (measured - darkest).abs() < EXACT,
        "the vertex under the disk's centre should be the darkest one; it measured \
         {measured} against a minimum of {darkest}"
    );
    // …and the disk's own face, with nothing above it, stays pure white.
    let brightest = visibility.iter().copied().fold(f64::MIN, f64::max);
    assert!(
        (brightest - 1.0).abs() < EXACT,
        "fully exposed vertices must reach exactly 1.0; the brightest measured {brightest}"
    );
}

#[test]
fn the_stored_normal_decides_which_way_a_surface_faces() {
    // Two parallel quads, both carrying `vn 0 0 -1`. The lower one therefore faces
    // downward, away from the quad above it, and stays bright; the upper one faces
    // straight at the lower and goes dark. Ignoring the file's normals — or re-deriving
    // them from winding — would flip one of these two.
    let fixture = "\
v -5 -5 0
v 5 -5 0
v 5 5 0
v -5 5 0
v -5 -5 0.5
v 5 -5 0.5
v 5 5 0.5
v -5 5 0.5
vn 0 0 -1
f 1//1 2//1 3//1 4//1
f 5//1 8//1 7//1 6//1
";
    let visibility = vertex_visibility(&solve(fixture), SAMPLES);
    assert_eq!(visibility.len(), 8);
    for (index, value) in visibility.iter().enumerate().take(4) {
        assert!(
            (value - 1.0).abs() < EXACT,
            "the lower quad faces away from the quad above it, so vertex {index} must be \
             1.0, not {value}"
        );
    }
    // Each upper vertex sits above a corner of the lower 10 × 10 quad, so the plate
    // covers about a quarter of the hemisphere it faces — the quarter in which both dx
    // and dy point into it — putting the answer near 0.75. The point is the contrast with
    // the exact 1.0 of the row above: the same geometry, opposite normals, and only the
    // file's `vn` says which row is which.
    for (index, value) in visibility.iter().enumerate().skip(4) {
        assert!(
            (value - 0.75).abs() < 2e-2,
            "the upper quad sees the lower one across a quarter of its hemisphere, so \
             vertex {index} should land near 0.75; measured {value}"
        );
    }
}

#[test]
fn visibility_does_not_depend_on_model_scale() {
    // Authored units must not change the answer: the angle test, the weight and the hit
    // range are all ratios. Plates, not a cube — a cube is 1.0 everywhere, which any
    // broken scale would still "pass".
    let text = common::plates();
    let unit = vertex_visibility(&solve(&text), 1024);
    let mut scaled = objserde::parse(&text).expect("fixture parses");
    for vertex in &mut scaled.vertices {
        for coordinate in &mut vertex.position {
            *coordinate *= 1000.0;
        }
    }
    let measured = vertex_visibility(&Geometry::from_obj(&scaled), 1024);
    assert_eq!(measured.len(), unit.len());
    assert!(
        unit.iter().any(|value| *value < 0.5),
        "the fixture should contain occluded vertices for this to test anything"
    );
    for (expected, value) in unit.iter().zip(&measured) {
        assert!(
            (expected - value).abs() < 1e-9,
            "scaling the mesh by 1000 changed a vertex from {expected} to {value}"
        );
    }
}

#[test]
fn the_solve_is_reproducible() {
    // Vertices are solved in parallel; one vertex's result must not depend on which
    // worker computed it or in what order they finished.
    let geometry = solve(&common::plates());
    let first = vertex_visibility(&geometry, 600);
    for _ in 0..3 {
        assert_eq!(vertex_visibility(&geometry, 600), first);
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
