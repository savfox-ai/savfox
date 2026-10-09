use rust_embed::Embed;
use salvo::prelude::*;

#[derive(Embed)]
#[folder = "static"]
struct StaticAssets;

/// SPA handler: serve static files from the embedded `gateway-dioxus/dist/` directory.
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
                "public, max-age=31536000, immutable"
                    .parse()
                    .expect("static cache-control header should parse"),
            );
        } else {
            res.headers_mut().insert(
                "cache-control",
                "no-cache"
                    .parse()
                    .expect("static cache-control header should parse"),
            );
        }

        res.headers_mut().insert(
            "content-type",
            mime.as_ref()
                .parse()
                .expect("guessed content-type header should parse"),
        );

        res.write_body(file.data.to_vec()).ok();
    } else {
        // SPA fallback  - serve index.html for all non-file routes.
        if let Some(index) = StaticAssets::get("index.html") {
            res.headers_mut().insert(
                "content-type",
                "text/html; charset=utf-8"
                    .parse()
                    .expect("html content-type header should parse"),
            );
            res.headers_mut().insert(
                "cache-control",
                "no-cache"
                    .parse()
                    .expect("static cache-control header should parse"),
            );
            res.write_body(index.data.to_vec()).ok();
        } else {
            res.status_code(StatusCode::NOT_FOUND);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires canonical scripts/build-web.ps1 artifacts"]
    async fn built_frontend_assets_are_served_with_spa_fallback() {
        let static_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
        let index = std::fs::read(static_dir.join("index.html"))
            .expect("run scripts/build-web.ps1 before this verification");
        assert!(!index.is_empty());
        let script = StaticAssets::iter()
            .find(|path| path.ends_with(".js"))
            .expect("canonical frontend has JavaScript");
        let wasm = StaticAssets::iter()
            .find(|path| path.ends_with(".wasm"))
            .expect("canonical frontend has WebAssembly");
        for path in ["/", "/channels", script.as_ref(), wasm.as_ref()] {
            let mut request = Request::new();
            *request.uri_mut() = format!("/{}", path.trim_start_matches('/'))
                .parse()
                .unwrap();
            let mut response = Response::new();
            spa_handler
                .handle(
                    &mut request,
                    &mut Depot::new(),
                    &mut response,
                    &mut FlowCtrl::new(Vec::new()),
                )
                .await;
            assert_eq!(
                response.status_code.unwrap_or(StatusCode::OK),
                StatusCode::OK
            );
            let expected = if path == "/" || path == "/channels" {
                index.clone()
            } else {
                std::fs::read(static_dir.join(path)).unwrap()
            };
            let salvo::http::ResBody::Once(body) = response.take_body() else {
                panic!("the static response must contain the exact generated artifact");
            };
            assert_eq!(body.as_ref(), expected.as_slice());
            assert!(response.headers().contains_key("content-type"));
        }
    }
}
