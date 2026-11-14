//! The values themselves, with nothing written to disk.
//!
//! Run it with `cargo run -p aomesh --example visibility_values`.
//!
//! [`bake`] is the whole product, but it is not the only thing worth having: per-vertex
//! occlusion is a number, and a caller can use it directly — to tint a preview, to pick
//! a LOD, to prove a support really is buried. That path is [`objserde::parse`] →
//! [`Geometry::from_obj`] → [`vertex_visibility`], and it never touches a filesystem.
//!
//! The scene is a lone floor, then the same floor with two walls standing near two of
//! its edges. The floor's vertices do not move between the two runs; only what is in
//! front of them does.

use std::error::Error;

use aomesh::ao::{report, vertex_visibility};
use aomesh::geometry::Geometry;
use aomesh::texture::level;

/// One quad, `8 × 8`, in the plane `z = 0`.
const FLOOR: &str = "v 0 0 0\nv 8 0 0\nv 8 8 0\nv 0 8 0\nf 1 2 3 4\n";

/// A wall four units tall standing two units off the floor's `x = 0` edge, facing `+x`.
const WALL_ALONG_X: &str = "v -2 0 0\nv -2 8 0\nv -2 8 4\nv -2 0 4\nf 5 6 7 8\n";

/// The same two units off the `y = 0` edge, facing `+y`. Vertex numbers continue from
/// the floor and the first wall, which is why these assemble in this order.
const WALL_ALONG_Y: &str = "v 0 -2 0\nv 8 -2 0\nv 8 -2 4\nv 0 -2 4\nf 9 12 11 10\n";

/// The four floor vertices, named by where they stand relative to the two walls.
const WHERE: [&str; 4] = [
    "beside both walls",
    "beside one, 10 from the other",
    "furthest from both",
    "beside one, 10 from the other",
];

fn main() -> Result<(), Box<dyn Error>> {
    // 1024 viewpoints per vertex, where a bake's default is 256: this is a print, not a
    // bake, and it costs about a tenth of a second either way.
    let samples = 1024;

    let open = Geometry::from_obj(&objserde::parse(FLOOR)?);
    let walled = Geometry::from_obj(&objserde::parse(&format!(
        "{FLOOR}{WALL_ALONG_X}{WALL_ALONG_Y}"
    ))?);

    let alone = vertex_visibility(&open, samples);
    let standing = vertex_visibility(&walled, samples);

    println!(
        "{:<32} {:>8} {:>8} {:>7}",
        "floor vertex", "alone", "walled", "level"
    );
    for index in 0..4 {
        println!(
            "{:<32} {:>8.4} {:>8.4} {:>7}",
            WHERE[index],
            alone[index],
            standing[index],
            level(standing[index]),
        );
    }

    let measured = report(&standing);
    println!(
        "\nwhole mesh  min {:.3}  mean {:.3}  max {:.3}",
        measured.min, measured.mean, measured.max
    );

    // The ordering is the point: occlusion tracks what stands in front of each surface,
    // and it falls off with distance. The walls stand *off* the floor's edges rather than
    // along them for a reason worth knowing — a wall lying in a plane that passes through
    // a vertex hides nothing from that vertex, only from the vertices along its span. So
    // the corner nearest both is darkest and the far corner stays near white, and neither
    // of them is exactly 1.0 once geometry is in front of it.
    assert!(
        alone.iter().all(|&value| value == 1.0),
        "a lone plane hides nothing from itself, so every vertex is white — got {alone:?}"
    );
    assert!(
        standing[0] < standing[1] && standing[1] < standing[2],
        "the corner beside both walls should be darkest and the far one brightest, in \
         between them a wall seen edge-on: got {:?}",
        &standing[..4],
    );
    assert!(
        standing[2] > 0.95,
        "two walls at ten units' distance barely graze the far corner: {:?}",
        standing[2],
    );
    Ok(())
}
