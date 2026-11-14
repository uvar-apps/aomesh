//! Per-vertex ambient occlusion, baked into ordinary texture coordinates.
//!
//! The question a bake answers, for every vertex: of the viewpoints on a sphere around
//! the mesh that lie **in front of that surface** — within 180° of its normal — how
//! many can actually see it, counting each in proportion to how face-on it is? The
//! answer, a share in `0.0..=1.0`, becomes the vertex's texture coordinate, and a
//! `256 × 1` black-to-white ramp is written beside the mesh. An ordinary renderer with
//! no AO support of its own then reproduces the occlusion as an ordinary diffuse
//! texture, which is the whole point: no shader, no engine feature, no second channel.
//!
//! Measuring over the front hemisphere, weighted by `cos θ`, is what makes the numbers
//! mean what they say. Nothing in front of a surface leaves it exactly 1.0, so white is
//! always reachable; only real geometry — a cavity wall, a plate resting above — pulls
//! a vertex down; and a viewpoint at 90° carries no weight, so a grazing ray cannot
//! call a vertex hidden no matter what the intersection test returns.
//!
//! Two dependencies carry the compute, each doing one job: [`objserde`] reads and
//! writes the OBJ and MTL, and a hand-built [`bvh::Bvh`] answers the visibility
//! questions. `image` encodes the ramp. This crate computes and writes files; it draws
//! nothing. The side-by-side validation image the command-line program produces is that
//! program's concern, and `aomesh-cli` is the package holding the rasterizer.
//!
//! # Quick start
//!
//! ```
//! use aomesh::{BakeOptions, bake};
//!
//! // The unit cube. Convex, so it hides nothing from itself.
//! let cube = "\
//! v 0 0 0
//! v 1 0 0
//! v 1 1 0
//! v 0 1 0
//! v 0 0 1
//! v 1 0 1
//! v 1 1 1
//! v 0 1 1
//! f 1 4 3 2
//! f 5 6 7 8
//! f 1 2 6 5
//! f 3 4 8 7
//! f 1 5 8 4
//! f 2 3 7 6
//! ";
//! let directory = tempfile::tempdir()?;
//! let input = directory.path().join("cube.obj");
//! std::fs::write(&input, cube)?;
//!
//! let report = bake(&input, None, &BakeOptions::default())?;
//! assert_eq!(report.vertices, 8);
//! assert_eq!(report.triangles, 12, "six quads, each fan-split into two");
//! assert!(
//!     report.visibility.min > 0.999_999,
//!     "a convex mesh cannot occlude itself, so nothing should be dark"
//! );
//! assert!(report.object.ends_with("cube_ao.obj"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What a bake writes
//!
//! | file | contents |
//! | --- | --- |
//! | `<stem>_ao.obj` | the input's geometry, plus one `vt u u` per vertex and a reference to the new library |
//! | `<stem>_ao.mtl` | one material selecting the ramp with `map_Kd -imfchan l`, so a diffuse-only pipeline reads luminance from that one channel |
//! | `<stem>_ao.png` | the `256 × 1` grayscale ramp, texel *k* storing the value *k* |
//!
//! The names come from [`output::OutputNames::resolve`]: beside the input under an `_ao`
//! suffix, or following the destination the caller chose, in which case the MTL and PNG
//! take that stem too. **The input is never modified** — a bake reads it and writes new
//! files, and the input's own material libraries are left alone rather than baked over.
//!
//! # Reading a value
//!
//! `1.0` is open sky, `0.0` is fully occluded, and nothing is inverted anywhere in this
//! crate. The stored coordinate is not that share passed straight through: it is
//! `(0.5 + 255·v) / 256`, which maps the unit interval onto the *centres* of the first
//! and last texel, because under repeat wrapping `u = 0.0` and `u = 1.0` are the same
//! point and would render perfectly open and perfectly occluded as the same mid-gray.
//! [`texture::coordinate`] carries the derivation.
//!
//! # Two ways in
//!
//! - [`bake`] — a path in, three files out, and a [`BakeReport`] saying what happened.
//! - [`ao::vertex_visibility`] — the values alone, for a caller that already holds a
//!   mesh and wants occlusion for its own purposes: a paint-over, a LOD decision, a
//!   quality metric. [`geometry::Geometry::from_obj`] turns an objserde model into what
//!   that function measures, and nothing about it requires a file.
//!
//! # Runnable examples
//!
//! Three self-checking programs in `examples/`, each one a shape of call rather than a
//! fragment — `cargo run -p aomesh --example <name>`, and `cargo test` compiles all three:
//!
//! - `bake_a_mesh` — the one-call bake, with an optional mesh path argument.
//! - `visibility_values` — the numbers alone, no files: a floor measured with and
//!   without two walls stood near it.
//! - `custom_output` — destination and material name, then the result read back with
//!   objserde to show what a renderer will actually see.
//!
//! # Cost
//!
//! Work is linear in [`BakeOptions::samples`] — every vertex tests the same sphere of
//! viewpoints — and the hierarchy is built once per bake, `O(n log n)` in triangles. As
//! a reference point, on a ten-core M4 a 37 930-vertex, 75 852-triangle mesh takes
//! 0.27 s at the default 256 samples and 0.83 s at 1024: quadrupling the viewpoints
//! roughly triples the wall time, because the fixed part — parsing, formatting, writing
//! — does not grow with samples. Lowering `samples` is therefore the one lever, and 64
//! is usually enough to see a crease.
//!
//! Viewpoints sit on one sphere about the whole mesh at [`ao::VIEW_RADIUS_FACTOR`] times
//! its diagonal, so scale is irrelevant: the same settings behave identically on a 2 mm
//! casting and a 2 km terrain. What does matter is that all the geometry a vertex should
//! be occluded by is in the file — a bake cannot darken a surface against props it was
//! never given.
//!
//! # Feature flags
//!
//! - `count` (off): per-query traversal counters and phase timers on stderr, printed by
//!   the CLI and the instrument behind every figure in this documentation. The counters
//!   are atomics on the hot path: a `count` binary is not a fast binary, and a number
//!   measured with it enabled is wrong by a wide margin.
//!
//! # Modules
//!
//! | | answers |
//! | --- | --- |
//! | [`ao`] | how open is each vertex, and what the measurement must exclude to mean it |
//! | [`bvh`] | can this segment reach that viewpoint, including what was measured *not* to be faster |
//! | [`geometry`] | an OBJ projected onto positions, welded normals and triangles |
//! | [`output`] | the model and material library a bake writes, and how they are named |
//! | [`texture`] | the ramp, and the coordinate that addresses it correctly |
//!
//! # Errors
//!
//! [`BakeError`] names the offending path in every case. [`BakeError::Parse`] means the
//! OBJ could not be read, which includes a path that is not there; [`BakeError::Empty`]
//! means it holds no triangles, so there is nothing to occlude against and a bake would
//! be a blank lie; [`BakeError::Io`] means an output could not be written. The first two
//! are the input's problem and the third is the destination's.
//!
//! # Requirements
//!
//! Rust 1.85 or newer; edition 2024. Dual-licensed MIT OR Apache-2.0.

pub mod ao;
pub mod bvh;
pub mod geometry;
pub mod output;
pub mod texture;

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::geometry::Geometry;

/// Material name written to the generated MTL when the caller does not choose one.
pub const DEFAULT_MATERIAL_NAME: &str = "aomesh_ao";

#[derive(Debug, Error)]
pub enum BakeError {
    /// The OBJ could not be read — it does not parse, or there is nothing at that
    /// path. The message names the file; [`objserde::ObjError`] says which line.
    /// Nothing is written.
    #[error("{}: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: objserde::ObjError,
    },
    /// An output could not be written: the destination is not writable, or its parent
    /// cannot be created. Earlier outputs of the same bake may already exist, so a
    /// caller that cares should treat a bake as all-or-nothing by destination.
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file parsed but holds no triangle with three distinct corners, so there is
    /// nothing for rays to hit and every vertex would report 1.0 — a bake that would
    /// produce three files telling a lie about the mesh. Refused instead.
    #[error("{}: nothing to occlude against — the file has no triangles", .path.display())]
    Empty { path: PathBuf },
}

/// What a bake should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeOptions {
    /// Viewpoints on the enclosing sphere. Each vertex tests the ones in front of its
    /// own normal — roughly half of these — and each of those counts in proportion to
    /// how face-on it is, so viewpoints near the horizon barely register. Cost is
    /// linear in this number; 64 resolves a crease, 256 ([`ao::DEFAULT_SAMPLES`]) is
    /// the default, and a value this side of 4096 is what the test suite pins the
    /// closed forms at. Zero is treated as one rather than dividing by nothing.
    pub samples: usize,
    /// `newmtl` name in the generated library. Anything a Wavefront material name can
    /// be; [`DEFAULT_MATERIAL_NAME`] is what the CLI leaves it as.
    pub material_name: String,
}

impl Default for BakeOptions {
    fn default() -> Self {
        Self {
            samples: ao::DEFAULT_SAMPLES,
            material_name: DEFAULT_MATERIAL_NAME.to_string(),
        }
    }
}

/// What a bake did.
#[derive(Clone, Debug, PartialEq)]
pub struct BakeReport {
    /// Vertices in the mesh — one visibility value each.
    pub vertices: usize,
    /// Triangles the rays were cast against, after fan-triangulation.
    pub triangles: usize,
    /// Viewpoints actually used.
    pub samples: usize,
    /// Spread of the computed visibility.
    pub visibility: ao::VisibilityReport,
    /// The baked OBJ.
    pub object: PathBuf,
    /// Its material library.
    pub material: PathBuf,
    /// The `256 × 1` ramp.
    pub texture: PathBuf,
}

/// Bake `input` and write the OBJ, MTL and texture.
///
/// Outputs are named by [`output::OutputNames::resolve`]: beside the input under an
/// `_ao` suffix unless a destination is given. The input is never modified — a bake
/// reads it and writes new files, leaving the mesh and its own material libraries
/// byte for byte as they were. Where the input referenced a texture of its own, the
/// output does not: the AO map replaces it, which is the only way a single diffuse
/// channel can show occlusion.
///
/// Rayon spreads the vertices over the process's whole thread pool. Call `bake` from
/// inside a parallel iterator and you are scheduling on top of yourself; the values
/// will be right, and nothing else will be fast.
///
/// ```
/// use aomesh::{BakeOptions, bake};
///
/// // A floor: four corners, one quad, nothing above it.
/// const FLOOR: &str = "v 0 0 0\nv 4 0 0\nv 4 4 0\nv 0 4 0\nf 1 2 3 4\n";
///
/// let directory = tempfile::tempdir()?;
/// let input = directory.path().join("floor.obj");
/// std::fs::write(&input, FLOOR)?;
/// let before = std::fs::read(&input)?;
///
/// let report = bake(
///     &input,
///     Some(&directory.path().join("renders/floor_ao.obj")),
///     &BakeOptions {
///         samples: 64,
///         material_name: "floor_ao".to_string(),
///     },
/// )?;
///
/// // A destination moves and renames the whole set; the directory is created.
/// assert_eq!(report.object, directory.path().join("renders/floor_ao.obj"));
/// assert_eq!(report.material, directory.path().join("renders/floor_ao.mtl"));
/// assert_eq!(report.texture, directory.path().join("renders/floor_ao.png"));
/// assert_eq!((report.vertices, report.triangles, report.samples), (4, 2, 64));
/// // Under the open sky, every vertex is white — the range has a top.
/// assert_eq!(report.visibility.min, 1.0);
/// assert_eq!(std::fs::read(&input)?, before, "the input is untouched");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// [`BakeError::Parse`] when the OBJ will not read, [`BakeError::Empty`] when it
/// holds no triangles to test against, and [`BakeError::Io`] when an output cannot
/// be written.
pub fn bake(
    input: &Path,
    destination: Option<&Path>,
    options: &BakeOptions,
) -> Result<BakeReport, BakeError> {
    #[cfg(feature = "count")]
    let mut clock = std::time::Instant::now();
    #[cfg(feature = "count")]
    let lap = |label: &str, clock: &mut std::time::Instant| {
        eprintln!("{label:>12} {:6.1} ms", clock.elapsed().as_secs_f64() * 1e3);
        *clock = std::time::Instant::now();
    };
    let obj = objserde::parse_file(input).map_err(|source| BakeError::Parse {
        path: input.to_path_buf(),
        source,
    })?;
    let geometry = Geometry::from_obj(&obj);
    if geometry.is_empty() {
        return Err(BakeError::Empty {
            path: input.to_path_buf(),
        });
    }

    #[cfg(feature = "count")]
    lap("parse+bind", &mut clock);
    let visibility = ao::vertex_visibility(&geometry, options.samples);
    #[cfg(feature = "count")]
    lap("build+cast", &mut clock);
    let coordinates: Vec<f64> = visibility
        .iter()
        .map(|&value| texture::coordinate(value))
        .collect();

    let names = output::OutputNames::resolve(input, destination, &options.material_name);
    let baked = output::ao_obj(&obj, &coordinates, &names);
    #[cfg(feature = "count")]
    lap("format", &mut clock);
    write(&names.object, &output::obj_text(&baked))?;
    write(&names.material, &output::mtl_text(&names))?;
    write_ramp(&names.texture)?;
    #[cfg(feature = "count")]
    lap("write", &mut clock);
    Ok(BakeReport {
        vertices: obj.vertices.len(),
        triangles: geometry.triangles.len(),
        samples: options.samples.max(1),
        visibility: ao::report(&visibility),
        object: names.object,
        material: names.material,
        texture: names.texture,
    })
}

/// Write bytes, with `BakeError` naming the file that failed.
fn write(path: &Path, contents: &str) -> Result<(), BakeError> {
    output::write_text(path, contents).map_err(|source| BakeError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Write the ramp texture, which is bytes rather than text.
fn write_ramp(path: &Path) -> Result<(), BakeError> {
    texture::write_png(&texture::ramp(), path).map_err(|source| BakeError::Io {
        path: path.to_path_buf(),
        source,
    })
}
