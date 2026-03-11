use chrono::{TimeZone, Utc};

pub fn format_relative_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "N/A".to_string();
    }

    let dt = match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt,
        _ => return "Invalid".to_string(),
    };

    let now = Utc::now();
    let diff = now.signed_duration_since(dt);

    let total_secs = diff.num_seconds();
    if total_secs < 0 {
        return "刚刚".to_string();
    }

    if total_secs < 60 {
        "刚刚".to_string()
    } else if total_secs < 3600 {
        format!("{} 分钟前", total_secs / 60)
    } else if total_secs < 86400 {
        format!("{} 小时前", total_secs / 3600)
    } else if total_secs < 604800 {
        format!("{} 天前", total_secs / 86400)
    } else if total_secs < 2592000 {
        format!("{} 周前", total_secs / 604800)
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}

pub fn format_duration_human(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 24 {
        let days = hours / 24;
        let remain_hours = hours % 24;
        if remain_hours > 0 {
            format!("{}d {}h", days, remain_hours)
        } else {
            format!("{}d", days)
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{}h {}m", hours, mins)
        } else {
            format!("{}h", hours)
        }
    } else if mins > 0 {
        if secs > 0 {
            format!("{}m {}s", mins, secs)
        } else {
            format!("{}m", mins)
        }
    } else {
        format!("{}s", secs)
    }
}

pub fn format_duration_precise(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    let remain_ms = ms % 1000;
    format!("{:02}:{:02}.{:03}", mins, secs, remain_ms)
}

pub fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "N/A".to_string();
    }

    match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        _ => "Invalid".to_string(),
    }
}

pub fn format_timestamp_short(ts: i64) -> String {
    if ts <= 0 {
        return "N/A".to_string();
    }

    match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%m/%d %H:%M").to_string(),
        _ => "Invalid".to_string(),
    }
}

pub fn format_date(ts: i64) -> String {
    if ts <= 0 {
        return "N/A".to_string();
    }

    match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d").to_string(),
        _ => "Invalid".to_string(),
    }
}

pub fn format_time(ts: i64) -> String {
    if ts <= 0 {
        return "N/A".to_string();
    }

    match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%H:%M:%S").to_string(),
        _ => "Invalid".to_string(),
    }
}

pub fn now_ts() -> i64 {
    Utc::now().timestamp()
}

pub fn now_ts_ms() -> i64 {
    Utc::now().timestamp_millis()
}
