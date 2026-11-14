//! Helpers for this package's test binaries.
//!
//! Each test file compiles this module on its own and uses a subset of it, which would
//! otherwise read as dead code. The fixtures are deliberately dumb: a command line
//! needs something to point at, and the geometry that decides whether an AO value is
//! right is pinned in the library's own suite, not here.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The program under test: this package's own `aomesh` binary.
pub const BINARY: &str = env!("CARGO_BIN_EXE_aomesh");

/// The unit cube: eight convex corners with nothing near them, so every vertex must
/// bake pure white. The white-is-reachable end of the range, checked through the
/// binary; see the library's `ao_values.rs` for the rest.
pub const CUBE: &str = "\
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
f 1 4 3 2
f 5 6 7 8
f 1 2 6 5
f 3 4 8 7
f 1 5 8 4
f 2 3 7 6
";

/// `cube.obj` in `directory`.
pub fn sample(directory: &Path) -> PathBuf {
    let path = directory.join("cube.obj");
    std::fs::write(&path, CUBE).expect("fixture writes");
    path
}

/// A lone triangle that arrives with a material library and its own UVs.
pub const TEXTURED_OBJ: &str = "\
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

/// A tint, and a texture file that is never written — so decoding it must fail.
pub const MISSING_MAP_MTL: &str = "\
newmtl skin
Kd 0.2 0.6 0.9
map_Kd albedo.png
";

/// `textured.obj` and its library in `directory`, returning the OBJ path. The PNG the
/// library names is deliberately absent.
pub fn textured_sample(directory: &Path) -> PathBuf {
    std::fs::write(directory.join("original.mtl"), MISSING_MAP_MTL).expect("fixture writes");
    let path = directory.join("textured.obj");
    std::fs::write(&path, TEXTURED_OBJ).expect("fixture writes");
    path
}

/// Run the program with these arguments.
#[must_use]
pub fn invoke(args: &[&str]) -> std::process::Output {
    Command::new(BINARY)
        .args(args)
        .output()
        .expect("the binary runs")
}

/// What the program printed on stdout.
pub fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// What the program printed on stderr, which is where a failure explains itself.
pub fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A path a `&str` argument can take.
pub fn arg(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}
