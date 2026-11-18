//! The bake as a whole: files out, and what comes back when they are read again.
//!
//! Everything here reads the outputs with objserde — the same reader the tool used
//! to write them — and, for the parts a renderer cares about, with meshview. That
//! order matters: a bake that agreed with itself in memory but wrote an OBJ that
//! resolves to something else would still produce a nice picture, and it would be
//! the wrong picture.

mod common;

use std::path::Path;

use aomesh::{BakeError, BakeOptions, bake, texture};

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
        mtl.contains("\nmap_Kd other.png\n"),
        "the library must point at its own texture by name, with no options:\n{mtl}"
    );
    // The ramp is greyscale, so `-imfchan l` could not change what a reader samples —
    // and readers that skip options they do not implement take it for the file name.
    assert!(!mtl.contains("-imfchan"), "{mtl}");
}

/// Hard and welded edges are stated in an OBJ as `s` lines, and a bake that drops
/// them quietly changes how the mesh shades downstream. objserde 0.4 models
/// smoothing; this is the bake keeping it.
#[test]
fn smoothing_groups_survive_the_bake() {
    let directory = tempfile::tempdir().expect("temp dir");
    let source = "\
v 0 0 0
v 8 0 0
v 8 8 0
v 0 8 0
v 2 2 0
v 6 2 0
v 6 6 0
v 2 6 0
s 1
f 1 2 3 4
s off
f 5 6 7 8
";
    let input = common::write_mesh(directory.path(), "smoothed.obj", source);
    let report = bake(&input, None, &options(16)).expect("bake");

    let written = std::fs::read_to_string(&report.object).expect("baked obj reads");
    let baked = objserde::parse(&written).expect("baked obj parses");
    let from_source = objserde::parse(source).expect("source parses");

    assert!(
        written.contains("\ns 1\n") && written.contains("\ns off\n"),
        "both statements survive: {written}"
    );
    assert_eq!(baked.smooth_groups, ["1", "off"], "{written}");
    assert_eq!(
        baked
            .faces
            .iter()
            .map(|face| face.smooth_group)
            .collect::<Vec<_>>(),
        from_source
            .faces
            .iter()
            .map(|face| face.smooth_group)
            .collect::<Vec<_>>(),
        "every face keeps the smoothing value it was written under"
    );

    // The other half of the promise, and the one that protects every existing
    // bake: a mesh that never mentioned smoothing grows no `s` statement, so its
    // output stays what the earlier versions wrote.
    let (cube, _) = common::cube_with_kinds(2);
    let quiet = common::write_mesh(directory.path(), "plain.obj", &cube);
    let report = bake(&quiet, None, &options(16)).expect("bake");
    let written = std::fs::read_to_string(&report.object).expect("baked obj reads");
    let stray: Vec<&str> = written
        .lines()
        .filter(|line| line.starts_with('s'))
        .collect();
    assert!(stray.is_empty(), "a smoothing-free mesh gained {stray:?}");
}

/// A molecular surface arrives with a provenance header and one group per
/// connected component; the bake must hand both back, each group still in
/// front of the faces it named.
#[test]
fn header_comments_objects_and_groups_survive_the_bake() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (cube, _) = common::cube_with_kinds(2);
    // Split the cube's faces into two groups under one object, behind a header.
    let mut text = String::from("# sesmesh 1.1.2\n# area_A2: 6.0000\no ses\n");
    let mut faces_seen = 0;
    for line in cube.lines() {
        if line.starts_with("f ") {
            if faces_seen == 0 {
                text.push_str("g component_0\n");
            } else if faces_seen == 12 {
                text.push_str("g cavity_1\n");
            }
            faces_seen += 1;
        }
        text.push_str(line);
        text.push('\n');
    }
    let input = common::write_mesh(directory.path(), "grouped.obj", &text);

    let report = bake(&input, None, &options(16)).expect("bake");

    let written = std::fs::read_to_string(&report.object).expect("baked obj reads");
    assert!(
        written.starts_with("# sesmesh 1.1.2\n# area_A2: 6.0000\nmtllib grouped_ao.mtl\n"),
        "{written}"
    );
    let baked = objserde::parse(&written).expect("baked obj parses");
    let source = objserde::parse(&text).expect("source parses");
    assert_eq!(baked.objects, ["ses"]);
    assert_eq!(baked.groups, ["component_0", "cavity_1"]);
    let groups = |obj: &objserde::Obj| -> Vec<Option<u32>> {
        obj.faces.iter().map(|face| face.group).collect()
    };
    assert_eq!(
        groups(&baked),
        groups(&source),
        "every face keeps its group"
    );
    assert!(baked.faces.iter().all(|face| face.object == Some(0)));
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
    assert_eq!(error.path(), absent, "the accessor hands over the file");
}

/// How many times a message says a file's name. Once is right: twice means two layers
/// each prefixed the same path, and zero means the caller cannot tell which file broke.
fn times_named(message: &str, name: &str) -> usize {
    message.matches(name).count()
}

#[test]
fn every_failure_names_its_file_exactly_once() {
    let directory = tempfile::tempdir().expect("temp dir");

    // The case that started it: objserde's own I/O error already spells the path out, so
    // prefixing it again printed the file twice.
    let absent = directory.path().join("nowhere.obj");
    let error = bake(&absent, None, &options(8)).expect_err("nothing to read");
    assert_eq!(
        times_named(&error.to_string(), "nowhere.obj"),
        1,
        "missing input: {error}"
    );

    // The mirror case, and the reason the fix is conditional: a syntax error carries a
    // line number and no file name at all, so the wrapper's prefix is the only thing
    // naming it. Dropping the prefix everywhere — the obvious way to fix the case above —
    // would make every malformed OBJ anonymous. A test on the missing file alone would
    // have passed.
    let malformed = common::write_mesh(
        directory.path(),
        "malformed.obj",
        "v 0 0 x\nv 1 0 0\nv 0 1 0\nf 1 2 3\n",
    );
    let error = bake(&malformed, None, &options(8)).expect_err("`x` is not a coordinate");
    let message = error.to_string();
    assert_eq!(times_named(&message, "malformed.obj"), 1, "{message}");
    assert!(
        message.contains("line 1"),
        "the line number survives: {message}"
    );

    // Refused for having nothing to occlude against: named once, and it is a `Empty`.
    let bare = common::write_mesh(directory.path(), "bare.obj", "v 0 0 0\nv 1 0 0\n");
    let error = bake(&bare, None, &options(8)).expect_err("no triangles");
    assert!(matches!(error, BakeError::Empty { .. }), "{error}");
    assert_eq!(times_named(&error.to_string(), "bare.obj"), 1, "{error}");

    // An output that cannot be written names the destination once — `blocker` is a file,
    // so `blocker/inner/` can never exist.
    std::fs::write(directory.path().join("blocker"), "").expect("blocker written");
    let blocked = directory.path().join("blocker/inner/out.obj");
    let input = cube_fixture(directory.path());
    let error = bake(&input, Some(&blocked), &options(8))
        .expect_err("a directory cannot be made inside a file");
    assert!(
        matches!(error, BakeError::Io { .. }),
        "expected an output failure, got {error}"
    );
    assert_eq!(
        times_named(&error.to_string(), "out.obj"),
        1,
        "unwritable destination: {error}"
    );
    assert_eq!(
        error.path(),
        blocked,
        "and the accessor names the output, not the mesh being baked"
    );
}
