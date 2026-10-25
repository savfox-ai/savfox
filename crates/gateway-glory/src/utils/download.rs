use wasm_bindgen::JsCast;

/// Trigger a browser file download with the given filename, content string, and MIME type.
///
/// Uses `web_sys::Blob` + `Url::create_object_url_with_blob` and a temporary `<a>` element
/// to initiate the download without any server round-trip.
pub fn trigger_download(filename: &str, content: &str, mime_type: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let blob_opts = web_sys::BlobPropertyBag::new();
    blob_opts.set_type(mime_type);
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(
        &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(content)),
        &blob_opts,
    ) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let Some(doc) = window.document() else {
        let _ = web_sys::Url::revoke_object_url(&url);
        return;
    };
    let Ok(a) = doc.create_element("a") else {
        let _ = web_sys::Url::revoke_object_url(&url);
        return;
    };
    let _ = a.set_attribute("href", &url);
    let _ = a.set_attribute("download", filename);
    let _ = a.set_attribute("style", "display:none");
    if let Some(body) = doc.body() {
        let _ = body.append_child(&a);
        if let Some(el) = a.dyn_ref::<web_sys::HtmlElement>() {
            el.click();
        }
        let _ = body.remove_child(&a);
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}
