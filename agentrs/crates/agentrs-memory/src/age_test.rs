use chrono::{Duration, TimeZone, Utc};
use rstest::rstest;

use super::{memory_age, memory_age_days, memory_freshness_note, memory_freshness_text};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 12, 12, 0, 0).unwrap()
}

#[rstest]
#[case(0, "today")]
#[case(1, "yesterday")]
#[case(2, "2 days ago")]
#[case(47, "47 days ago")]
fn relative_age_is_human_readable(#[case] days: i64, #[case] expected: &str) {
    assert_eq!(memory_age(now() - Duration::days(days), now()), expected);
}

#[test]
fn future_timestamps_clamp_to_today() {
    let future = now() + Duration::days(3);
    assert_eq!(memory_age_days(future, now()), 0);
    assert_eq!(memory_age(future, now()), "today");
}

#[test]
fn freshness_warning_starts_after_one_day() {
    assert!(memory_freshness_text(now(), now()).is_none());
    assert!(memory_freshness_text(now() - Duration::days(1), now()).is_none());
    let warning = memory_freshness_note(now() - Duration::days(2), now()).unwrap();
    assert!(warning.starts_with("<system-reminder>"));
    assert!(warning.contains("2 days old"));
}
