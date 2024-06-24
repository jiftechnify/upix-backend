use regex::Regex;
use send::SendWrapper;
use upix_lib::{encode_image, upscale_image, ApiError, ApiResult};
use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    match handle(req, env, ctx).await {
        Ok(resp) => Ok(resp),
        Err(e) => e.to_response(),
    }
}

async fn handle(req: Request, env: Env, ctx: Context) -> ApiResult<Response> {
    // deny methods other than GET
    if req.method() != Method::Get {
        console_log!("unsupported method: {:?}", req.method());
        return Err(ApiError::no_msg(405)); // 405 Method Not Allowed
    }

    // get bindings to the bucket
    let Ok(bucket) = env.bucket("IMGS_BUCKET") else {
        console_error!("Failed to get bindings to the R2 bucket");
        return Err(ApiError::no_msg(500));
    };
    let bucket = SendWrapper::new(bucket);

    // return cached response if available
    let cache = Cache::default();
    let cached_resp = cache.get(&req, false).await.map_err(|e| {
        console_error!("Failed to match request against cache: {:?}", e);
        ApiError::no_msg(500)
    })?;
    if let Some(resp) = cached_resp {
        console_log!("Cache hit: {}", req.path());
        return Ok(resp);
    }

    // generate a response with upscaled image
    let img_data = generate_upscaled_image(&req.path(), bucket).await?;
    let resp_headers: Headers = [
        ("Content-Type", "image/png"),
        ("Cache-Control", "public, max-age=31536000"),
    ]
    .iter()
    .collect();
    let mut resp = Response::from_bytes(img_data)
        .map(|r| r.with_headers(resp_headers))
        .unwrap();

    // cache the response
    let resp2 = resp.cloned().unwrap();
    ctx.wait_until(async move {
        match cache.put(&req, resp2).await {
            Ok(_) => console_log!("Cached response: {}", req.path()),
            Err(e) => console_error!("Failed to cache response: {:?}", e),
        }
    });

    Ok(resp)
}

async fn generate_upscaled_image(
    req_path: &str,
    bucket: SendWrapper<Bucket>,
) -> ApiResult<Vec<u8>> {
    let Some(parts) = match_req_path(req_path) else {
        console_log!("Path doesn't match the pattern: {}", req_path);
        return Err(ApiError::no_msg(404));
    };
    if parts.ext != "png" {
        console_log!("Unsupported extension: {}", parts.ext);
        return Err(ApiError::no_msg(404));
    }

    // get source image data from the bucket
    let src_img_data = bucket
        .get(format!("{}.png", parts.hash))
        .execute()
        .await
        .map_err(|e| {
            console_error!("Failed to fetch image from the bucket: {:?}", e);
            ApiError::no_msg(500)
        })?
        .ok_or_else(|| {
            console_log!("Image not found: {}", parts.hash);
            ApiError::no_msg(404)
        })?
        .body()
        .ok_or_else(|| {
            console_error!("Object doesn't have body");
            ApiError::no_msg(500)
        })?
        .bytes()
        .await
        .map_err(|e| {
            console_error!("Failed to read object body: {:?}", e);
            ApiError::no_msg(500)
        })?;

    // upscale the image
    let src_img = image::load_from_memory_with_format(&src_img_data, image::ImageFormat::Png)
        .map_err(|e| {
            console_error!("Failed to decode image from memory: {:?}", e);
            ApiError::no_msg(500)
        })?;
    let upscaled_img = if parts.scale == 1 {
        src_img
    } else {
        upscale_image(&src_img, parts.scale)
    };

    let mut upscaled_img_data = Vec::new();
    encode_image(
        &upscaled_img,
        image::ImageFormat::Png,
        &mut upscaled_img_data,
    )
    .map_err(|e| {
        console_error!("Failed to encode image: {:?}", e);
        ApiError::no_msg(500)
    })?;
    Ok(upscaled_img_data)
}

struct ReqPathParts {
    hash: String,
    scale: u32,
    ext: String,
}

fn match_req_path(path: &str) -> Option<ReqPathParts> {
    let re_path =
        Regex::new(r"^/(?P<hash>[0-9a-f]{64})(?P<sx>_(?P<scale>[0-9]+)x)?\.(?P<ext>[a-z]+)$")
            .unwrap();
    let caps = re_path.captures(path)?;

    let hash = caps.name("hash")?.as_str().to_string();
    let scale = match caps.name("sx") {
        Some(_) => caps.name("scale")?.as_str().parse().ok()?,
        None => 1,
    };
    let ext = caps.name("ext")?.as_str().to_string();
    Some(ReqPathParts { hash, scale, ext })
}

#[cfg(test)]
mod test {
    use super::match_req_path;

    const HASH: &str = "1ea5e9febc7265432c41cf87b41f9ca1ea084bec600509add2c04048a8fec600";

    #[test]
    fn test_match_req_path() {
        let path = format!("{}_2x.png", HASH);
        let parts = match_req_path(&path).unwrap();
        assert_eq!(parts.hash, HASH);
        assert_eq!(parts.scale, 2);
        assert_eq!(parts.ext, "png");

        let path = format!("{}.png", HASH);
        let parts = match_req_path(&path).unwrap();
        assert_eq!(parts.hash, HASH);
        assert_eq!(parts.scale, 1);
        assert_eq!(parts.ext, "png");

        let path = "notahash_2x.png";
        let parts = match_req_path(path);
        assert!(parts.is_none());

        let path = format!("{}_2x", HASH);
        let parts = match_req_path(&path);
        assert!(parts.is_none());
    }
}
