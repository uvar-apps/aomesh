//! Assembling and writing the bake's output triple, in objserde only.
//!
//! The generated OBJ carries the same geometry it read — positions, trailing
//! vertex fields, normals — with the AO answer attached as texture coordinates:
//! one `vt` per vertex, and every face corner of a vertex pointing at that
//! vertex's own entry. The `.mtl` declares one material whose `map_Kd` is the
//! `256 × 1` ramp, so a renderer resolving the vertex's coordinate reads back the
//! grey that vertex was judged.
//!
//! Writing is objserde's Mode A (`Obj`'s `Display`), deliberately: the crate's
//! Mode B (`write_mesh`) normalizes faces to `f a//a b//b c//c` and cannot carry
//! a `vt` at all, so it cannot express an AO bake.

use std::path::{Path, PathBuf};

use objserde::{
    Channel, Corner, Face, LibraryStatus, Map, MapKind, MapOption, Material, MaterialLib,
    MtlWriteOptions, Obj, TexCoord,
};

/// The three files one bake produces, plus the names they use for each other.
///
/// An OBJ reaches its material library and a material reaches its texture by a
/// path relative to the file doing the referring, so the references are bare file
/// names and all three outputs sit side by side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputNames {
    /// `<stem>.obj`
    pub object: PathBuf,
    /// `<stem>.mtl`
    pub material: PathBuf,
    /// `<stem>.png`
    pub texture: PathBuf,
    /// `mtllib` spelling.
    pub material_reference: String,
    /// `map_Kd` spelling.
    pub texture_reference: String,
    /// `newmtl` spelling.
    pub material_name: String,
}

impl OutputNames {
    /// Name the outputs for a bake of `input`.
    ///
    /// With no explicit destination the outputs join the input under an `_ao`
    /// suffix, so a bake never overwrites the mesh it came from.
    ///
    /// ```
    /// use aomesh::output::OutputNames;
    /// use std::path::Path;
    ///
    /// let names = OutputNames::resolve(Path::new("meshes/torus.obj"), None, "aomesh_ao");
    /// assert_eq!(names.object, Path::new("meshes/torus_ao.obj"));
    /// assert_eq!(names.material, Path::new("meshes/torus_ao.mtl"));
    /// assert_eq!(names.texture, Path::new("meshes/torus_ao.png"));
    /// // Inside the OBJ the references are bare file names: the three outputs are
    /// // written side by side, so the mesh stays portable as a set.
    /// assert_eq!(names.material_reference, "torus_ao.mtl");
    /// assert_eq!(names.texture_reference, "torus_ao.png");
    ///
    /// // A destination renames the whole set and moves it too.
    /// let moved = OutputNames::resolve(
    ///     Path::new("meshes/torus.obj"),
    ///     Some(Path::new("out/baked.obj")),
    ///     "ao",
    /// );
    /// assert_eq!(moved.object, Path::new("out/baked.obj"));
    /// assert_eq!(moved.texture, Path::new("out/baked.png"));
    ///
    /// // A destination with no extension is taken as a stem.
    /// let stem = OutputNames::resolve(Path::new("m.obj"), Some(Path::new("out/ao")), "ao");
    /// assert_eq!(stem.object, Path::new("out/ao.obj"));
    /// ```
    #[must_use]
    pub fn resolve(input: &Path, output: Option<&Path>, material_name: &str) -> Self {
        let object = match output {
            Some(path) => {
                if path.extension().is_none() {
                    path.with_extension("obj")
                } else {
                    path.to_path_buf()
                }
            }
            None => {
                let stem = input.file_stem().unwrap_or_default().to_string_lossy();
                // `with_stem` is not stable, so the name is rebuilt and the input's
                // extension kept if it had one.
                let mut name = format!("{stem}_ao");
                if let Some(extension) = input.extension() {
                    name.push('.');
                    name.push_str(&extension.to_string_lossy());
                }
                input.with_file_name(name)
            }
        };
        let stem = object.file_stem().unwrap_or_default().to_string_lossy();
        let directory = object.parent().unwrap_or(Path::new("."));
        let material = directory.join(format!("{stem}.mtl"));
        let texture = directory.join(format!("{stem}.png"));
        Self {
            material_reference: file_name(&material),
            texture_reference: file_name(&texture),
            object,
            material,
            texture,
            material_name: material_name.to_string(),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Build the output model: input geometry, AO in the texture coordinates.
///
/// `coordinates` must be parallel to `input.vertices` — one `vt` value per
/// vertex, as produced by [`crate::texture::coordinate`].
#[must_use]
pub fn ao_obj(input: &Obj, coordinates: &[f64], names: &OutputNames) -> Obj {
    // One `vt` per vertex, so a corner's texture index is its own vertex index
    // and the mapping stays readable in the file.
    let tex_coords: Vec<TexCoord> = coordinates
        .iter()
        .map(|&u| {
            // Two components, the second a copy of the first. A 1D `vt u` is
            // legal Wavefront, but samplers key off an (s, t) pair — a texture at
            // one pixel tall ignores whatever `t` says, so replicating `u` costs
            // nothing and makes the coordinate readable by every OBJ reader.
            TexCoord::new(&[u, u]).expect("two components")
        })
        .collect();

    let faces = input
        .faces
        .iter()
        .map(|face| Face {
            corners: face
                .corners
                .iter()
                .map(|corner| Corner {
                    vt: Some(corner.v),
                    ..*corner
                })
                .collect(),
            material: Some(0),
        })
        .collect();

    Obj {
        vertices: input.vertices.clone(),
        normals: input.normals.clone(),
        tex_coords,
        faces,
        material_libs: vec![MaterialLib {
            path: names.material_reference.clone(),
            // The library is authored alongside this OBJ in the same run, so the
            // reference is real by the time anything reads it back.
            status: LibraryStatus::Loaded,
        }],
        materials: vec![ao_material(names)],
    }
}

/// The one material of a baked mesh: white surface, AO ramp as the diffuse map,
/// read through the luminance channel.
#[must_use]
pub fn ao_material(names: &OutputNames) -> Material {
    Material {
        name: names.material_name.clone(),
        library: Some(0),
        defined: true,
        ambient: Some([1.0, 1.0, 1.0]),
        // Diffuse multiplies the sampled texel, so anything but white would tint
        // the occlusion instead of showing it.
        diffuse: Some([1.0, 1.0, 1.0]),
        maps: vec![Map {
            kind: MapKind::Diffuse,
            path: names.texture_reference.clone(),
            options: vec![MapOption::Channel(Channel::Luminance)],
        }],
        ..Material::default()
    }
}

/// Serialize a model to OBJ text.
#[must_use]
pub fn obj_text(obj: &Obj) -> String {
    obj.to_string()
}

/// Serialize the AO material library to MTL text.
#[must_use]
pub fn mtl_text(names: &OutputNames) -> String {
    objserde::write_material_lib(
        &[ao_material(names)],
        &MtlWriteOptions {
            header: vec![format!(
                "# ambient occlusion for {}",
                names.object.display()
            )],
        },
    )
}

/// Write `text` to `path`, creating the parent directory when needed.
pub fn write_text(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}
