use rust_embed::Embed;
use salvo::prelude::*;

#[derive(Embed)]
#[folder = "static"]
struct StaticAssets;

/// SPA handler: serve static files from the embedded `gateway-dixous/dist/` directory.
///
/// 1. Try to match the request path to an exact embedded file.
/// 2. If not found, fall back to `index.html` (for client-side routing).
#[handler]
pub(crate) async fn spa_handler(req: &mut Request, res: &mut Response) {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = StaticAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();

        // Set cache headers for hashed assets.
        if path.contains("/assets/") {
            res.headers_mut().insert(
                "cache-control",
                "public, max-age=31536000, immutable".parse().unwrap(),
            );
        } else {
            res.headers_mut()
                .insert("cache-control", "no-cache".parse().unwrap());
        }

        res.headers_mut()
            .insert("content-type", mime.as_ref().parse().unwrap());

        res.write_body(file.data.to_vec()).ok();
    } else {
        // SPA fallback  - serve index.html for all non-file routes.
        if let Some(index) = StaticAssets::get("index.html") {
            res.headers_mut()
                .insert("content-type", "text/html; charset=utf-8".parse().unwrap());
            res.headers_mut()
                .insert("cache-control", "no-cache".parse().unwrap());
            res.write_body(index.data.to_vec()).ok();
        } else {
            res.status_code(StatusCode::NOT_FOUND);
        }
    }
}
