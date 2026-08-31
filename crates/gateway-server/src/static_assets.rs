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
mod frontend_smoke {
    use super::*;

    #[tokio::test]
    async fn frontend_static_smoke_serves_current_built_assets() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
        let index = std::fs::read(root.join("index.html")).expect("build-web must run first");
        let acceptor = TcpListener::new("127.0.0.1:0").bind().await;
        let address = acceptor.local_addr().unwrap();
        let router = Router::new()
            .push(Router::with_path("{**rest}").get(spa_handler))
            .push(Router::with_path("").get(spa_handler));
        let server = tokio::spawn(Server::new(acceptor).serve(router));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        for route in ["/", "/channels"] {
            let response = client
                .get(format!("http://{address}{route}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            assert!(
                response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .contains("text/html")
            );
            assert_eq!(response.bytes().await.unwrap().as_ref(), index.as_slice());
        }
        let index_text = std::str::from_utf8(&index).unwrap();
        let asset = regex::Regex::new(r#"<script[^>]+src="([^"]+\.js)""#)
            .unwrap()
            .captures(index_text)
            .expect("built HTML must reference a JavaScript asset")[1]
            .trim_start_matches('/')
            .to_owned();
        let wasm = format!("{}_bg.wasm", asset.strip_suffix(".js").unwrap());
        for asset in [asset, wasm] {
            let expected = std::fs::read(root.join(&asset)).unwrap();
            let response = client
                .get(format!("http://{address}/{asset}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            if asset.ends_with(".wasm") {
                assert_eq!(response.headers()["content-type"], "application/wasm");
            }
            assert_eq!(response.bytes().await.unwrap().as_ref(), expected.as_slice());
        }
        server.abort();
        let _ = server.await;
    }
}
