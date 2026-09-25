//! Local times for people. The daemon reports RFC 3339 UTC timestamps.

use jiff::Timestamp;
use jiff::tz::TimeZone;

fn local(ts: &str) -> Option<jiff::Zoned> {
    let ts: Timestamp = ts.parse().ok()?;
    Some(ts.to_zoned(TimeZone::system()))
}

/// "14:32" today, "Sep 24, 14:32" on other days.
pub fn short(ts: &str) -> String {
    short_at(ts, &jiff::Zoned::now())
}

fn short_at(ts: &str, now: &jiff::Zoned) -> String {
    let Some(at) = local(ts) else {
        return ts.to_string();
    };
    let at = at.with_time_zone(now.time_zone().clone());
    if at.date() == now.date() {
        at.strftime("%H:%M").to_string()
    } else if at.year() == now.year() {
        at.strftime("%b %-d, %H:%M").to_string()
    } else {
        at.strftime("%b %-d %Y, %H:%M").to_string()
    }
}

/// The date heading of an activity log group: "Today", "Yesterday", or
/// "Wednesday, Sep 23".
pub fn day(ts: &str) -> String {
    day_at(ts, &jiff::Zoned::now())
}

fn day_at(ts: &str, now: &jiff::Zoned) -> String {
    let Some(at) = local(ts) else {
        return String::new();
    };
    let at = at.with_time_zone(now.time_zone().clone());
    let today = now.date();
    if at.date() == today {
        "Today".into()
    } else if today.yesterday().ok() == Some(at.date()) {
        "Yesterday".into()
    } else if at.year() == now.year() {
        at.strftime("%A, %b %-d").to_string()
    } else {
        at.strftime("%A, %b %-d %Y").to_string()
    }
}

/// "14:32:05", for rows under a day heading.
pub fn clock(ts: &str) -> String {
    local(ts).map_or_else(|| ts.to_string(), |t| t.strftime("%H:%M:%S").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> jiff::Zoned {
        "2026-09-25T10:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(TimeZone::UTC)
    }

    #[test]
    fn short_times_depend_on_the_day() {
        let now = now();
        assert_eq!(short_at("2026-09-25T09:12:03Z", &now), "09:12");
        assert_eq!(short_at("2026-09-24T09:12:03Z", &now), "Sep 24, 09:12");
        assert_eq!(short_at("2025-01-02T09:12:03Z", &now), "Jan 2 2025, 09:12");
        assert_eq!(short_at("garbage", &now), "garbage");
    }

    #[test]
    fn days_are_named() {
        let now = now();
        assert_eq!(day_at("2026-09-25T01:00:00Z", &now), "Today");
        assert_eq!(day_at("2026-09-24T23:00:00Z", &now), "Yesterday");
        assert_eq!(day_at("2026-09-21T12:00:00Z", &now), "Monday, Sep 21");
    }
}
