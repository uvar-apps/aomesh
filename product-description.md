# AOMESH

Per vertex ambient occlusion calculation stored into a 1D texture.

This Rust command line tool reads meshes from OBJ files using the `objserde` crate
then computes the per vertex ambient occlusion by checking the visibility of each
vertex from multiple viewpoints located on a sphere surrounding the mesh and pointing
toward the center of the mesh/spehere. The occlusion is proportional to how many times
the vertex is hidden from each viewpoint. It then generates a new mesh with ambient occlusion
stored as 1D texture coordinates and the texture coordinate of the vertex matching the ambient occlustion value.

It must be possible to configure the number of viewpoints used.

After computing each visibility a 1D texture coordinate is assigned to each vertex
matching the visibility computed as:

(number of time the vertex is visibile from each viewpoint) / (total number of viewpoints).

A 1D texture going from black (0) to white (1) is generated.
The texture should be a 256 grayscale 1D image i.e. 256 pixels wide and 1 pixel tall
with 1D (luminance, scalar) pixel values going from 0 to 255.

The visibility of the vertex matches the 1D texture coordinate which in turn matches the
texture color e.g. if the visibility is zero it maps to texture coordinate zero which
maps to color 0, if the fixibility is maximum, i.e. 1, it maps to texture coordinate
1 which is white.

All the texture coordinates are stored inside the generated OBJ file and an
accompanying MTL file stores texture information and points to the 1D texture
stored as a PNG file referenced from the diffuse texture map parameter.

To validate the implementation generate an image from the mesh file using
the `meshview` Rust crate.
