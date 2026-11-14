# aomesh-cli

The `aomesh` command: bake per-vertex ambient occlusion into a Wavefront OBJ's texture
coordinates, plus the `256 × 1` ramp that makes an ordinary renderer display it.

This package is the program. The bake itself — the viewpoints, the ray tests, the numbers —
is [`aomesh`](https://crates.io/crates/aomesh), which this crate calls and does not
reimplement; what lives here is argument parsing, the report, and the validation image.

```sh
cargo install aomesh-cli
aomesh path/to/mesh.obj
```

From a checkout of the [repository](https://github.com/uvar-apps/aomesh), at its root — which
comes with three sample meshes under `assets/`:

```sh
cargo build --release
./target/release/aomesh assets/torus.obj
```

## What a run does

```
/tmp/cli-doc/torus.obj: 960 vertices, 1920 triangles, 256 viewpoints
visibility  min 0.588  mean 0.909  max 1.000
wrote /tmp/cli-doc/torus_ao.obj
wrote /tmp/cli-doc/torus_ao.mtl
wrote /tmp/cli-doc/torus_ao.png
wrote /tmp/cli-doc/torus_ao_preview.png
done in 0.05s
```

**Line 1** counts what was read: triangles after polygon faces are fan-triangulated,
viewpoints after clamping.

**Line 2** is the spread of the bake, and it is worth reading before opening the mesh. `max`
of 1.0 means something in there faces open sky — white is reachable, so the measurement is
normalizing correctly. A `max` below 1.0 with nothing plausible covering the mesh means the
file occludes itself where it should not, which is the failure worth catching here rather
than in a render. `min` 0.588 on this torus is the inside of the hole, where the far side of
the tube really is in the way.

**Lines 3 to 6** name the files, all new, all beside the destination you gave (default: the
input's name with `_ao` appended):

| file | what it is |
| --- | --- |
| `torus_ao.obj` | the same geometry — positions, normals, trailing vertex fields — with one `vt u u` per vertex |
| `torus_ao.mtl` | one material: `map_Kd -imfchan l torus_ao.png`, so a diffuse-only pipeline reads occlusion from the single channel's luminance |
| `torus_ao.png` | the ramp: 256 pixels wide, one tall, texel *k* storing the value *k* |
| `torus_ao_preview.png` | the source mesh and the bake side by side under one shared orthographic camera, so any difference between the panels is a shading difference and nothing else |

The input is never modified, and its own material libraries are not baked alongside: the AO
material replaces whatever `map_Kd` the mesh arrived with, because this file's purpose is to
carry occlusion.

### Failure

A bake that cannot happen prints one line on stderr and exits 1:

```
$ aomesh nofaces.obj          # parsed, but nothing to occlude against
aomesh: nofaces.obj: nothing to occlude against — the file has no triangles
$ aomesh nope.obj             # nothing there, or unreadable
aomesh: nope.obj: nope.obj: No such file or directory (os error 2)
```

The second line repeats the path — objserde prefixes it in its own error and `aomesh` prefixes
it again. A cosmetic flaw in a message, found while writing this README; fixing it would
change the program's output, so it is filed as [uvar-apps/aomesh#8](https://github.com/uvar-apps/aomesh/issues/8)
rather than slipped into a release whose whole claim is that its bytes did not move.

Anything the argument parser rejects — an unknown flag, `-s 0` — exits 2 with clap's own
`error: …` message instead, before any file is opened.

## Flags

| flag | default | what it does |
| --- | --- | --- |
| `<INPUT>` | — | the OBJ to bake. Read only; outputs are new files |
| `-o, --output <PATH>` | input name + `_ao` | destination OBJ. The MTL and PNG take that stem and directory; a path with no extension is treated as a stem |
| `-s, --samples <N>` | 256 | viewpoints on the enclosing sphere. Each vertex tests the ones in front of its normal — about half. Cost is linear; 64 resolves a crease, 1024 is for a final. `0` is refused |
| `--material <NAME>` | `aomesh_ao` | the `newmtl` name in the generated library |
| `--preview-size <N>` | 620 | edge length in pixels of each panel of the validation image |
| `--no-preview` | off | skip the validation image — the bake writes OBJ, MTL and PNG only |
| `-q, --quiet` | off | print only the files written, not the counts or the spread |
| `-h, --help` | | full help; `-h` gives the one-line summary |

## Looking at a bake

Open `torus_ao.obj` in any viewer that resolves `map_Kd` — that is the whole claim: no AO
support needed downstream. Or skip the viewer and open `torus_ao_preview.png`: the two panels
are drawn from the same camera, at the same scale and orientation, so they cannot differ by
silhouette, only by the darkening the bake wrote into the coordinates.

## Timing yourself

`cargo build --release --features count` adds traversal counters and phase timers to stderr —
parse, build, cast, format, write, and per-ray figures like nodes visited and the share of
segments that turn out blocked. Those counters are atomics on the hot path: use the build to
find out *where* time goes, and a plain build to measure whether it went anywhere.

Rust 1.85 or newer. Dual-licensed MIT OR Apache-2.0. The library's documentation — semantics,
output contract, the API tour — is at
[docs.rs/aomesh](https://docs.rs/aomesh); issues on this repository.
