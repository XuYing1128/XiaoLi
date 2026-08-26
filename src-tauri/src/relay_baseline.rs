use crate::relay_audit::RelayProtocol;
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, TimeZone,
    Utc,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayBaselineSummary {
    pub id: String,
    pub label: String,
    pub model: String,
    pub protocol: RelayProtocol,
    /// `official`, `community`, or `user`. These namespaces are stored in
    /// separate rows and relay observations can never overwrite them.
    pub source: String,
    pub version: String,
    pub sample_count: usize,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub signed: bool,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSchedule {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_baseline_profile_id: Option<String>,
    pub cadence: String,
    #[serde(default = "default_weekday")]
    pub weekday: u8,
    pub local_time: String,
    pub pair_official: bool,
    pub monthly_request_limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_month: Option<String>,
    #[serde(default)]
    pub monthly_reserved_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_audit_id: Option<String>,
}

const fn default_weekday() -> u8 {
    1
}

impl Default for AuditSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_id: None,
            official_baseline_profile_id: None,
            cadence: "weekly".to_owned(),
            weekday: default_weekday(),
            local_time: "20:00".to_owned(),
            pair_official: false,
            monthly_request_limit: 1_000,
            history_retention_days: Some(180),
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            budget_month: None,
            monthly_reserved_requests: 0,
            active_audit_id: None,
        }
    }
}

impl AuditSchedule {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.cadence.as_str(), "daily" | "weekly") {
            return Err("cadence must be daily or weekly".to_owned());
        }
        if self.weekday > 6 {
            return Err("weekday must be between 0 and 6".to_owned());
        }
        let Some((hour, minute)) = self.local_time.split_once(':') else {
            return Err("localTime must be HH:MM".to_owned());
        };
        let hour = hour.parse::<u8>().map_err(|_| "invalid localTime")?;
        let minute = minute.parse::<u8>().map_err(|_| "invalid localTime")?;
        if hour > 23 || minute > 59 {
            return Err("localTime must be HH:MM".to_owned());
        }
        let minimum_budget = if self.pair_official { 300 } else { 150 };
        if self.monthly_request_limit < minimum_budget || self.monthly_request_limit > 100_000 {
            return Err("monthlyRequestLimit is outside the supported range".to_owned());
        }
        if self.enabled {
            let Some(profile_id) = self.profile_id.as_deref() else {
                return Err("enabled schedules require profileId".to_owned());
            };
            validate_id(profile_id, "profileId")?;
            if self.pair_official {
                let Some(official_id) = self.official_baseline_profile_id.as_deref() else {
                    return Err(
                        "paired scheduled audits require officialBaselineProfileId".to_owned()
                    );
                };
                validate_id(official_id, "officialBaselineProfileId")?;
                if official_id == profile_id {
                    return Err("paired endpoints must be different profiles".to_owned());
                }
            }
        }
        if self
            .history_retention_days
            .is_some_and(|days| !(1..=36_500).contains(&days))
        {
            return Err("historyRetentionDays is outside the supported range".to_owned());
        }
        Ok(())
    }

    pub const fn request_reservation(&self) -> u32 {
        if self.pair_official {
            300
        } else {
            150
        }
    }

    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.enabled
            && self
                .next_run_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|value| value.with_timezone(&Utc) <= now)
    }
}

pub fn current_budget_month(now: DateTime<Utc>) -> String {
    now.with_timezone(&Local).format("%Y-%m").to_string()
}

/// Computes one randomized local due time. The random offset is generated by
/// the operating system and deliberately persisted by the caller so process
/// restarts do not continuously reroll the schedule.
pub fn next_scheduled_run(schedule: &AuditSchedule, now: DateTime<Utc>) -> Result<String, String> {
    schedule.validate()?;
    let jitter = random_jitter_minutes()?;
    let local_now = now.with_timezone(&Local);
    let candidate = next_local_naive(schedule, local_now.naive_local(), jitter)?;
    let local_candidate = resolve_local_datetime(candidate)?;
    Ok(local_candidate
        .with_timezone(&Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn random_jitter_minutes() -> Result<i64, String> {
    let mut bytes = [0_u8; 2];
    getrandom::fill(&mut bytes)
        .map_err(|_| "operating-system random source is unavailable".to_owned())?;
    Ok(i64::from(u16::from_le_bytes(bytes) % 61) - 30)
}

fn next_local_naive(
    schedule: &AuditSchedule,
    now: NaiveDateTime,
    jitter_minutes: i64,
) -> Result<NaiveDateTime, String> {
    let (hour, minute) = parse_local_time(&schedule.local_time)?;
    let mut days_ahead = if schedule.cadence == "daily" {
        0_i64
    } else {
        let current = i64::from(now.weekday().num_days_from_sunday());
        (i64::from(schedule.weekday) - current).rem_euclid(7)
    };
    let mut base = now
        .date()
        .and_hms_opt(u32::from(hour), u32::from(minute), 0)
        .ok_or_else(|| "localTime could not be represented".to_owned())?
        + ChronoDuration::days(days_ahead);
    if base <= now {
        days_ahead = if schedule.cadence == "daily" { 1 } else { 7 };
        base += ChronoDuration::days(days_ahead);
    }
    let mut candidate = base + ChronoDuration::minutes(jitter_minutes.clamp(-30, 30));
    if candidate <= now {
        candidate += ChronoDuration::days(if schedule.cadence == "daily" { 1 } else { 7 });
    }
    Ok(candidate)
}

fn parse_local_time(value: &str) -> Result<(u8, u8), String> {
    let Some((hour, minute)) = value.split_once(':') else {
        return Err("localTime must be HH:MM".to_owned());
    };
    let hour = hour.parse::<u8>().map_err(|_| "invalid localTime")?;
    let minute = minute.parse::<u8>().map_err(|_| "invalid localTime")?;
    if hour > 23 || minute > 59 {
        return Err("localTime must be HH:MM".to_owned());
    }
    Ok((hour, minute))
}

fn resolve_local_datetime(mut candidate: NaiveDateTime) -> Result<DateTime<Local>, String> {
    for _ in 0..=3 {
        match Local.from_local_datetime(&candidate) {
            LocalResult::Single(value) => return Ok(value),
            LocalResult::Ambiguous(earlier, _) => return Ok(earlier),
            LocalResult::None => candidate += ChronoDuration::hours(1),
        }
    }
    Err("scheduled local time is unavailable around a clock transition".to_owned())
}

fn validate_id(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("{field} is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_is_off_by_default_and_validates_budget() {
        let schedule = AuditSchedule::default();
        assert!(!schedule.enabled);
        assert!(schedule.validate().is_ok());
        let mut invalid = schedule;
        invalid.monthly_request_limit = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn enabled_schedule_requires_bound_profiles_and_pair_budget() {
        let mut schedule = AuditSchedule {
            enabled: true,
            profile_id: Some("relay-one".to_owned()),
            ..AuditSchedule::default()
        };
        assert!(schedule.validate().is_ok());
        schedule.pair_official = true;
        assert!(schedule.validate().is_err());
        schedule.official_baseline_profile_id = Some("official-one".to_owned());
        schedule.monthly_request_limit = 299;
        assert!(schedule.validate().is_err());
        schedule.monthly_request_limit = 300;
        assert!(schedule.validate().is_ok());
    }

    #[test]
    fn next_local_time_applies_bounded_jitter_and_rolls_forward() {
        let schedule = AuditSchedule {
            enabled: true,
            profile_id: Some("relay-one".to_owned()),
            cadence: "daily".to_owned(),
            local_time: "20:00".to_owned(),
            ..AuditSchedule::default()
        };
        let now =
            NaiveDateTime::parse_from_str("2026-08-27 19:50:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let early = next_local_naive(&schedule, now, -30).unwrap();
        assert_eq!(early.date().to_string(), "2026-08-28");
        assert_eq!(early.time().format("%H:%M").to_string(), "19:30");
        let late = next_local_naive(&schedule, now, 30).unwrap();
        assert_eq!(late.date().to_string(), "2026-08-27");
        assert_eq!(late.time().format("%H:%M").to_string(), "20:30");
    }
}
