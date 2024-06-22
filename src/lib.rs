use std::io::Cursor;

use futures::future;
use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use sha2::{Digest, Sha256};
use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get("/", handle_get)
        .post_async("/", handle_post_image)
        .run(req, env)
        .await
}

fn handle_get(mut _req: Request, _ctx: RouteContext<()>) -> Result<Response> {
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

type SendBucket = send::SendWrapper<Bucket>;

macro_rules! map_pin {
    ($($e:expr),* $(,)?) => {
        vec![$(Box::pin($e)),*]
    };
}

async fn handle_post_image(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let Ok(bucket) = ctx.bucket("IMGS_BUCKET") else {
        return Response::error("Internal Server Error", 500);
    };
    let bucket = send::SendWrapper::new(bucket);

    let Ok(Some(content_type)) = req.headers().get("Content-Type") else {
        return Response::error("Bad Request", 400);
    };
    if !content_type.starts_with("image/") {
        return Response::error("Bad Request", 400);
    }
    let Some(img_fmt) = ImageFormat::from_mime_type(content_type) else {
        return Response::error("Bad Request", 400);
    };

    let Ok(body) = req.bytes().await else {
        return Response::error("Bad Request", 400);
    };
    let Ok((img, hash)) = load_image_with_hash(body, img_fmt) else {
        return Response::error("Internal Server Error", 500);
    };

    let uploader = ImageUploader {
        img,
        hash,
        dest_fmt: ImageFormat::Png,
        dest_bucket: bucket,
    };

    let tasks: Vec<future::BoxFuture<_>> = map_pin![
        uploader.upload_original_image(),
        uploader.upload_upscaled_image(2),
        uploader.upload_upscaled_image(4),
        uploader.upload_upscaled_image(8),
        uploader.upload_upscaled_image(16),
    ];
    let task_res: Result<Vec<_>> = future::join_all(tasks).await.into_iter().collect();

    match task_res {
        Ok(_) => Response::empty().map(|r| r.with_status(201)), // 201 Created
        Err(e) => {
            console_error!("{:?}", e);
            Response::error("Internal Server Error", 500)
        }
    }
}

fn load_image_with_hash(img_data: Vec<u8>, img_fmt: ImageFormat) -> Result<(DynamicImage, String)> {
    let mut hasher = Sha256::new();
    hasher.update(&img_data);
    let hash = hex::encode(hasher.finalize());

    let img = image::load_from_memory_with_format(&img_data, img_fmt)
        .map_err(|e| Error::RustError(e.to_string()))?;

    Ok((img, hash))
}

fn write_image(img: &DynamicImage, img_fmt: ImageFormat, dest: &mut Vec<u8>) -> Result<()> {
    let mut buf = Cursor::new(dest);
    img.write_to(&mut buf, img_fmt)
        .map_err(|e| Error::RustError(e.to_string()))?;

    Ok(())
}

#[worker::send]
async fn upload_image_to_bucket(
    name: &str,
    data: Vec<u8>,
    img_fmt: ImageFormat,
    bucket: SendBucket,
) -> Result<()> {
    let key = format!("{}.{}", name, img_fmt.extensions_str()[0]);
    let meta = HttpMetadata {
        content_type: Some(img_fmt.to_mime_type().to_string()),
        ..HttpMetadata::default()
    };

    bucket.put(key, data).http_metadata(meta).execute().await?;
    Ok(())
}

struct ImageUploader {
    img: DynamicImage,
    hash: String,
    dest_fmt: ImageFormat,
    dest_bucket: SendBucket,
}

impl ImageUploader {
    async fn upload_original_image(&self) -> Result<()> {
        let mut img_data = Vec::new();
        write_image(&self.img, self.dest_fmt, &mut img_data)?;
        upload_image_to_bucket(
            &self.hash,
            img_data,
            self.dest_fmt,
            self.dest_bucket.clone(),
        )
        .await?;
        console_log!("uploaded original image");
        Ok(())
    }

    async fn upload_upscaled_image(&self, scale: u32) -> Result<()> {
        let (w, h) = self.img.dimensions();
        let img = self.img.resize(w * scale, h * scale, FilterType::Nearest);

        let mut img_data = Vec::new();
        write_image(&img, self.dest_fmt, &mut img_data)?;

        let name = format!("{}_{}x", self.hash, scale);
        upload_image_to_bucket(&name, img_data, self.dest_fmt, self.dest_bucket.clone()).await?;
        console_log!("uploaded {}x upscaled image", scale);
        Ok(())
    }
}
