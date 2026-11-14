//! The validation image.
//!
//! The tool draws a source mesh and its baked twin side by side so a person can see
//! that the bake moved shading and nothing else. That renderer lives in this package,
//! private to the binary, so these tests drive the program and read back the PNG — the
//! level at which the feature actually exists. The geometry that decides whether an AO
//! *value* is right is pinned in the library's suite; what is checked here is the sheet.

mod common;

use common::{arg, invoke, sample, stderr, textured_sample};

/// Decode the sheet as RGBA.
fn sheet(path: &std::path::Path) -> image::RgbaImage {
    image::open(path)
        .expect("the sheet decodes as a PNG")
        .to_rgba8()
}

#[test]
fn the_validation_image_shows_both_meshes() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());

    let output = invoke(&["-s", "64", "--preview-size", "128", arg(&input)]);
    assert!(output.status.success(), "{}", stderr(&output));

    let path = directory.path().join("cube_ao_preview.png");
    assert!(path.exists(), "the image the run reported is not there");
    let sheet = sheet(&path);
    assert!(
        sheet.width() >= 2 * 128,
        "both panels should be side by side, got {} px",
        sheet.width()
    );

    // The two kinds of pixel are the two things the sheet is made of, and they are
    // separable by design: a bake's shading is a grayscale ramp, while the backdrop is
    // deliberately *not* gray, so that a fully occluded — near black — surface cannot
    // read as empty space. The exact backdrop colour is an implementation detail; that
    // the two populations are distinguishable is the requirement.
    let mut surface = 0usize;
    let mut backdrop = 0usize;
    for pixel in sheet.pixels() {
        let [r, g, b, _] = pixel.0;
        if r == g && g == b {
            surface += 1;
        } else {
            backdrop += 1;
        }
    }
    assert!(
        surface > 100,
        "geometry should be drawn in the panels, found {surface} gray px of {}",
        sheet.pixels().len()
    );
    assert!(
        backdrop > 100,
        "the non-gray backdrop should frame both panels, found {backdrop}"
    );
}

#[test]
fn the_image_still_renders_when_a_source_texture_cannot_be_decoded() {
    // The fixture's library names albedo.png, which is never written. A texture the
    // input came with is presentation: its triangles fall back to the flat Kd tint, and
    // the sheet — whose right panel is the whole point — still comes out.
    let directory = tempfile::tempdir().expect("temp dir");
    let input = textured_sample(directory.path());

    let output = invoke(&["-s", "16", "--preview-size", "96", arg(&input)]);
    assert!(
        output.status.success(),
        "an undecodable source texture must not fail the run: {}",
        stderr(&output)
    );

    let path = directory.path().join("textured_ao_preview.png");
    assert!(
        path.exists(),
        "the bake was reported, so the sheet should be"
    );
    assert!(sheet(&path).width() >= 2 * 96, "both panels present");
}
