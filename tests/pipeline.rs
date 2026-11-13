//! The bake as a whole: files out, and what comes back when they are read again.
//!
//! Everything here reads the outputs with objserde — the same reader the tool used
//! to write them — and, for the parts a renderer cares about, with meshview. That
//! order matters: a bake that agreed with itself in memory but wrote an OBJ that
//! resolves to something else would still produce a nice picture, and it would be
//! the wrong picture.

mod common;

use std::path::Path;

use aomesh::{BakeError, BakeOptions, bake, preview, texture};

/// Cube with a 2×2 quad grid per face: 26 vertices, 24 quads, 48 triangles.
fn cube_fixture(directory: &Path) -> std::path::PathBuf {
    let (text, _) = common::cube_with_kinds(2);
    common::write_mesh(directory, "cube.obj", &text)
}

fn options(samples: usize) -> BakeOptions {
    BakeOptions {
        samples,
        ..BakeOptions::default()
    }
}

#[test]
fn a_bake_writes_obj_mtl_and_texture_beside_the_input() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let before = std::fs::read(&input).expect("fixture reads");

    let report = bake(&input, None, &options(64)).expect("bake");

    assert_eq!(report.object, directory.path().join("cube_ao.obj"));
    assert_eq!(report.material, directory.path().join("cube_ao.mtl"));
    assert_eq!(report.texture, directory.path().join("cube_ao.png"));
    for written in [&report.object, &report.material, &report.texture] {
        assert!(written.exists(), "{} was not written", written.display());
    }
    assert_eq!(report.vertices, 26);
    // 24 quads — a 2×2 grid on each of the six faces — fan-triangulated.
    assert_eq!(report.triangles, 48);
    assert_eq!(
        std::fs::read(&input).expect("fixture reads"),
        before,
        "the mesh that was baked must not be modified"
    );
}

#[test]
fn an_explicit_destination_renames_the_whole_set() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let destination = directory.path().join("nested/other.obj");

    let report = bake(&input, Some(&destination), &options(16)).expect("bake");

    assert_eq!(report.object, destination);
    assert_eq!(report.material, directory.path().join("nested/other.mtl"));
    assert_eq!(report.texture, directory.path().join("nested/other.png"));
    assert!(report.material.exists(), "the directory is created");
    // References inside the files stay relative to the OBJ.
    let mtl = std::fs::read_to_string(&report.material).expect("mtl reads");
    assert!(
        mtl.contains("map_Kd -imfchan l other.png"),
        "the library must point at its own texture by name:\n{mtl}"
    );
}

#[test]
fn the_baked_obj_stores_one_coordinate_per_vertex() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(64)).expect("bake");

    let baked = objserde::parse_file(&report.object).expect("baked obj parses");
    assert_eq!(baked.vertices.len(), 26);
    assert_eq!(
        baked.tex_coords.len(),
        baked.vertices.len(),
        "one vt per vertex, so a corner can address its own vertex's occlusion"
    );
    for coordinate in &baked.tex_coords {
        assert_eq!(coordinate.arity, 2, "samplers key off an (s, t) pair");
        assert_eq!(
            coordinate.components[0], coordinate.components[1],
            "the second component repeats the first instead of inventing a value"
        );
        assert!(
            (0.0..1.0).contains(&coordinate.components[0]),
            "coordinate left the texture: {}",
            coordinate.components[0]
        );
    }
    for face in &baked.faces {
        assert_eq!(face.material, Some(0), "every face uses the AO material");
        for corner in &face.corners {
            assert_eq!(
                corner.vt,
                Some(corner.v),
                "a corner must address its own vertex's coordinate"
            );
        }
    }
    assert_eq!(baked.material_libs.len(), 1);
    assert_eq!(baked.material_libs[0].path, "cube_ao.mtl");
    let material = &baked.materials[0];
    assert_eq!(material.name, aomesh::DEFAULT_MATERIAL_NAME);
    assert_eq!(material.diffuse, Some([1.0, 1.0, 1.0]), "Kd must not tint");
    let map = material.diffuse_map().expect("a diffuse map is declared");
    assert_eq!(map.path, "cube_ao.png");
}

#[test]
fn baking_adds_texture_coordinates_without_losing_normals() {
    let directory = tempfile::tempdir().expect("temp dir");
    // One triangle, flat-shaded: corners in `v//vn` form, which the bake must keep.
    let input = common::write_mesh(
        directory.path(),
        "triangle.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n",
    );

    let report = bake(&input, None, &options(32)).expect("bake");
    let text = std::fs::read_to_string(&report.object).expect("obj reads");
    let baked = objserde::parse_file(&report.object).expect("baked obj parses");

    assert_eq!(baked.normals, vec![[0.0, 0.0, 1.0]]);
    for corner in &baked.faces[0].corners {
        assert_eq!(corner.vn, Some(0), "the input's normal binding survived");
        assert_eq!(corner.vt, Some(corner.v), "and a coordinate was added");
    }
    // Written as `v/vt/vn`, the three-field form.
    assert!(
        text.contains("f 1/1/1 2/2/1 3/3/1"),
        "corners should carry position, texture and normal:\n{text}"
    );
    // Nothing else exists to occlude a lone triangle.
    assert_eq!(report.visibility.min, 1.0);
    assert_eq!(report.visibility.max, 1.0);
}

#[test]
fn the_texture_is_a_256_wide_ramp() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(16)).expect("bake");

    let image = image::open(&report.texture)
        .expect("texture decodes")
        .to_luma8();
    assert_eq!((image.width(), image.height()), (256, 1), "a 1D texture");
    for x in 0..256u32 {
        assert_eq!(image.get_pixel(x, 0)[0], x as u8, "texel {x}");
    }
}

#[test]
fn a_written_coordinate_samples_at_its_visibility() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(16)).expect("bake");
    let ramp = meshview::Texture::load(&report.texture.to_string_lossy()).expect("decodes");

    for visibility in [0.0, 0.125, 0.5, 0.875, 1.0] {
        let coordinate = texture::coordinate(visibility);
        let [red, green, blue] = ramp.sample(coordinate, coordinate);
        let expected = 255.0 * visibility;
        assert!(
            (red - expected).abs() < 1.0,
            "visibility {visibility} sampled {red}, expected {expected}"
        );
        assert!(
            green == red && blue == red,
            "the ramp is scalar, so every channel should agree: {red} {green} {blue}"
        );
    }
}

#[test]
fn the_raw_ratio_would_alias_so_it_is_not_used() {
    // Why aomesh::texture::coordinate is not simply `u = visibility`: under repeat
    // wrapping a 256-texel texture is addressed at texel centres, which makes u = 0
    // and u = 1 the same point — the seam between texel 255 and texel 0. Handing the
    // sampler the raw ratio therefore renders a fully occluded vertex and a fully
    // open one identically, at mid gray.
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(16)).expect("bake");
    let ramp = meshview::Texture::load(&report.texture.to_string_lossy()).expect("decodes");

    let at_zero = ramp.sample(0.0, 0.0)[0];
    let at_one = ramp.sample(1.0, 1.0)[0];
    assert!(
        (at_zero - 127.5).abs() < 1.0 && (at_one - 127.5).abs() < 1.0,
        "the raw ratio should alias both endpoints to mid gray, got {at_zero} and {at_one}"
    );
    // And the mapping the tool actually writes separates them.
    assert!(ramp.sample(texture::coordinate(0.0), 0.0)[0] < 1.0);
    assert!(ramp.sample(texture::coordinate(1.0), 0.0)[0] > 254.0);
}

#[test]
fn a_renderer_resolves_the_texture_onto_every_triangle() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(64)).expect("bake");

    let textured = meshview::load_textured(&report.object.to_string_lossy())
        .expect("the generated obj, mtl and png load together");
    assert_eq!(textured.textures.len(), 1, "the ramp is decoded once");
    assert_eq!(
        textured.face_count(),
        report.triangles,
        "the bake and a reader of the written file must count the same triangles"
    );
    for triangle in &textured.tri_tex {
        assert!(triangle.shaded_by_material, "a material is in effect");
        assert_eq!(triangle.texture, Some(0), "and it resolves to the ramp");
    }
}

#[test]
fn the_validation_image_shows_both_meshes() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = cube_fixture(directory.path());
    let report = bake(&input, None, &options(64)).expect("bake");
    let image_path = directory.path().join("cube_ao_preview.png");

    preview::render_preview(&input, &report.object, &image_path, 128).expect("preview renders");
    assert!(image_path.exists());
    let sheet = image::open(&image_path)
        .expect("preview decodes")
        .to_rgba8();
    assert!(
        sheet.width() >= 2 * 128,
        "both panels should be side by side, got {} px",
        sheet.width()
    );

    let mut surface = 0usize;
    let mut backdrop = 0usize;
    for pixel in sheet.pixels() {
        let [r, g, b, _] = pixel.0;
        if r == g && b == g {
            surface += 1;
        }
        if [r, g, b] == preview::PREVIEW_BACKGROUND {
            backdrop += 1;
        }
    }
    assert!(surface > 100, "geometry should be drawn in the panels");
    assert!(backdrop > 100, "the backdrop frames both panels");
}

#[test]
fn a_bake_replaces_the_meshs_own_texture_with_its_occlusion() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = common::write_mesh(directory.path(), "textured.obj", TEXTURED_FIXTURE);
    common::write_mesh(directory.path(), "original.mtl", ORIGINAL_MTL);
    let before = std::fs::read(&input).expect("fixture reads");

    let report = bake(&input, None, &options(32)).expect("bake");
    let baked = objserde::parse_file(&report.object).expect("baked obj parses");

    // The mesh arrived with an albedo map and its own UVs; both are gone.
    assert_eq!(baked.material_libs.len(), 1);
    assert_eq!(
        baked.material_libs[0].path, "textured_ao.mtl",
        "the library referenced is the one this bake wrote"
    );
    assert_eq!(baked.materials.len(), 1);
    assert_eq!(baked.materials[0].name, aomesh::DEFAULT_MATERIAL_NAME);
    assert_eq!(
        baked.materials[0].diffuse_map().expect("map").path,
        "textured_ao.png",
        "and not the albedo the mesh came with"
    );
    // One triangle has nothing to occlude it, so every vertex is open, and the
    // coordinates say so rather than repeating the fixture's 0.9 / 0.1 / 0.5.
    let open = texture::coordinate(1.0);
    for coordinate in &baked.tex_coords {
        assert!(
            (coordinate.components[0] - open).abs() < 1e-12,
            "expected the baked coordinate {open}, found {:?}",
            coordinate.components
        );
    }
    assert_eq!(
        std::fs::read(&input).expect("fixture reads"),
        before,
        "the source mesh and its library are left alone"
    );
    assert!(
        !directory.path().join("original_ao.mtl").exists(),
        "the input's library is not baked alongside"
    );
}

#[test]
fn the_preview_still_renders_when_a_source_texture_cannot_be_decoded() {
    // albedo.png named by the fixture's library does not exist. A texture is
    // presentation: its triangles fall back to the flat Kd tint, and the image —
    // whose right panel is the whole point — still comes out.
    let directory = tempfile::tempdir().expect("temp dir");
    let input = common::write_mesh(directory.path(), "textured.obj", TEXTURED_FIXTURE);
    common::write_mesh(directory.path(), "original.mtl", ORIGINAL_MTL);
    let report = bake(&input, None, &options(16)).expect("bake");

    let image_path = directory.path().join("fallback_preview.png");
    preview::render_preview(&input, &report.object, &image_path, 96).expect("preview renders");
    let sheet = image::open(&image_path)
        .expect("preview decodes")
        .to_rgba8();
    assert!(sheet.width() >= 2 * 96, "both panels present");
}

/// A lone triangle with materials, a library reference and hand-written UVs.
const TEXTURED_FIXTURE: &str = "\
mtllib original.mtl
v 0 0 0
v 2 0 0
v 0 2 0
vt 0.9 0.9
vt 0.1 0.1
vt 0.5 0.5
usemtl skin
f 1/1 2/2 3/3
";

/// Names a texture that is not there, and sets a tint the fallback should use.
const ORIGINAL_MTL: &str = "\
newmtl skin
Kd 0.2 0.6 0.9
map_Kd albedo.png
";

#[test]
fn a_mesh_with_no_faces_is_refused_rather_than_baked_blank() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = common::write_mesh(directory.path(), "points.obj", "v 0 0 0\nv 1 0 0\n");

    let error = bake(&input, None, &options(16)).expect_err("no triangles to test");
    assert!(
        matches!(error, BakeError::Empty { .. }),
        "expected an empty-mesh error, got {error}"
    );
    assert!(
        !directory.path().join("points_ao.obj").exists(),
        "a refused bake writes nothing"
    );
}

#[test]
fn a_missing_input_is_reported_with_its_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let absent = directory.path().join("nowhere.obj");

    let error = bake(&absent, None, &options(16)).expect_err("nothing to read");
    assert!(
        matches!(error, BakeError::Parse { .. }),
        "expected a parse error, got {error}"
    );
    assert!(error.to_string().contains("nowhere.obj"), "{error}");
}
