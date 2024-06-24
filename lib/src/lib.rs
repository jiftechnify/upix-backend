use std::io::Cursor;

use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageError, ImageFormat};

/// Encode the `DynamicImage` into a `dest` buffer with the given format.
pub fn encode_image(
    img: &DynamicImage,
    img_fmt: ImageFormat,
    dest: &mut Vec<u8>,
) -> Result<(), ImageError> {
    let mut buf = Cursor::new(dest);
    img.write_to(&mut buf, img_fmt)
}

/// Upscale the image by a given scale factor and return it as a brand new `DynamicImage`.
pub fn upscale_image(img: &DynamicImage, scale: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    img.resize(w * scale, h * scale, FilterType::Nearest)
}
