//! The validation image.
//!
//! meshview draws — it is a rasterizer, and it is the product description's chosen
//! instrument for checking that the bake looks like the mesh it came from. It does
//! not parse anything here: both panels are assembled from an objserde model, so
//! the OBJ and MTL readers in this crate are objserde alone.
//!
//! This is the only place in the workspace with a rasterizer, deliberately: drawing a
//! picture for a human to eyeball is a property of the tool, not of a bake, and
//! meshview carries `clap` and some twenty crates with it. The library stays free of
//! both, so a program that links `aomesh` to occlude a mesh pays for geometry and rays
//! only.
//!
//! The sheet puts the source mesh and the baked mesh side by side under **one**
//! shared orthographic camera. A pixel difference between the panels can then only
//! mean a *shading* difference: same scale, same orientation, same silhouette.
//! That is the thing worth seeing, because the bake's whole claim is that it moved
//! the texture coordinates and nothing else.

use std::collections::HashMap;
use std::path::Path;

use meshview::{
    CompareOptions, Mesh, Texture, TexturedCompareItem, TexturedMesh, TriTex,
    render_compare_textured, write_png_rgba,
};
use thiserror::Error;

/// Tile edge length, in pixels, of each panel.
pub const DEFAULT_PREVIEW_SIZE: usize = 620;

/// Three-quarter view: head-on, a convex mesh hides its own shading behind its
/// silhouette, which is precisely the half of the picture being validated.
const PREVIEW_YAW: f64 = 0.65;
const PREVIEW_PITCH: f64 = 0.35;

/// Supersampling factor per tile.
const PREVIEW_AA: usize = 2;

/// Fraction of a tile the mesh should fill.
const PREVIEW_FILL: f64 = 0.8;

/// Panel background. Deliberately not a gray: a fully occluded surface renders
/// near black, and a black backdrop would make it read as empty space.
pub const PREVIEW_BACKGROUND: [u8; 3] = [60, 20, 80];

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("{path}: {source}")]
    Parse {
        path: String,
        source: objserde::ObjError,
    },
    #[error("{}: path is not valid UTF-8, which meshview's PNG writer requires", .path.display())]
    InvalidPath { path: std::path::PathBuf },
    #[error(transparent)]
    Sheet(#[from] meshview::SheetError),
}

/// Render `source` and `baked` side by side into `output` as a PNG.
///
/// `baked` is read back off disk rather than taken from memory, so the image also
/// proves that the written OBJ, its MTL and the texture resolve to the coordinates
/// that were computed.
///
/// # Errors
///
/// See [`PreviewError`]: an OBJ that will not parse, a path the PNG writer cannot
/// use, or an empty comparison.
pub fn render_preview(
    source: &Path,
    baked: &Path,
    output: &Path,
    size: usize,
) -> Result<(), PreviewError> {
    let original = load_for_raster(source).map_err(|error| PreviewError::Parse {
        path: source.display().to_string(),
        source: error,
    })?;
    let textured = load_for_raster(baked).map_err(|error| PreviewError::Parse {
        path: baked.display().to_string(),
        source: error,
    })?;

    // One camera for both panels, framed by the original's bounds.
    let zoom = (PREVIEW_FILL * size as f64) / longest_edge(&original.mesh).max(f64::MIN_POSITIVE);
    let options = CompareOptions {
        zoom,
        focus: Some(original.mesh.centroid()),
        size,
        aa: PREVIEW_AA,
        yaw: PREVIEW_YAW,
        pitch: PREVIEW_PITCH,
        background: PREVIEW_BACKGROUND,
        ..CompareOptions::default()
    };
    let source_label = stem(source);
    let baked_label = stem(baked);
    let items = [
        TexturedCompareItem {
            name: &source_label,
            mesh: &original,
        },
        TexturedCompareItem {
            name: &baked_label,
            mesh: &textured,
        },
    ];
    let result = render_compare_textured(&items, &options)?;

    // meshview's PNG writer is byte-deterministic and takes a `&str`; a path that
    // is not UTF-8 cannot be handed to it.
    let output_text = output.to_str().ok_or_else(|| PreviewError::InvalidPath {
        path: output.to_path_buf(),
    })?;
    write_png_rgba(output_text, &result.sheet)?;
    Ok(())
}

fn longest_edge(mesh: &Mesh) -> f64 {
    let mut min = [f64::MAX; 3];
    let mut max = [f64::MIN; 3];
    for point in &mesh.positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2])
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Parse an OBJ — objserde only — and assemble what meshview draws from it.
///
/// Normals come from objserde's binding layer: one unit normal per position, the
/// area-weighted geometric normal where no `vn` was bound. That is the same
/// contract meshview applies in its own loader, so this crate and a meshview
/// render of the same file agree about what the lighting means.
///
/// A `map_Kd` that will not decode leaves those triangles on their flat `Kd` tint
/// rather than failing the run: a texture is presentation, and losing one is not a
/// reason to lose the image.
fn load_for_raster(path: &Path) -> Result<TexturedMesh, objserde::ObjError> {
    let obj = objserde::parse_file(path)?;
    let directory = path.parent().unwrap_or(Path::new("."));
    let bound = obj.bind();

    let mut indices = Vec::with_capacity(bound.triangles.len());
    let mut tri_tex = Vec::with_capacity(bound.triangles.len());
    let mut textures: Vec<Texture> = Vec::new();
    let mut slots: HashMap<String, Option<usize>> = HashMap::new();

    for face in &obj.faces {
        let material = obj.material_of(face);
        let kd = material
            .and_then(|material| material.diffuse)
            .unwrap_or([1.0; 3]);
        let shaded = material.is_some();
        let texture = material
            .and_then(|material| material.diffuse_map())
            .and_then(|map| resolve_texture(directory, &map.path, &mut slots, &mut textures));

        for triangle in face.triangulate() {
            let mut uv = [[0.0; 2]; 3];
            let mut has_uv = true;
            for (slot, corner) in uv.iter_mut().zip(triangle.corners) {
                match corner
                    .vt
                    .and_then(|index| obj.tex_coords.get(index as usize))
                {
                    Some(coordinate) if coordinate.arity >= 2 => {
                        *slot = [coordinate.components[0], coordinate.components[1]];
                    }
                    _ => has_uv = false,
                }
            }
            indices.push([
                triangle.corners[0].v,
                triangle.corners[1].v,
                triangle.corners[2].v,
            ]);
            tri_tex.push(TriTex {
                uv,
                texture: if has_uv { texture } else { None },
                kd,
                shaded_by_material: shaded,
            });
        }
    }

    Ok(TexturedMesh {
        mesh: Mesh {
            positions: bound.positions,
            normals: bound.unit_normals,
            indices,
        },
        tri_tex,
        textures,
    })
}

/// The texture slot for `reference`, decoding it on first use.
///
/// The path is taken relative to the OBJ that named it, which is where an MTL
/// reference lives. A file that is absent or will not decode yields `None`, and
/// the triangles using it fall back to their flat `Kd`.
fn resolve_texture(
    directory: &Path,
    reference: &str,
    slots: &mut HashMap<String, Option<usize>>,
    textures: &mut Vec<Texture>,
) -> Option<usize> {
    let resolved = directory.join(reference).to_string_lossy().into_owned();
    if let Some(&known) = slots.get(&resolved) {
        return known;
    }
    let slot = Texture::load(&resolved).ok().map(|texture| {
        let index = textures.len();
        textures.push(texture);
        index
    });
    slots.insert(resolved, slot);
    slot
}
