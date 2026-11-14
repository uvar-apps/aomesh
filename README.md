# aomesh

Per-vertex ambient occlusion, baked into 1D texture coordinates.

`aomesh` reads a Wavefront OBJ, places a sphere of viewpoints around the mesh, and asks
one question per vertex: **of the viewpoints in front of that surface, how many can see
me?** That share becomes the vertex's texture coordinate, and a `256 × 1` black-to-white
PNG written beside the mesh turns the coordinate into an ordinary diffuse texture — so a
renderer with no ambient-occlusion support of its own displays the bake as it would
display any colour map.

"In front of" is the 180° span the vertex's normal opens into. A viewpoint behind the
surface is inside the material, not in the sky, and is counted neither as seen nor as
hidden. That is what keeps an unobstructed plane at full white: a flat square has nothing
above it, so nothing should darken it — the middle of it least of all.

Occlusion is stored in the mesh rather than faked at draw time: a vertex in a crease
carries its own darkening forever, resolution-independently.

## Build and run

```sh
cargo install aomesh
aomesh assets/torus.obj
```

From a checkout instead:

```sh
cargo build --release
./target/release/aomesh assets/torus.obj
```

A run leaves four files next to the mesh it read, and never modifies that mesh:

```
torus_ao.obj        the same geometry, with AO in its `vt` table
torus_ao.mtl        one material: white surface, AO ramp as map_Kd
torus_ao.png        the ramp: 256 pixels wide, 1 tall, 0 … 255
torus_ao_preview.png  source and bake side by side, one shared camera
```

```
Usage: aomesh [OPTIONS] <INPUT>

Arguments:
  <INPUT>  Wavefront OBJ to bake. Left untouched; outputs are new files

Options:
  -o, --output <OUTPUT>          Destination OBJ (default: input name + `_ao`)
  -s, --samples <SAMPLES>        Viewpoints on the sphere (each vertex tests the ~half in
                                 front of its normal) [default: 256]
      --material <MATERIAL>      `newmtl` name [default: aomesh_ao]
      --preview-size <SIZE>      Panel edge in pixels [default: 620]
      --no-preview               Skip the validation image
  -q, --quiet                    Report only the files written
```

To look at a bake, open `torus_ao.obj` in any OBJ viewer that resolves `map_Kd`. The
preview sheet needs no viewer: the left panel is the source mesh and the right is the
bake, rendered by [meshview](https://crates.io/crates/meshview) under one orthographic
camera, so the two differ *only* where the bake says they should.

## How it works

**Viewpoints.** `-s` points on a Fibonacci lattice over the sphere that encloses the
mesh, at 50× its diagonal. The lattice is uniform and, unlike random sampling,
deterministic, so the same `-s` puts the same viewpoints in the same places and a bake
reproduces itself byte for byte.

**Which viewpoints count.** A vertex is asked only about the ones in front of it: those
whose direction from the vertex lies within 180° of the vertex normal. A viewpoint behind
the normal is inside the material rather than in the sky, and counting it would darken
every surface merely for being the outside of something — an exposed plane would cap out
at half brightness and a closed mesh would contain no white anywhere. Normalized over the
front hemisphere, an unobstructed surface reads exactly 1.0 and only geometry actually
standing in the way pulls a vertex down. The normal is objserde's: the `vn` bound to that
position, or the area-weighted normal of its incident faces where the file stored none.

**Weighting.** Each of those viewpoints counts in proportion to `cos θ`, its angle off the
normal, so the result is the cosine-weighted hemispherical mean — the transfer integral
that vertex AO was originally defined as, and the visibility term for irradiance from a
uniform sky. This matters most exactly where the ray test is weakest: a direction 85° off
the normal counts as 0.09 of a straight-up one, and a direction at 90° counts *nothing*, so
no grazing viewpoint can mark a vertex hidden however its intersection test comes back. No
band to exclude, no boundary case to reason about — the weight does it. Visually, contact
darkening stays tight and broad shallow hollows stay clean, instead of the flat, harsh
shading a bare coverage count produces. An unobstructed surface is unaffected: numerator
and denominator are weighted the same way, so seeing everything still lands on exactly 1.0.

**Casting.** A vertex is *seen* from a viewpoint when the segment between them crosses no
triangle. Every segment is answered by a bounding-volume hierarchy built over the
mesh's triangles, so cost is `vertices × samples × log(triangles)` rather than the naive
product of all three; vertices are independent and are distributed over rayon. The
hierarchy splits its nodes where a binned surface-area heuristic says a segment is least
likely to reach, holds eight triangles per leaf, and keeps each triangle's corners and
edges in one packed record so a leaf reads sequentially. On a real 75 852-triangle mesh (a
molecular surface, 37 930 vertices, 256 viewpoints — some 4.8 million of them in front of a
surface) a whole run measures about 0.25 s: parse, hierarchy, rays and outputs.

Two details decide whether the numbers mean anything. A triangle that references the very
vertex being tested is skipped: the vertex sits *on* the surface, so without this every
neighbouring triangle would shadow it. And the "ray parallel to this triangle" test is
relative to the sizes involved, because a coplanar segment and triangle form a singular
system whose answer is meaningless rather than zero — which is also why a flat plane
occludes nothing.

**Storage.** Visibility becomes the texture coordinate. The generated OBJ carries one `vt`
per vertex — a corner addresses its own vertex's entry, so the file reads directly — and
the MTL declares one material whose `map_Kd` is the ramp, sampled through its luminance
channel with `-imfchan l`. Geometry, trailing vertex fields and `vn` bindings pass through
untouched; the input's own materials are replaced, since this file's material exists to
carry occlusion.

Writing goes through objserde's structural mode (`Obj`'s `Display`) rather than its
`write_mesh`: the latter normalizes faces to `f a//a b//b c//c` and cannot express a `vt`
at all, which is to say it cannot express an AO bake. All OBJ and MTL reading and writing
in this crate is objserde's; meshview is only ever handed geometry to rasterize.

### Why the coordinate is not just the visibility value

The obvious line — `u = visibility` — is wrong against an actual sampler. A repeating
256-texel texture is read at **texel centres**, which makes `u = 0.0` and `u = 1.0` the same
point: the seam between texel 255 and texel 0. Feeding the raw ratio through therefore
renders a perfectly occluded vertex and a perfectly open one identically, in mid grey — the
two values that matter most. So the coordinate is written as

```
u = (0.5 + 255 · visibility) / 256
```

which puts visibility 0 at the centre of texel 0 (black) and 1 at the centre of texel 255
(white). Because the ramp stores the value *k* in texel *k*, a sample returns exactly
`255 · visibility`; the deviation costs nothing that survives the texture's own 8 bits.
`tests/pipeline.rs` demonstrates the aliasing and the fix, so this cannot be
"optimised" back into a bug unnoticed. A `vt` also carries two components rather than one:
`vt u` is legal Wavefront, but samplers key off an (s, t) pair, and against a texture one
pixel tall the second component is ignored anyway.

## Performance

Measured on an Apple M4 (4 performance and 6 efficiency cores), comparing this branch
against `v0.1.0` with both binaries built in the same session and run alternately in the
same directory, so neither side gets a warmer machine. Every output — OBJ, MTL and PNG —
came out **byte-identical**, because none of this changes which triangle a segment hits:

| mesh | `-s` | ten cores | one core |
| --- | --- | --- | --- |
| molecular surface, 37 930 v / 75 852 t | 256 | 308 ms → 250 ms (**1.23×**) | 1931 ms → 1457 ms (**1.33×**) |
| same mesh | 1024 | 999 ms → 817 ms (**1.22×**) | — |
| same mesh | 16 | 67 ms → 59 ms (**1.13×**) | — |
| `torus.obj` | 256 | 6.8 ms → 6.5 ms (**1.05×**) | 25.1 ms → 21.2 ms (**1.19×**) |
| `corner.obj` | 256 | 4.8 ms → 4.8 ms (**1.01×**) | 9.6 ms → 8.0 ms (**1.21×**) |
| `stacks.obj` | 256 | 3.9 ms → 3.9 ms | 5.4 ms → 5.2 ms (**1.04×**) |

Where those milliseconds are, on the big mesh at `-s 256`: about 46 ms of fixed work —
16 ms for objserde to parse the 5.3 MB input, 17 ms to format the 7.8 MB output, 4 ms to
build the hierarchy, 1.4 ms for the filesystem — and about 205 ms inside the hierarchy. One
segment reaches 25 nodes, 14 triangles, 10.4 intersection tests, and turns out blocked 29.9%
of the time. `cargo build --release --features count` prints all of it.

Four things produced the gain, in rough order of size: the surface-area split, testing a
node's two children at their parent rather than pushing them blind, eight triangles per leaf,
and one packed record per triangle instead of re-gathering its corners from
`positions` on every test. Building the hierarchy across threads took it from 10.8 ms to
4.0 ms, which is most of what the fixed cost leaves available here — the rest of that 46 ms
belongs to objserde, in a mesh's own parse and format.

**What does not help, measured rather than assumed.** Vector instructions. Batching four
triangles through one `f64x4` Möller–Trumbore test came in 1.02–1.34× *slower* than the
scalar loop it replaced; vectorising only the box-rejection filter reached parity. The reason
is in the numbers above: a shadow ray usually hits nothing, and scalar code abandons a miss
after two or three operations in that triangle's favour, while a lane has to finish the
arithmetic. Widening the *viewpoint weights* — the one loop that is pure streaming
arithmetic, exactly what SIMD wants — cannot matter either: with the hierarchy stubbed out,
all of that arithmetic is 1.9 ms of 207 ms, so a perfect 4× on it would move a bake by 0.6%.
Storing node boxes as outward-rounded `f32`, which halves the bytes per visit, also lost
(1.17× where the 64-byte nodes reached 1.33×): no `target-cpu` flag and no ISA assumption
would have made that one work either.

**Portability.** Nothing here is architecture-specific: no `target-feature` attributes, no
runtime dispatch, no intrinsics — the code is the same on every target Rust builds for, and
rayon sized the thread pool to the cores actually available (`RAYON_NUM_THREADS` still
overrides it). The hierarchy's shape does not depend on scheduling: `fill`, `build` and the
shipped grain are asserted to produce identical nodes in identical order, so a bake on one
core matches a bake on eighty, byte for byte.

## Verification

```sh
cargo test
```

The interesting tests are the ones that do not compare the implementation against itself,
and these are exact-valued rather than tolerances around a previous run. Every fixture has
an answer that can be worked out by hand:

| fixture | what it is | what every vertex must measure |
| --- | --- | --- |
| flat 8 × 8 grid | nothing above it | 1.0, the whole middle included |
| welded cube, faces outward | convex; nothing in front of anything | 1.0 at corners, edges and face centres alike |
| same cube, faces inward | a sealed cavity | 0.0 mid-face · ≈ 0.29 on an edge · ≈ 0.59 at a corner |
| a 3-unit disk 4 units above a floor | a cone of half-angle atan(3/4) over one vertex | 0.64 directly beneath it — 0.80 if the weighting were missing |
| two parallel quads, both `vn` down | lower faces away, upper stares at the other | 1.0 below; ≈ 0.75 above |

The disk row is derived on paper: a cone of half-angle α carries `sin²α` of the
hemisphere's cosine weight, and `sin²α = r²/(r²+h²) = 9/25`, hence 0.64. A bare coverage
count would report `cos α` = 0.80 instead — so that fixture is also the proof that the
cosine term is really in the arithmetic.

The sealed cavity is where hand arithmetic let me down first, and it earns its place by
that. Mid-face is 0.0 by inspection: a cavity with nothing inside reads black. But the edge
and corner values are not clean closed forms, because a welded corner's normal is whatever
objserde's area-weighted binding produces (`[0.4082, 0.4082, 0.8165]`), not the naive
bisector an envelope derivation assumes. So the test computes its own expectation instead:
across the same viewpoint lattice, with an independent predicate — *a ray from the shell is
hidden unless some component carries it out through a wall plane the vertex sits on* — and
no ray casting anywhere near it. Oracle and solver agree to 1e-15 at every vertex, which
beats a hand figure that happens to land nearby.

An exact 1.0 across a convex cube is also the regression test for the failure this tool was
once guilty of: counting viewpoints a surface cannot address parks every face at half
brightness and leaves a bake with no white anywhere in it.

Two structural tests guard the hierarchy itself, because everything fast here is a decision
about how to arrange it, and an arrangement is only legal if it preserves the answer.
`every_triangle_sits_inside_every_box_above_it` walks a finished tree and asserts that each
node's box covers every triangle below it and that each triangle appears in exactly one
leaf — the invariant a box rejection depends on. It was written against a real break: a
suffix sweep in the split heuristic that handed a child a box covering one bin instead of
everything to its right. A sealed cavity still read black and a floor still read white; the
disk fixture, whose value is a closed form, is what failed.
`the_parallel_build_and_the_serial_one_agree` builds the same mesh three ways — one shared
node vector, a `rayon::join` at every level, and the shipped grain — and compares the nodes
one by one, so thread scheduling cannot quietly become part of an output.

Beyond that the suite covers the written files (one `vt` per vertex, corners kept as
`v/vt/vn`, `vt` arity, relative references in the MTL, the ramp's exact pixel values), the
sampler round-trip, scale invariance, bit-for-bit reproducibility, the error paths, and the
flag handling and exit codes of the binary.

## Samples

Three procedural meshes in `assets/`, chosen to bracket the behaviour:

- `corner.obj` — a floor plate and a wall butted to a right angle. The crease is the
  darkest line in the bake and is visible from outside, so this is the one to look at first.
- `torus.obj` — open surfaces bake white; the darkening sits inside the hole.
- `stacks.obj` — two plates with a narrow gap. The exposed faces stay white and the two
  faces turned towards each other go dark, in one mesh at once; from outside, the dark
  faces are hidden, which is the point.

None of these is a rendering of a reference bake; the numbers to expect are the ones above.
