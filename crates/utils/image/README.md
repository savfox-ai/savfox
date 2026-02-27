# savfox-utils-image

Handles loading, resizing, and encoding images for upload to model APIs. The main entry point, `load_and_resize_to_fit`, reads an image file from disk, ensures it fits within a 2048x768 bounding box (downscaling with a triangle filter if necessary), and returns an `EncodedImage` containing the raw bytes, MIME type, and dimensions. PNG and JPEG formats are supported; unrecognized formats are re-encoded as PNG.

Results are cached in a global `BlockingLruCache` keyed by the SHA-1 digest of the file contents, so repeated reads of the same file skip decoding and encoding. The cache holds up to 32 entries. File I/O is Tokio-aware: inside a runtime it uses `block_in_place` to avoid blocking worker threads, and outside a runtime it falls back to synchronous reads.

`EncodedImage` also provides `into_data_url()` for converting the encoded bytes into a base64 `data:` URI suitable for inline embedding.
