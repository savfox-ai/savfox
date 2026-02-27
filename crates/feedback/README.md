# savfox-feedback

Feedback collection and upload library for Savfox. This crate captures diagnostic logs into a bounded ring buffer (default 4 MiB) during a session and provides the ability to upload feedback reports to Sentry.

The `SavfoxFeedback` struct exposes `tracing_subscriber` layers for integration into the application's logging pipeline. The `logger_layer` captures full-fidelity logs at TRACE level regardless of the user's `RUST_LOG` setting, while the `metadata_layer` collects structured key/value tags emitted to the `feedback_tags` tracing target. When the user submits feedback, a snapshot of the buffered logs and tags is assembled into a Sentry envelope with optional file attachments (logs and rollout data) and uploaded with a configurable classification (bug, bad result, good result).
