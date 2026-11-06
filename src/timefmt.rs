//! Time formatting. Files and logs hold UTC; event lines add a local time.
//!
//! The local time zone and its label come from `CQ_TZ` and `CQ_TZ_LABEL`,
//! else `tz` and `tz_label` in the root's `config.toml`, else the build's
//! `CQ_DEFAULT_TZ` and `CQ_DEFAULT_TZ_LABEL` (see `config`), not from the
//! shell, because Git Bash on Windows can report the local time zone wrongly.

use std::sync::OnceLock;
use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::config;

pub fn now() -> Timestamp {
    Timestamp::now()
}

/// RFC 3339 in UTC with `Z` and whole seconds: `2026-10-24T00:06:15Z`.
pub fn rfc3339(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The ID prefix: `20261024T000615Z`.
pub fn id_stamp(ts: Timestamp) -> String {
    ts.strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// Parse an RFC 3339 UTC time that ends in `Z`.
pub fn parse_rfc3339_z(s: &str) -> Option<Timestamp> {
    if !s.ends_with('Z') {
        return None;
    }
    s.parse().ok()
}

/// Parse the `yyyymmddThhmmssZ` prefix of an ID.
pub fn parse_id_stamp(s: &str) -> Option<Timestamp> {
    let b = s.as_bytes();
    if b.len() != 16 {
        return None;
    }
    let text = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[9..11],
        &s[11..13],
        &s[13..15]
    );
    parse_rfc3339_z(&text)
}

pub fn from_system_time(t: SystemTime) -> Option<Timestamp> {
    Timestamp::try_from(t).ok()
}

pub const ENV_TZ: &str = "CQ_TZ";
pub const ENV_TZ_LABEL: &str = "CQ_TZ_LABEL";
const BUILD_TZ: Option<&str> = option_env!("CQ_DEFAULT_TZ");
const BUILD_TZ_LABEL: Option<&str> = option_env!("CQ_DEFAULT_TZ_LABEL");

/// The zone and label for the local time, read once per process.
struct Local {
    tz: TimeZone,
    /// `None` uses the zone's abbreviation at each instant (GMT, BST, CET).
    label: Option<String>,
}

/// Read once, so `config::install` must come first.
fn local() -> &'static Local {
    static LOCAL: OnceLock<Local> = OnceLock::new();
    LOCAL.get_or_init(|| {
        let file = config::installed();
        // An unknown run-time name falls back to the file's zone, then the
        // build's, then the system zone.
        let tz = config::candidates(ENV_TZ, file.tz.as_deref(), BUILD_TZ)
            .into_iter()
            .find_map(|name| TimeZone::get(&name).ok())
            .unwrap_or_else(TimeZone::system);
        let label = config::candidates(ENV_TZ_LABEL, file.tz_label.as_deref(), BUILD_TZ_LABEL)
            .into_iter()
            .next();
        Local { tz, label }
    })
}

/// The local time zone, as for `local_hhmm`.
pub fn local_zone() -> TimeZone {
    local().tz.clone()
}

/// `HH:MM <label>` in the local time zone: `09:55 UK` with the owner's
/// build, else for example `09:55 GMT`.
pub fn local_hhmm(ts: Timestamp) -> String {
    let l = local();
    hhmm_label(ts, &l.tz, l.label.as_deref())
}

/// The local zone and label, for `--help`.
pub fn local_description() -> String {
    let l = local();
    let zone = l.tz.iana_name().unwrap_or("the system time zone");
    match &l.label {
        Some(label) => format!("{zone}, labelled {label}"),
        None => format!("{zone}, labelled with its abbreviation"),
    }
}

fn hhmm_label(ts: Timestamp, tz: &TimeZone, label: Option<&str>) -> String {
    let zoned = ts.to_zoned(tz.clone());
    let hhmm = zoned.strftime("%H:%M");
    match label {
        Some(label) => format!("{hhmm} {label}"),
        None => {
            let info = tz.to_offset_info(ts);
            match info.abbreviation() {
                "" => format!("{hhmm} {}", zoned.strftime("%:z")),
                abbr => format!("{hhmm} {abbr}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn london_time_follows_bst_and_gmt() {
        let tz = TimeZone::get("Europe/London").unwrap();
        let uk = |s: &str| hhmm_label(ts(s), &tz, Some("UK"));
        // BST (UTC+1) until 01:00 UTC on the last Sunday of October 2026.
        assert_eq!(uk("2026-10-24T00:06:16Z"), "01:06 UK");
        assert_eq!(uk("2026-10-25T00:59:00Z"), "01:59 UK");
        // GMT from then on.
        assert_eq!(uk("2026-10-25T01:00:00Z"), "01:00 UK");
        assert_eq!(uk("2026-12-01T12:00:00Z"), "12:00 UK");
        // BST starts at 01:00 UTC on the last Sunday of March 2027.
        assert_eq!(uk("2027-03-28T00:59:00Z"), "00:59 UK");
        assert_eq!(uk("2027-03-28T01:00:00Z"), "02:00 UK");
    }

    #[test]
    fn label_defaults_to_the_abbreviation() {
        let london = TimeZone::get("Europe/London").unwrap();
        assert_eq!(
            hhmm_label(ts("2026-10-24T00:06:16Z"), &london, None),
            "01:06 BST"
        );
        assert_eq!(
            hhmm_label(ts("2026-12-01T12:00:00Z"), &london, None),
            "12:00 GMT"
        );
        let paris = TimeZone::get("Europe/Paris").unwrap();
        assert_eq!(
            hhmm_label(ts("2026-12-01T12:00:00Z"), &paris, None),
            "13:00 CET"
        );
    }

    #[test]
    fn formats_round_trip() {
        let t = ts("2026-10-24T00:06:15Z");
        assert_eq!(rfc3339(t), "2026-10-24T00:06:15Z");
        assert_eq!(id_stamp(t), "20261024T000615Z");
        assert_eq!(parse_id_stamp("20261024T000615Z"), Some(t));
        assert_eq!(parse_rfc3339_z("2026-10-24T00:06:15Z"), Some(t));
        assert_eq!(parse_rfc3339_z("2026-10-24T00:06:15+00:00"), None);
    }
}
