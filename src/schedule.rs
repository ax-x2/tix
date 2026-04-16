use chrono::{DateTime, LocalResult, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;
use std::fmt::Write as FmtWrite;

use crate::types::{AlarmSpec, AppResult, TimeNotation};

pub fn resolve_alarm_with_now(
    spec: AlarmSpec,
    timezone: Tz,
    now: DateTime<Tz>,
) -> AppResult<DateTime<Tz>> {
    match spec {
        AlarmSpec::Duration(duration) => {
            let delta = chrono::TimeDelta::from_std(duration)
                .map_err(|_| "duration is too large to schedule".to_string())?;
            Ok(now + delta)
        }
        AlarmSpec::Explicit(datetime) => {
            let resolved = datetime.with_timezone(&timezone);
            if resolved <= now {
                return Err(format!(
                    "alarm time {} is already in the past",
                    resolved.to_rfc3339()
                ));
            }
            Ok(resolved)
        }
        AlarmSpec::Absolute(datetime) => resolve_local_datetime(datetime, timezone, now),
        AlarmSpec::TimeOfDay(time) => resolve_time_of_day(time, timezone, now),
    }
}

pub fn format_alarm_time(datetime: DateTime<Tz>, notation: TimeNotation) -> String {
    let mut rendered = String::with_capacity(32);
    write_alarm_time(datetime, notation, &mut rendered);
    rendered
}

pub fn write_alarm_time(datetime: DateTime<Tz>, notation: TimeNotation, out: &mut String) {
    match notation {
        TimeNotation::H24 => {
            let _ = write!(out, "{}", datetime.format("%Y-%m-%d %H:%M:%S %Z"));
        }
        TimeNotation::H12 => {
            let _ = write!(out, "{}", datetime.format("%Y-%m-%d %I:%M:%S %p %Z"));
        }
    }
}

fn resolve_local_datetime(
    datetime: NaiveDateTime,
    timezone: Tz,
    now: DateTime<Tz>,
) -> AppResult<DateTime<Tz>> {
    let resolved = match timezone.from_local_datetime(&datetime) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(_, _) => {
            return Err(format!(
                "local time {} is ambiguous in timezone {} due to DST; use a more explicit time",
                datetime, timezone
            ));
        }
        LocalResult::None => {
            return Err(format!(
                "local time {} does not exist in timezone {} due to DST",
                datetime, timezone
            ));
        }
    };

    if resolved <= now {
        return Err(format!(
            "alarm time {} is already in the past",
            resolved.to_rfc3339()
        ));
    }

    Ok(resolved)
}

fn resolve_time_of_day(
    time: NaiveTime,
    timezone: Tz,
    now: DateTime<Tz>,
) -> AppResult<DateTime<Tz>> {
    let today = now.date_naive();
    let mut last_invalid_local_time = None;

    for offset_days in [0_i64, 1_i64] {
        let date = today
            .checked_add_signed(chrono::TimeDelta::days(offset_days))
            .ok_or_else(|| "failed to compute alarm date".to_string())?;
        let datetime = date.and_time(time);

        let resolved = match timezone.from_local_datetime(&datetime) {
            LocalResult::Single(value) => value,
            LocalResult::Ambiguous(_, _) => {
                last_invalid_local_time = Some(format!(
                    "local time {} is ambiguous in timezone {} due to DST; use a full date/time",
                    datetime, timezone
                ));
                continue;
            }
            LocalResult::None => {
                last_invalid_local_time = Some(format!(
                    "local time {} does not exist in timezone {} due to DST",
                    datetime, timezone
                ));
                continue;
            }
        };

        if resolved > now {
            return Ok(resolved);
        }
    }

    Err(last_invalid_local_time.unwrap_or_else(|| "failed to schedule time-only alarm".to_string()))
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDateTime, NaiveTime, TimeZone};

    use super::*;
    use crate::parse::parse_alarm_spec;
    use crate::types::{DateOrder, DateParseConfig};

    fn parse_config(order: DateOrder) -> DateParseConfig {
        DateParseConfig {
            fallback_order: order,
            prefer_locale_order: false,
            locale_order: None,
        }
    }

    #[test]
    fn time_only_future_stays_today() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 3, 12, 12, 0, 0)
            .single()
            .unwrap();
        let spec = parse_alarm_spec("13:30", parse_config(DateOrder::Dmy)).unwrap();

        let resolved = resolve_alarm_with_now(spec, timezone, now).unwrap();

        assert_eq!(
            resolved,
            timezone
                .with_ymd_and_hms(2026, 3, 12, 13, 30, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn time_only_past_rolls_to_tomorrow() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 3, 12, 14, 0, 0)
            .single()
            .unwrap();
        let spec = parse_alarm_spec("13:30", parse_config(DateOrder::Dmy)).unwrap();

        let resolved = resolve_alarm_with_now(spec, timezone, now).unwrap();

        assert_eq!(
            resolved,
            timezone
                .with_ymd_and_hms(2026, 3, 13, 13, 30, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn ambiguous_dst_local_datetime_is_rejected() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 10, 24, 12, 0, 0)
            .single()
            .unwrap();
        let spec = parse_alarm_spec("2026-10-25 02:30", parse_config(DateOrder::Dmy)).unwrap();

        let result = resolve_alarm_with_now(spec, timezone, now);

        assert!(result.is_err());
    }

    #[test]
    fn nonexistent_time_today_rolls_to_valid_tomorrow() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 3, 29, 12, 0, 0)
            .single()
            .unwrap();
        let spec = parse_alarm_spec("02:30", parse_config(DateOrder::Dmy)).unwrap();

        let resolved = resolve_alarm_with_now(spec, timezone, now).unwrap();

        assert_eq!(
            resolved,
            timezone
                .with_ymd_and_hms(2026, 3, 30, 2, 30, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn ambiguous_time_today_rolls_to_valid_tomorrow() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 10, 25, 12, 0, 0)
            .single()
            .unwrap();
        let spec = parse_alarm_spec("02:30", parse_config(DateOrder::Dmy)).unwrap();

        let resolved = resolve_alarm_with_now(spec, timezone, now).unwrap();

        assert_eq!(
            resolved,
            timezone
                .with_ymd_and_hms(2026, 10, 26, 2, 30, 0)
                .single()
                .unwrap()
        );
    }

    #[test]
    fn explicit_local_absolute_value_round_trips() {
        let timezone: Tz = "Europe/Berlin".parse().unwrap();
        let now = timezone
            .with_ymd_and_hms(2026, 3, 11, 12, 0, 0)
            .single()
            .unwrap();
        let spec = crate::types::AlarmSpec::Absolute(
            NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap(),
        );

        let resolved = resolve_alarm_with_now(spec, timezone, now).unwrap();

        assert_eq!(resolved.time(), NaiveTime::from_hms_opt(13, 30, 0).unwrap());
    }
}
