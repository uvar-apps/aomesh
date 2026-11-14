//! The 1D AO texture and the coordinate that addresses it.
//!
//! The texture is a `256 × 1` grayscale PNG running 0 … 255: black at the left
//! end (occluded), white at the right (fully visible). Because the stored value
//! equals the texel index, a texel-centre sample returns exactly the visibility
//! it was addressed with.

use std::path::Path;

use image::{GrayImage, ImageFormat};

/// Texels in the 1D texture, and the number of distinct AO levels it holds.
pub const TEXTURE_WIDTH: usize = 256;

/// The black-to-white ramp: [`TEXTURE_WIDTH`] pixels wide, one tall, scalar
/// luminance `0 … 255`.
#[must_use]
pub fn ramp() -> GrayImage {
    GrayImage::from_fn(TEXTURE_WIDTH as u32, 1, |x, _| {
        image::Luma([x.min(TEXTURE_WIDTH as u32 - 1) as u8])
    })
}

/// Write `image` as a PNG, format stated rather than inferred from the name.
///
/// # Errors
///
/// Any I/O or encoder failure, including a path that cannot be created.
pub fn write_png(image: &GrayImage, path: &Path) -> std::io::Result<()> {
    image
        .save_with_format(path, ImageFormat::Png)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Texture coordinate for a visibility value, as it is written to `vt`.
///
/// The obvious mapping — `u = visibility` — is wrong against a real sampler.
/// Under repeat wrapping a `256`-texel texture is read at **texel centres**
/// (`fract(u) · 256 − 0.5`), which makes `u = 0.0` and `u = 1.0` the same point:
/// the boundary between texel 255 and texel 0. Feeding the raw ratio through
/// therefore renders the two values that matter most — perfectly occluded and
/// perfectly open — as identical mid-gray.
///
/// Mapping the unit interval across the *centres* of the first and last texel
/// keeps both endpoints honest and costs no precision that survives the texture's
/// own 8 bits:
///
/// ```text
/// u = (0.5 + 255 · visibility) / 256
/// visibility 0.0 → 1/512    → texel   0 → black
/// visibility 1.0 → 511/512  → texel 255 → white
/// ```
///
/// Since texel *k* stores the value *k*, the sampled result is `255 · visibility`.
///
/// ```
/// use aomesh::texture::coordinate;
///
/// // Each endpoint lands on a texel centre, read back as `fract(u) · 256 − 0.5`.
/// assert_eq!(coordinate(0.0) * 256.0 - 0.5, 0.0, "texel 0: black, fully occluded");
/// assert_eq!(coordinate(1.0) * 256.0 - 0.5, 255.0, "texel 255: white, fully open");
/// // Which is the point: passed through unchanged, 0.0 and 1.0 are the same seam.
/// assert_ne!(coordinate(0.0), coordinate(1.0));
/// // Outside the range a visibility cannot go, clamped rather than wrapped.
/// assert_eq!(coordinate(-0.5), coordinate(0.0));
/// assert_eq!(coordinate(1.5), coordinate(1.0));
/// ```
#[must_use]
pub fn coordinate(visibility: f64) -> f64 {
    let clamped = visibility.clamp(0.0, 1.0);
    (0.5 + (TEXTURE_WIDTH as f64 - 1.0) * clamped) / TEXTURE_WIDTH as f64
}

/// The texture level a visibility rounds to. Used only for reporting; the written
/// coordinate keeps full precision.
///
/// ```
/// use aomesh::texture::level;
///
/// assert_eq!(level(0.0), 0);
/// assert_eq!(level(1.0), 255);
/// assert_eq!(level(0.5), 128, "half of 255 is 127.5, and it rounds away from black");
/// ```
#[must_use]
pub fn level(visibility: f64) -> u8 {
    let clamped = visibility.clamp(0.0, 1.0);
    (clamped * (TEXTURE_WIDTH as f64 - 1.0)).round() as u8
}
