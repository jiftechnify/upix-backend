use std::io::Cursor;

use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageError, ImageFormat};
use serde_json::json;
use sha2::{Digest, Sha256};
use worker::{Response, Result as WorkerResult};

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

#[derive(Debug)]
pub struct ApiError {
    status: u16,
    message: Option<String>,
}

impl ApiError {
    pub fn new(status: u16, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: Some(msg.into()),
        }
    }
    pub fn no_msg(status: u16) -> Self {
        Self {
            status,
            message: None,
        }
    }

    pub fn to_response(&self) -> WorkerResult<Response> {
        let r = match &self.message {
            None => Response::empty(),
            Some(msg) => Response::from_json(&json!({ "message": msg })),
        };
        r.map(|r| r.with_status(self.status))
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Calculate the SHA-256 hash of the given data and convert it to a hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}
