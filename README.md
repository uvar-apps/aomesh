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
product of all three; vertices are independent and are distributed over rayon. On a real
75 852-triangle mesh (a molecular surface, 37 930 vertices, 256 viewpoints — some 4.8
million of them in front of a surface) a whole run measures about 0.9 s: parse, hierarchy,
rays and outputs.

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
