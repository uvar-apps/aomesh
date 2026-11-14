//! One call, three files: the shape of every use of this crate.
//!
//! Run it with `cargo run -p aomesh --example bake_a_mesh`, or point it at a mesh of
//! your own with `cargo run -p aomesh --example bake_a_mesh -- ~/meshes/seat.obj`.
//!
//! The built-in scene is assembled here rather than loaded from disk, so the example
//! needs nothing; the bake itself is what a caller with a real file would write —
//! build [`BakeOptions`], call [`bake`], read the [`BakeReport`].

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use aomesh::{BakeOptions, bake};

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::temp_dir().join("aomesh-example-bake");
    fs::create_dir_all(&directory)?;

    // A caller with a mesh on disk names that file and skips the write below. The
    // input is never modified — outputs are new files, named after it.
    let supplied = std::env::args().nth(1);
    let built_in = supplied.is_none();
    let input = match supplied {
        Some(argument) => PathBuf::from(argument),
        None => {
            let scene = directory.join("pedestal.obj");
            fs::write(&scene, pedestal())?;
            scene
        }
    };

    let report = bake(&input, None, &BakeOptions::default())?;

    println!(
        "{}: {} vertices, {} triangles, {} viewpoints",
        input.display(),
        report.vertices,
        report.triangles,
        report.samples,
    );
    println!(
        "visibility  min {:.3}  mean {:.3}  max {:.3}",
        report.visibility.min, report.visibility.mean, report.visibility.max
    );
    println!("wrote {}", report.object.display());
    println!("wrote {}", report.material.display());
    println!("wrote {}", report.texture.display());

    // Both ends of the range are present in this scene, and both are physically right:
    // floor directly under the pedestal is sealed against it — nothing above it but box,
    // so its weighted sum never accumulates — while the floor's far corners face open
    // sky. Reading min and max is worth doing for any mesh, because a bake with no white
    // anywhere is the classic sign of geometry occluding itself where it should not.
    if built_in {
        assert!(
            report.visibility.min < 1e-9,
            "the floor under the pedestal is sealed, so it should read black"
        );
        assert!(
            report.visibility.max > 0.98,
            "the floor's far corners face open sky and should read white"
        );
        println!(
            "\nOutputs left in {} for inspection; delete it when done.",
            directory.display()
        );
    }
    Ok(())
}

/// A `9 × 9` floor grid at `z = 0` with a box standing on it.
///
/// The floor is subdivided because occlusion is a per-vertex quantity: a single quad
/// would store a value at four far corners and nowhere the darkening matters.
fn pedestal() -> String {
    const N: usize = 9;
    let mut obj = String::from("# floor grid with a pedestal on it\n");
    let floor = |x: usize, y: usize| -> u32 { (y * (N + 1) + x + 1) as u32 };
    for y in 0..=N {
        for x in 0..=N {
            writeln!(obj, "v {x} {y} 0").expect("writing to a String");
        }
    }
    for y in 0..N {
        for x in 0..N {
            writeln!(
                obj,
                "f {} {} {} {}",
                floor(x, y),
                floor(x + 1, y),
                floor(x + 1, y + 1),
                floor(x, y + 1)
            )
            .expect("writing to a String");
        }
    }

    let base = (N + 1) * (N + 1);
    let corner = |index: usize| -> u32 { (base + index) as u32 };
    for [x, y, z] in [
        [3.0, 3.0, 0.0],
        [6.0, 3.0, 0.0],
        [6.0, 6.0, 0.0],
        [3.0, 6.0, 0.0],
        [3.0, 3.0, 3.0],
        [6.0, 3.0, 3.0],
        [6.0, 6.0, 3.0],
        [3.0, 6.0, 3.0],
    ] {
        writeln!(obj, "v {x} {y} {z}").expect("writing to a String");
    }
    // Wound outward, as a solid should be: its own faces stay bright, and the floor in
    // its shadow is what goes dark.
    for face in [
        [1, 4, 3, 2],
        [5, 6, 7, 8],
        [1, 2, 6, 5],
        [3, 4, 8, 7],
        [1, 5, 8, 4],
        [2, 3, 7, 6],
    ] {
        writeln!(
            obj,
            "f {} {} {} {}",
            corner(face[0]),
            corner(face[1]),
            corner(face[2]),
            corner(face[3])
        )
        .expect("writing to a String");
    }
    obj
}
