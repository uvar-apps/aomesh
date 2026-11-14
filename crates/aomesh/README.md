# aomesh

Per-vertex ambient occlusion, baked into ordinary texture coordinates — for programs that
have a mesh and want it occluded, without a shader, an engine feature or a second channel.

The question a bake answers, per vertex: of the viewpoints on a sphere around the mesh that
lie **in front of that surface** — within 180° of its normal — how many can actually see it,
counting each in proportion to how face-on it is? That share becomes the vertex's texture
coordinate, and a `256 × 1` black-to-white ramp written beside the mesh turns it into an
ordinary diffuse texture any renderer can display.

Because measurement happens over the front hemisphere weighted by `cos θ`, an unobstructed
surface is exactly 1.0 white rather than "the brightest thing in a gloomy bake": white is
always reachable, only real geometry darkens a vertex, and a viewpoint at 90° carries no
weight, so grazing rays cannot hide a surface.

```sh
cargo add aomesh
```

## Baking a mesh

```rust
use aomesh::{BakeOptions, bake};

fn main() -> Result<(), aomesh::BakeError> {
    let report = bake("seat.obj".as_ref(), None, &BakeOptions::default())?;
    println!(
        "{} vertices, visibility {:.3}–{:.3}",
        report.vertices, report.visibility.min, report.visibility.max
    );
    Ok(())
}
```

Three files come out, named after the input; the input itself is never modified:

| file | contents |
| --- | --- |
| `seat_ao.obj` | the same geometry, plus one `vt u u` per vertex and a reference to the new library |
| `seat_ao.mtl` | one material, `map_Kd -imfchan l` at the ramp, so a diffuse-only pipeline reads luminance from that one channel |
| `seat_ao.png` | `256 × 1` grayscale ramp, texel *k* storing the value *k* |

Pass a destination to move and rename the set — the MTL and PNG follow the destination's stem,
since an OBJ reaches its library by a path relative to itself:

```rust
use aomesh::{BakeOptions, bake};
use std::path::Path;

fn main() -> Result<(), aomesh::BakeError> {
    bake(
        Path::new("seat.obj"),
        Some(Path::new("renders/settled.obj")),
        &BakeOptions {
            samples: 1024,
            material_name: "settled_ao".to_string(),
        },
    )?;
    Ok(())
}
```

## Taking the values instead

Nothing about occlusion requires writing a file. A caller that wants numbers — a paint-over,
a LOD choice, a metric on a support — goes straight to the measurement. This route parses the
OBJ yourself, so it wants `objserde` alongside (`cargo add objserde`); the bake above does the
same thing internally.

```rust
use aomesh::ao::{report, vertex_visibility};
use aomesh::geometry::Geometry;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let obj = objserde::parse_file(Path::new("seat.obj"))?;
    let geometry = Geometry::from_obj(&obj);
    let visibility = vertex_visibility(&geometry, 256); // one f64 in 0.0..=1.0 per vertex
    let spread = report(&visibility);
    println!("min {:.3}  mean {:.3}  max {:.3}", spread.min, spread.mean, spread.max);
    Ok(())
}
```

`visibility` is parallel to the mesh's vertices. The coordinate that gets stored is not that
share passed through, but `(0.5 + 255·v) / 256`, which lands the endpoints on texel centres —
under repeat wrapping `u = 0.0` and `u = 1.0` are the same seam, and the raw ratio would
render perfectly open and perfectly occluded as identical mid-gray.

## What it costs

Linear in `samples`, `O(n log n)` in triangles for the hierarchy once per bake, rayon over
every vertex. As a reference point, on a ten-core M4 a 37 930-vertex / 75 852-triangle mesh:
0.27 s at the default 256 viewpoints, 0.83 s at 1024 — quadrupling samples roughly triples wall
time, because parsing, formatting and writing do not grow with it. 64 is usually enough to
resolve a crease.

Scale does not matter: the viewpoint sphere sits at 50× the mesh's diagonal, so the same
settings behave identically on a 2 mm casting and a 2 km terrain. What does matter is that
everything a vertex should be occluded by is in the mesh you hand over — a bake cannot darken
a surface against props it was never given.

## Feature flags

`count` (off) adds traversal counters and phase timers: the instrument behind every number in
this documentation. The counters are atomics on the hot path, so a `count` build is not a
fast build and a timing taken with it enabled is wrong by a wide margin.

## Dependencies

Four, and none of them a rasterizer or an argument parser: `objserde` for reading and writing
OBJ and MTL, `image` to encode the ramp, `rayon` for the vertices, `thiserror` for the error
type. Drawing the side-by-side validation image is the command-line tool's job, which is why
that lives in [`aomesh-cli`](https://crates.io/crates/aomesh-cli) and meshview appears here
only as a dev-dependency, where tests use it as a second reader of what objserde wrote.

## Examples and documentation

- `cargo run -p aomesh --example bake_a_mesh` — the one-call shape, optionally on a mesh of
  your own: `--example bake_a_mesh -- ~/meshes/seat.obj`
- `cargo run -p aomesh --example visibility_values` — the numbers alone: a floor measured with
  and without two walls standing near it
- `cargo run -p aomesh --example custom_output` — destination and material name, then the
  result read back through objserde to show what a renderer will see

[docs.rs](https://docs.rs/aomesh) carries the crate tour and the per-module notes, including
what the bvh measured *not* to be faster, so nobody has to try it twice.

Rust 1.85 or newer. Dual-licensed MIT OR Apache-2.0. Source and issue tracker:
[github.com/uvar-apps/aomesh](https://github.com/uvar-apps/aomesh).
