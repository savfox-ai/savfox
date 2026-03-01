#![allow(missing_debug_implementations)]

mod default_client;
mod error;
mod request;
mod retry;
mod sse;
mod telemetry;
mod transport;

pub use crate::default_client::{SavfoxHttpClient, SavfoxRequestBuilder};
pub use crate::error::{StreamError, TransportError};
pub use crate::request::{Request, RequestCompression, Response};
pub use crate::retry::{RetryOn, RetryPolicy, backoff, run_with_retry};
pub use crate::sse::sse_stream;
pub use crate::telemetry::RequestTelemetry;
pub use crate::transport::{ByteStream, HttpTransport, ReqwestTransport, StreamResponse};
