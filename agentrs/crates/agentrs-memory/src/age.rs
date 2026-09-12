use chrono::{DateTime, Utc};

/// Whole days elapsed since `mtime`. Future timestamps clamp to zero.
pub fn memory_age_days(mtime: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(mtime).num_days().max(0)
}

/// Render an age in a form that makes staleness apparent to a model.
pub fn memory_age(mtime: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match memory_age_days(mtime, now) {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        days => format!("{days} days ago"),
    }
}

/// Return a caveat for memories old enough to require revalidation.
pub fn memory_freshness_text(mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let days = memory_age_days(mtime, now);
    (days > 1).then(|| {
        format!(
            "This memory is {days} days old. Memories are point-in-time observations; verify it against the current code and project state before relying on it."
        )
    })
}

/// Wrap the freshness caveat for direct prompt injection.
pub fn memory_freshness_note(mtime: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    memory_freshness_text(mtime, now).map(|text| format!("<system-reminder>{text}</system-reminder>"))
}

#[cfg(test)]
#[path = "age_test.rs"]
mod age_test;
