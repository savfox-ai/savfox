use std::path::Path;

use chrono::{DateTime, Timelike, Utc};

pub(crate) async fn file_modified_time_utc(path: &Path) -> Option<DateTime<Utc>> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    let updated_at: DateTime<Utc> = modified.into();
    Some(updated_at.with_nanosecond(0).unwrap_or(updated_at))
}
