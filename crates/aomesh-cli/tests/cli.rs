//! The command line itself: flags, exit codes, and which files appear.
//!
//! The library is covered elsewhere; these run the built binary, because the flag
//! wiring is product surface too — a default that overwrites the input, or
//! `--samples 0` dividing by zero, is a bug no library test would notice.

mod common;

use common::{invoke, sample, stderr, stdout};

/// The three numbers on the `visibility  min …  mean …  max …` summary line.
fn reported_visibility(text: &str) -> Vec<f64> {
    let line = text
        .lines()
        .find(|line| line.starts_with("visibility"))
        .expect("the summary reports visibility");
    line.split_whitespace()
        .filter_map(|token| token.parse::<f64>().ok())
        .collect()
}

#[test]
fn a_run_writes_the_default_set_and_exits_zero() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());

    let output = invoke(&[
        "--no-preview",
        "-s",
        "2048",
        input.to_str().expect("utf-8 path"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    for name in ["cube_ao.obj", "cube_ao.mtl", "cube_ao.png"] {
        assert!(directory.path().join(name).exists(), "{name} missing");
        assert!(printed.contains(name), "{name} not reported:\n{printed}");
    }
    assert!(
        !directory.path().join("cube_ao_preview.png").exists(),
        "--no-preview must skip the image"
    );

    // 8 vertices, 6 quads fan-split into 12 triangles.
    assert!(printed.contains("8 vertices"), "{printed}");
    assert!(printed.contains("12 triangles"), "{printed}");
    let visibility = reported_visibility(&printed);
    assert_eq!(visibility.len(), 3, "{printed}");
    for value in visibility {
        assert!(
            (value - 1.0).abs() < 1e-12,
            "a convex cube hides nothing from itself, so min, mean and max should all \
             read 1.0 — pure white; the run reported {value}"
        );
    }
}

#[test]
fn quiet_reports_only_the_files_written() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());

    let output = invoke(&["-q", "--no-preview", input.to_str().expect("utf-8 path")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(
        !printed.contains("visibility") && !printed.contains("done in"),
        "quiet should drop the measurements:\n{printed}"
    );
    assert!(printed.contains("cube_ao.png"), "{printed}");
}

#[test]
fn the_validation_image_is_written_by_default() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());

    let output = invoke(&[input.to_str().expect("utf-8 path")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let image = directory.path().join("cube_ao_preview.png");
    assert!(image.exists(), "the validation image is the default");
    let size = std::fs::metadata(&image).expect("preview stats").len();
    assert!(size > 1000, "a two-panel sheet is not {size} bytes");
    assert!(
        stdout(&output).contains("cube_ao_preview.png"),
        "and it is reported"
    );
}

#[test]
fn an_explicit_output_renames_the_whole_set() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());
    let destination = directory.path().join("baked.obj");

    let output = invoke(&[
        "--no-preview",
        "-o",
        destination.to_str().expect("utf-8 path"),
        input.to_str().expect("utf-8 path"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(destination.exists());
    assert!(directory.path().join("baked.mtl").exists());
    assert!(directory.path().join("baked.png").exists());
    assert!(
        !directory.path().join("cube_ao.obj").exists(),
        "an explicit destination replaces the default names, it does not add to them"
    );
}

#[test]
fn zero_viewpoints_is_refused_before_any_work() {
    let directory = tempfile::tempdir().expect("temp dir");
    let input = sample(directory.path());

    let output = invoke(&["-s", "0", input.to_str().expect("utf-8 path")]);
    assert!(!output.status.success(), "zero viewpoints cannot divide");
    assert!(
        stderr(&output).contains("samples"),
        "the refusal should name the flag: {}",
        stderr(&output)
    );
    assert!(
        !directory.path().join("cube_ao.obj").exists(),
        "a refused run writes nothing"
    );
}

#[test]
fn a_missing_input_fails_with_a_message_and_a_nonzero_status() {
    let directory = tempfile::tempdir().expect("temp dir");
    let absent = directory.path().join("nope.obj");

    let output = invoke(&[absent.to_str().expect("utf-8 path")]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.starts_with("aomesh:"), "{message}");
    assert!(message.contains("nope.obj"), "{message}");
    assert!(
        stdout(&output).is_empty(),
        "nothing should be reported as written, got {}",
        stdout(&output)
    );
}
