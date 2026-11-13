//! Per-vertex ambient occlusion, baked into 1D texture coordinates.
//!
//! `aomesh` reads a Wavefront OBJ, places a configurable number of viewpoints on a
//! sphere around it, and asks one question per vertex: of the viewpoints that lie in
//! front of that surface — within 180° of its normal — how many can actually see it,
//! counting each in proportion to how face-on it is? That weighted share becomes the
//! vertex's texture coordinate, and a `256 × 1` black-to-white ramp is written beside the
//! mesh so an ordinary renderer, with no AO support of its own, reproduces the occlusion
//! as an ordinary diffuse texture.
//!
//! Measuring over the front hemisphere, weighted by `cos θ`, is what keeps an
//! unobstructed surface at full brightness: nothing in front of it means white, and only
//! real geometry — a cavity wall, a plate resting above — darkens a vertex. A viewpoint at
//! exactly 90° carries no weight at all, so a grazing ray cannot call a vertex hidden.
//!
//! Three dependencies carry the whole product, each doing one job:
//! [`objserde`] reads and writes the OBJ and MTL, a hand-built [`Bvh`] answers the
//! visibility questions, and [`meshview`] rasterizes the validation image.
//!
//! ```no_run
//! use aomesh::{BakeOptions, bake};
//!
//! let report = bake("cube.obj".as_ref(), None, &BakeOptions::default())?;
//! println!("{} → {}", report.object.display(), report.texture.display());
//! # Ok::<(), aomesh::BakeError>(())
//! ```

pub mod ao;
pub mod bvh;
pub mod geometry;
pub mod output;
pub mod preview;
pub mod texture;

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::geometry::Geometry;

/// Material name written to the generated MTL when the caller does not choose one.
pub const DEFAULT_MATERIAL_NAME: &str = "aomesh_ao";

#[derive(Debug, Error)]
pub enum BakeError {
    #[error("{}: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: objserde::ObjError,
    },
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: nothing to occlude against — the file has no triangles", .path.display())]
    Empty { path: PathBuf },
}

/// What a bake should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeOptions {
    /// Viewpoints per vertex.
    pub samples: usize,
    /// `newmtl` name in the generated library.
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
/// `_ao` suffix unless a destination is given. The input is never modified.
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

    let visibility = ao::vertex_visibility(&geometry, options.samples);
    let coordinates: Vec<f64> = visibility
        .iter()
        .map(|&value| texture::coordinate(value))
        .collect();

    let names = output::OutputNames::resolve(input, destination, &options.material_name);
    let baked = output::ao_obj(&obj, &coordinates, &names);
    write(&names.object, &output::obj_text(&baked))?;
    write(&names.material, &output::mtl_text(&names))?;
    write_ramp(&names.texture)?;

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
