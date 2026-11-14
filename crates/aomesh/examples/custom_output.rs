//! Where a bake's files go, what its material is called, and how to read both back.
//!
//! Run it with `cargo run -p aomesh --example custom_output`.
//!
//! Two knobs matter to a caller: the destination, and the `newmtl` name. Both are
//! ordinary fields of [`BakeOptions`] — this is the same single call as the defaults,
//! with the outputs sent somewhere deliberate. The read-back at the end is how a
//! renderer will see the result, so it is worth stating out loud what a bake leaves
//! behind.

use std::error::Error;
use std::fs;

use aomesh::{BakeOptions, bake};

/// A floor fanned around a centre vertex, with a plate held half a unit above it.
///
/// The centre vertex matters twice over. It is the one directly under the plate, so it
/// is where the bake goes dark — occlusion is stored per vertex, and a floor made of one
/// quad would keep its values at four corners that the plate never covers, showing
/// almost nothing. Subdividing a surface is what lets a bake say anything about it.
const SCENE: &str = "\
v 0 0 0
v 4 0 0
v 4 4 0
v 0 4 0
v 2 2 0
v 0.5 0.5 0.5
v 3.5 0.5 0.5
v 3.5 3.5 0.5
v 0.5 3.5 0.5
f 1 2 5
f 2 3 5
f 3 4 5
f 4 1 5
f 6 7 8 9
";

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::temp_dir().join("aomesh-example-output");
    fs::create_dir_all(&directory)?;
    let input = directory.join("scene.obj");
    fs::write(&input, SCENE)?;
    let before = fs::read(&input)?;

    // The MTL and PNG follow the destination's stem, in the destination's directory:
    // an OBJ reaches its library, and a material reaches its texture, by a path
    // relative to the file doing the referring, so the three have to travel together.
    // `renders/` does not exist yet; a bake creates it.
    let destination = directory.join("renders/settled.obj");
    let report = bake(
        &input,
        Some(&destination),
        &BakeOptions {
            material_name: "settled_ao".to_string(),
            ..BakeOptions::default()
        },
    )?;

    assert_eq!(report.object, destination);
    assert_eq!(report.material, directory.join("renders/settled.mtl"));
    assert_eq!(report.texture, directory.join("renders/settled.png"));

    let baked = objserde::parse_file(&report.object)?;
    assert_eq!(
        baked.material_libs.len(),
        1,
        "one library, the one this bake wrote"
    );
    assert_eq!(baked.material_libs[0].path, "settled.mtl");
    let material = &baked.materials[0];
    assert_eq!(material.name, "settled_ao");
    let map = material
        .diffuse_map()
        .expect("the AO map is the diffuse map");
    assert_eq!(map.path, "settled.png");
    assert_eq!(
        baked.tex_coords.len(),
        baked.vertices.len(),
        "one texture coordinate per vertex, which is what a per-vertex bake means"
    );
    assert_eq!(
        fs::read(&input)?,
        before,
        "the input mesh is left exactly as it was found"
    );
    // The two surfaces in the scene, at the two ends of the range: the plate's top
    // faces nothing but sky, the floor's corners have the plate over them.
    assert_eq!(report.visibility.max, 1.0, "the plate's top faces open sky");
    assert!(
        report.visibility.min < 0.5,
        "the floor vertex under the plate should be markedly dark: {:?}",
        report.visibility,
    );

    println!("baked      {}", report.object.display());
    println!("material   {}", material.name);
    println!("texture    {}", map.path);
    println!(
        "written    {} coordinates for {} vertices across {} faces",
        baked.tex_coords.len(),
        baked.vertices.len(),
        baked.faces.len(),
    );
    println!(
        "visibility min {:.3}  mean {:.3}  max {:.3}",
        report.visibility.min, report.visibility.mean, report.visibility.max
    );
    println!(
        "\nOutputs left in {} for inspection; delete it when done.",
        directory.display()
    );
    Ok(())
}
