use futures::future;
use image::{DynamicImage, GenericImageView, ImageError, ImageFormat};
use serde::Serialize;
use sha2::{Digest, Sha256};
use worker::{
    console_error, console_log, event, send::SendWrapper, Bucket, Context, Cors, Env, FormEntry,
    HttpMetadata, Request, Response, Result as WorkerResult, RouteContext, Router,
};

use upix_lib::{encode_image, upscale_image, ApiError, ApiResult};

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> WorkerResult<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get("/", handle_get)
        .post_async("/", handle_post_image)
        .run(req, env)
        .await
}

fn handle_get(_req: Request, _ctx: RouteContext<()>) -> WorkerResult<Response> {
    Response::ok("upix API")
}

// fn get_images(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
//     let bucket = ctx.bucket("IMGS_BUCKET")?;
//     let images = bucket.list().limit(100).execute().await?.objects();
//     console_log!("{}", images.len());
//     if images.is_empty() {
//         return Response::ok("no images found");
//     }

//     let images = images.iter().map(|img| img.key()).collect::<Vec<_>>();
//     Response::from_json(&images)
// }

async fn handle_post_image(req: Request, ctx: RouteContext<()>) -> WorkerResult<Response> {
    let res = post_image(req, ctx).await;
    match res {
        Ok(images) => Response::from_json(&images),
        Err(e) => e.to_response(),
    }
    .and_then(|r| r.with_cors(&Cors::default().with_origins(["*"])))
}

async fn post_image(mut req: Request, ctx: RouteContext<()>) -> ApiResult<Vec<UploadedImage>> {
    let Ok(bucket) = ctx.bucket("IMGS_BUCKET") else {
        console_error!("failed to get bindings to the R2 bucket");
        return Err(ApiError::no_msg(500));
    };
    let bucket = SendWrapper::new(bucket);

    let (img_data, img_fmt) = get_image_data_from_request(&mut req).await?;
    let img = image::load_from_memory_with_format(&img_data, img_fmt).map_err(|e| match e {
        ImageError::Decoding(_) => ApiError::new(400, "Failed to decode image"),
        e => {
            console_error!("failed to load image: {:?}", e);
            ApiError::no_msg(500)
        }
    })?;
    validate_img_dimension(&img)?;

    let uploader = ImageUploader {
        img,
        hash: sha256_hex(&img_data),
        dest_fmt: ImageFormat::Png,
        dest_bucket: bucket,
    };
    let upload_res = uploader.upload_all().await;
    upload_res.map_err(|_| ApiError::no_msg(500))
}

const MAX_DATA_LEN: usize = 512 * 1024;

async fn get_image_data_from_request(req: &mut Request) -> ApiResult<(Vec<u8>, ImageFormat)> {
    let Ok(Some(content_type)) = req.headers().get("Content-Type") else {
        return Err(ApiError::new(400, "Missing Content-Type header"));
    };

    if content_type.starts_with("multipart/form-data") {
        get_image_data_from_form_data(req).await
    } else {
        get_image_data_from_req_body(req, &content_type).await
    }
}

async fn get_image_data_from_req_body(
    req: &mut Request,
    ctype: &str,
) -> ApiResult<(Vec<u8>, ImageFormat)> {
    let img_fmt = validate_img_format(ctype)?;

    let Ok(img_data) = req.bytes().await else {
        console_error!("could not read request body from the request");
        return Err(ApiError::no_msg(500));
    };
    if img_data.len() > MAX_DATA_LEN {
        return Err(ApiError::new(413, "Too large image data"));
    }
    Ok((img_data, img_fmt))
}

async fn get_image_data_from_form_data(req: &mut Request) -> ApiResult<(Vec<u8>, ImageFormat)> {
    let Ok(form_data) = req.form_data().await else {
        console_error!("could not read form data from the request");
        return Err(ApiError::no_msg(500));
    };

    let Some(file_entry) = form_data.get("file") else {
        return Err(ApiError::new(400, "Missing 'file' field in form data"));
    };
    let FormEntry::File(file) = file_entry else {
        return Err(ApiError::new(400, "'file' field is not a file"));
    };

    if file.size() > MAX_DATA_LEN {
        return Err(ApiError::new(413, "Too large image data"));
    }

    let img_fmt = validate_img_format(&file.type_())?;
    let Ok(img_data) = file.bytes().await else {
        console_error!("could not read file data from the form data");
        return Err(ApiError::no_msg(500));
    };
    Ok((img_data, img_fmt))
}

fn validate_img_format(content_type: &str) -> ApiResult<ImageFormat> {
    if !content_type.starts_with("image/") {
        return Err(ApiError::new(400, "Content-Type is not for an image"));
    }
    let Some(img_fmt) = ImageFormat::from_mime_type(content_type) else {
        return Err(ApiError::new(400, "Content-Type is not for an image"));
    };

    match img_fmt {
        ImageFormat::Png | ImageFormat::WebP | ImageFormat::Bmp | ImageFormat::Gif => Ok(img_fmt),
        _ => Err(ApiError::new(
            400,
            format!("Unsupported image format: {}", img_fmt.extensions_str()[0]),
        )),
    }
}

const MAX_PIXELS: u32 = 65536;
const MAX_LONG_SIDE_LEN: u32 = 1024;
const MAX_ASPECT_RATIO: f64 = 16.0;

fn validate_img_dimension(img: &DynamicImage) -> ApiResult<()> {
    let (w, h) = img.dimensions();
    if w * h > MAX_PIXELS {
        return Err(ApiError::new(
            400,
            format!("Image has too many pixels ({} > {})", w * h, MAX_PIXELS),
        ));
    }

    let (long, short) = if w > h { (w, h) } else { (h, w) };
    if long > MAX_LONG_SIDE_LEN {
        return Err(ApiError::new(
            400,
            format!(
                "Long side of image is too long ({} > {})",
                long, MAX_LONG_SIDE_LEN
            ),
        ));
    }
    if f64::from(long) / f64::from(short) > MAX_ASPECT_RATIO {
        return Err(ApiError::new(
            400,
            format!(
                "Aspect retio of image is out of range ({} : {} > {} : 1)",
                long, short, MAX_ASPECT_RATIO
            ),
        ));
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Uploads an image to a bucket. Returns the file name (stem + extension for the image format) of the uploaded image if succeeded.
#[worker::send]
async fn upload_image_to_bucket(
    stem: &str,
    data: Vec<u8>,
    img_fmt: ImageFormat,
    bucket: SendWrapper<Bucket>,
) -> Result<String, ()> {
    console_log!("uploading image... (stem: {})", stem);

    let key = format!("{}.{}", stem, img_fmt.extensions_str()[0]);
    let meta = HttpMetadata {
        content_type: Some(img_fmt.to_mime_type().to_string()),
        ..HttpMetadata::default()
    };

    let put_res = bucket.put(&key, data).http_metadata(meta).execute().await;
    match put_res {
        Ok(_) => Ok(key),
        Err(e) => {
            console_error!("failed to upload image to the bucket: {:?}", e);
            Err(())
        }
    }
}

struct ImageUploader {
    img: DynamicImage,
    hash: String,
    dest_fmt: ImageFormat,
    dest_bucket: SendWrapper<Bucket>,
}

#[derive(Debug, Serialize)]
struct UploadedImage {
    name: String,
    scale: u32,
    width: u32,
    height: u32,
}

impl ImageUploader {
    async fn upload_all(&self) -> Result<Vec<UploadedImage>, ()> {
        let (w, h) = self.img.dimensions();
        let long = u32::max(w, h);

        let tasks = [1, 2, 4, 8, 16]
            .into_iter()
            .take_while(|&x| long * x <= 1024)
            .map(|scale| {
                if scale == 1 {
                    Box::pin(self.upload_original_image()) as future::BoxFuture<_>
                } else {
                    Box::pin(self.upload_upscaled_image(scale)) as future::BoxFuture<_>
                }
            });
        future::join_all(tasks).await.into_iter().collect()
    }

    async fn upload_original_image(&self) -> Result<UploadedImage, ()> {
        let mut img_data = Vec::new();
        encode_image(&self.img, self.dest_fmt, &mut img_data).map_err(|e| {
            console_error!("failed to encode image: {:?}", e);
        })?;

        let name = upload_image_to_bucket(
            &self.hash,
            img_data,
            self.dest_fmt,
            self.dest_bucket.clone(),
        )
        .await?;
        console_log!("uploaded original image (name: {})", &name);

        Ok(UploadedImage {
            name,
            scale: 1,
            width: self.img.width(),
            height: self.img.height(),
        })
    }

    async fn upload_upscaled_image(&self, scale: u32) -> Result<UploadedImage, ()> {
        let scaled = upscale_image(&self.img, scale);

        let mut img_data = Vec::new();
        encode_image(&scaled, self.dest_fmt, &mut img_data).map_err(|e| {
            console_error!("failed to encode image: {:?}", e);
        })?;

        // stem (file name without extension) is the hash followed by the scale
        let stem = format!("{}_{}x", self.hash, scale);

        let name = upload_image_to_bucket(&stem, img_data, self.dest_fmt, self.dest_bucket.clone())
            .await?;
        console_log!("uploaded {}x upscaled image (name: {})", scale, &name);

        Ok(UploadedImage {
            name,
            scale,
            width: scaled.width(),
            height: scaled.height(),
        })
    }
}
