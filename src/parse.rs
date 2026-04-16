use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

use crate::config::{parse_bool, parse_notification_timeout_ms, parse_volume};
use crate::types::{
    AlarmNotificationConfig, AlarmSpec, AppResult, Command, DateOrder, DateParseConfig, RunMode,
};

pub const HELP: &str = "\
tix - clock alarm timer

Usage:
  tix [--dry-run] [-f|--foreground] [-b|--background] [--timezone <IANA-TZ>] <when>
  tix status
  tix stop [<id>|--all]
  tix volume
  tix volume <0.0..=1.0>
  tix volume set <0.0..=1.0>
  tix volume test [<0.0..=1.0>]
  tix config init
  tix config show
  tix config path
  tix config set <key> <value>

Examples:
  tix 10m
  tix -f 10m
  tix -b 10m
  tix 13:30
  tix 01:30pm
  tix 12/31/2026 8:15pm
  tix 12.03.2026 13:30
  tix status
  tix stop 17fd2a
  tix stop --all
  tix volume
  tix volume 0.35
  tix volume test

Notes:
  - CLI mode flags override the configured default mode.
  - slash dates accept unambiguous inputs in any common order.
  - ambiguous slash dates follow the locale-aware config policy.
  - time-only alarms schedule today if still in the future, otherwise tomorrow.
  - multiple background alarms can be active at the same time.
";

pub fn parse_command<I>(args: I) -> AppResult<Command>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter().peekable();
    let Some(first) = args.next() else {
        return Ok(Command::Help);
    };

    if first == "--help" || first == "-h" {
        return Ok(Command::Help);
    }

    match first.as_str() {
        "config" => return parse_config_command(args),
        "status" => return Ok(Command::Status),
        "stop" => return parse_stop_command(args),
        "volume" => return parse_volume_command(args),
        "__worker" => return parse_worker_command(args),
        _ => {}
    }

    let mut dry_run = false;
    let mut mode_override = None;
    let mut timezone_override = None;
    let mut parts = Vec::new();

    push_alarm_arg(
        first,
        &mut args,
        &mut dry_run,
        &mut mode_override,
        &mut timezone_override,
        &mut parts,
    )?;

    while let Some(arg) = args.next() {
        push_alarm_arg(
            arg,
            &mut args,
            &mut dry_run,
            &mut mode_override,
            &mut timezone_override,
            &mut parts,
        )?;
    }

    if parts.is_empty() {
        return Err("missing alarm input\n\n".to_owned() + HELP);
    }

    Ok(Command::Alarm {
        spec_text: parts.join(" "),
        dry_run,
        timezone_override,
        mode_override,
    })
}

pub fn parse_alarm_spec(input: &str, date_config: DateParseConfig) -> AppResult<AlarmSpec> {
    let normalized = normalize_spec_text(input);
    if normalized.is_empty() {
        return Err("empty alarm input".to_string());
    }

    let duration_input = normalized
        .strip_prefix("in ")
        .map(str::trim)
        .unwrap_or(&normalized);
    if let Ok(duration) = humantime::parse_duration(duration_input) {
        return Ok(AlarmSpec::Duration(duration));
    }

    if let Ok(datetime) = DateTime::parse_from_rfc3339(&normalized) {
        return Ok(AlarmSpec::Explicit(datetime));
    }

    if let Some(time) = parse_time_only(&normalized) {
        return Ok(AlarmSpec::TimeOfDay(time));
    }

    if let Some(datetime) = parse_local_datetime(&normalized, date_config)? {
        return Ok(AlarmSpec::Absolute(datetime));
    }

    Err(format!(
        "unsupported time format `{normalized}`\n\
supported examples: `10m`, `1h30m`, `13:30`, `01:30pm`, `12/31/2026 8:15pm`, `12.03.2026 13:30`, `2026-03-12 13:30`"
    ))
}

fn push_alarm_arg<I>(
    arg: String,
    args: &mut std::iter::Peekable<I>,
    dry_run: &mut bool,
    mode_override: &mut Option<RunMode>,
    timezone_override: &mut Option<String>,
    parts: &mut Vec<String>,
) -> AppResult<()>
where
    I: Iterator<Item = String>,
{
    match arg.as_str() {
        "--dry-run" => {
            *dry_run = true;
            Ok(())
        }
        "--foreground" | "-f" => set_mode_override(mode_override, RunMode::Foreground),
        "--background" | "-b" => set_mode_override(mode_override, RunMode::Background),
        "--timezone" => {
            let Some(value) = args.next() else {
                return Err("--timezone requires a value".to_string());
            };
            *timezone_override = Some(value);
            Ok(())
        }
        _ => {
            parts.push(arg);
            Ok(())
        }
    }
}

fn set_mode_override(current: &mut Option<RunMode>, new_mode: RunMode) -> AppResult<()> {
    match current {
        Some(existing) if *existing != new_mode => {
            Err("cannot pass both foreground and background mode flags".to_string())
        }
        _ => {
            *current = Some(new_mode);
            Ok(())
        }
    }
}

fn parse_config_command<I>(mut args: I) -> AppResult<Command>
where
    I: Iterator<Item = String>,
{
    match args.next().as_deref() {
        Some("init") => Ok(Command::ConfigInit),
        Some("show") => Ok(Command::ConfigShow),
        Some("path") => Ok(Command::ConfigPath),
        Some("set") => {
            let Some(key) = args.next() else {
                return Err("config set requires a key".to_string());
            };
            let Some(value) = args.next() else {
                return Err("config set requires a value".to_string());
            };
            if args.next().is_some() {
                return Err("config set accepts exactly one key and one value".to_string());
            }
            Ok(Command::ConfigSet { key, value })
        }
        _ => Err("supported config commands: init, show, path, set".to_string()),
    }
}

fn parse_stop_command<I>(mut args: I) -> AppResult<Command>
where
    I: Iterator<Item = String>,
{
    match args.next() {
        None => Ok(Command::Stop {
            selector: None,
            all: false,
        }),
        Some(value) if value == "--all" || value == "all" => {
            if args.next().is_some() {
                return Err("stop --all does not accept additional arguments".to_string());
            }
            Ok(Command::Stop {
                selector: None,
                all: true,
            })
        }
        Some(selector) => {
            if args.next().is_some() {
                return Err("stop accepts at most one alarm id selector".to_string());
            }
            Ok(Command::Stop {
                selector: Some(selector),
                all: false,
            })
        }
    }
}

fn parse_volume_command<I>(mut args: I) -> AppResult<Command>
where
    I: Iterator<Item = String>,
{
    match args.next().as_deref() {
        None | Some("show") => Ok(Command::VolumeShow),
        Some("set") => {
            let Some(value) = args.next() else {
                return Err("volume set requires a numeric value".to_string());
            };
            if args.next().is_some() {
                return Err("volume set accepts exactly one numeric value".to_string());
            }
            Ok(Command::VolumeSet {
                volume: parse_volume(&value)?,
            })
        }
        Some("test") => {
            let volume_override = match args.next() {
                Some(value) => Some(parse_volume(&value)?),
                None => None,
            };
            if args.next().is_some() {
                return Err("volume test accepts at most one numeric override".to_string());
            }
            Ok(Command::VolumeTest { volume_override })
        }
        Some(value) => {
            if args.next().is_some() {
                return Err("volume command accepts at most one positional value".to_string());
            }
            Ok(Command::VolumeSet {
                volume: parse_volume(value)?,
            })
        }
    }
}

fn parse_worker_command<I>(mut args: I) -> AppResult<Command>
where
    I: Iterator<Item = String>,
{
    let mut alarm_id = None;
    let mut target_utc = None;
    let mut auto_stop_seconds = 0_u64;
    let mut volume = None;
    let mut sound_file = None;
    let mut notifications_enabled = true;
    let mut notifications_clickable = true;
    let mut notifications_timeout_ms = 0_u32;
    let mut notifications_show_stop_button = true;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--alarm-id" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --alarm-id <id>".to_string());
                };
                alarm_id = Some(value);
            }
            "--target-utc" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --target-utc <rfc3339>".to_string());
                };
                let parsed = DateTime::parse_from_rfc3339(&value)
                    .map_err(|err| format!("invalid worker target time `{value}`: {err}"))?
                    .with_timezone(&Utc);
                target_utc = Some(parsed);
            }
            "--auto-stop-seconds" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --auto-stop-seconds <u64>".to_string());
                };
                auto_stop_seconds = value.parse().map_err(|_| {
                    "worker auto-stop seconds must be an unsigned integer".to_string()
                })?;
            }
            "--volume" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --volume <0.0..=1.0>".to_string());
                };
                volume = Some(parse_volume(&value)? as f32);
            }
            "--sound-file" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --sound-file <path>".to_string());
                };
                sound_file = Some(value);
            }
            "--notifications-enabled" => {
                let Some(value) = args.next() else {
                    return Err(
                        "__worker requires --notifications-enabled <true|false>".to_string()
                    );
                };
                notifications_enabled = parse_bool(&value)?;
            }
            "--notifications-clickable" => {
                let Some(value) = args.next() else {
                    return Err(
                        "__worker requires --notifications-clickable <true|false>".to_string()
                    );
                };
                notifications_clickable = parse_bool(&value)?;
            }
            "--notifications-timeout-ms" => {
                let Some(value) = args.next() else {
                    return Err("__worker requires --notifications-timeout-ms <u32>".to_string());
                };
                notifications_timeout_ms = parse_notification_timeout_ms(&value)?;
            }
            "--notifications-show-stop-button" => {
                let Some(value) = args.next() else {
                    return Err(
                        "__worker requires --notifications-show-stop-button <true|false>"
                            .to_string(),
                    );
                };
                notifications_show_stop_button = parse_bool(&value)?;
            }
            _ => return Err(format!("unknown internal worker argument `{arg}`")),
        }
    }

    Ok(Command::Worker {
        alarm_id: alarm_id.ok_or_else(|| "__worker requires --alarm-id <id>".to_string())?,
        target_utc: target_utc
            .ok_or_else(|| "__worker requires --target-utc <rfc3339>".to_string())?,
        auto_stop_seconds,
        volume: volume.ok_or_else(|| "__worker requires --volume <0.0..=1.0>".to_string())?,
        sound_file,
        notifications: AlarmNotificationConfig {
            enabled: notifications_enabled,
            clickable: notifications_clickable,
            timeout_ms: notifications_timeout_ms,
            show_stop_button: notifications_show_stop_button,
        },
    })
}

fn normalize_spec_text(input: &str) -> String {
    let mut normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowercase = normalized.to_ascii_lowercase();
    if lowercase.ends_with("am") || lowercase.ends_with("pm") {
        let len = normalized.len();
        let suffix = normalized[len - 2..].to_ascii_uppercase();
        normalized.replace_range(len - 2.., &suffix);
    }
    normalized
}

fn parse_time_only(input: &str) -> Option<NaiveTime> {
    const FORMATS: &[&str] = &[
        "%H:%M",
        "%H:%M:%S",
        "%I:%M%p",
        "%I:%M %p",
        "%I:%M:%S%p",
        "%I:%M:%S %p",
    ];

    parse_first_time(input, FORMATS)
}

fn parse_local_datetime(
    input: &str,
    date_config: DateParseConfig,
) -> AppResult<Option<NaiveDateTime>> {
    let Some((date_input, time_input)) = split_absolute_datetime(input) else {
        return Ok(None);
    };
    let Some(time) = parse_time_only(time_input) else {
        return Ok(None);
    };

    let Some(date) = parse_numeric_date(date_input, date_config)? else {
        return Ok(None);
    };

    Ok(Some(date.and_time(time)))
}

fn split_absolute_datetime(input: &str) -> Option<(&str, &str)> {
    for separator in ['T', ' '] {
        let Some((date, time)) = input.split_once(separator) else {
            continue;
        };
        let date = date.trim();
        let time = time.trim();
        if !date.is_empty() && !time.is_empty() {
            return Some((date, time));
        }
    }
    None
}

fn parse_numeric_date(
    date_input: &str,
    date_config: DateParseConfig,
) -> AppResult<Option<NaiveDate>> {
    let Some((separator, parts)) = split_date_parts(date_input) else {
        return Ok(None);
    };

    let candidates = date_candidates(parts, separator);
    if candidates.is_empty() {
        return Ok(None);
    }

    resolve_date_candidate(date_input, &candidates, date_config).map(Some)
}

fn split_date_parts(date_input: &str) -> Option<(char, [&str; 3])> {
    let separator = ['/', '-', '.']
        .into_iter()
        .find(|candidate| date_input.contains(*candidate))?;
    let mut parts = date_input.split(separator);
    let first = parts.next()?.trim();
    let second = parts.next()?.trim();
    let third = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    if !all_ascii_digits([first, second, third]) {
        return None;
    }
    Some((separator, [first, second, third]))
}

fn all_ascii_digits(parts: [&str; 3]) -> bool {
    parts
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn date_candidates(parts: [&str; 3], separator: char) -> Vec<DateCandidate> {
    let [first, second, third] = parts;
    let mut candidates = Vec::with_capacity(2);

    if first.len() == 4 {
        push_date_candidate(&mut candidates, DateOrder::Ymd, first, second, third);
        return candidates;
    }

    if third.len() != 4 {
        return candidates;
    }

    push_date_candidate(&mut candidates, DateOrder::Dmy, third, second, first);
    if separator != '.' {
        push_date_candidate(&mut candidates, DateOrder::Mdy, third, first, second);
    }
    candidates
}

fn push_date_candidate(
    candidates: &mut Vec<DateCandidate>,
    order: DateOrder,
    year: &str,
    month: &str,
    day: &str,
) {
    let Ok(year) = year.parse::<i32>() else {
        return;
    };
    let Ok(month) = month.parse::<u32>() else {
        return;
    };
    let Ok(day) = day.parse::<u32>() else {
        return;
    };
    let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else {
        return;
    };
    if candidates.iter().any(|candidate| candidate.date == date) {
        return;
    }
    candidates.push(DateCandidate { order, date });
}

fn resolve_date_candidate(
    raw_date: &str,
    candidates: &[DateCandidate],
    date_config: DateParseConfig,
) -> AppResult<NaiveDate> {
    if let [candidate] = candidates {
        return Ok(candidate.date);
    }

    if date_config.prefer_locale_order
        && let Some(locale_order) = date_config.locale_order
        && let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.order == locale_order)
    {
        return Ok(candidate.date);
    }

    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.order == date_config.fallback_order)
    {
        return Ok(candidate.date);
    }

    let supported_orders = candidates
        .iter()
        .map(|candidate| candidate.order.to_string())
        .collect::<Vec<_>>()
        .join("/");
    Err(format!(
        "ambiguous date `{raw_date}`; valid interpretations match {supported_orders}. disable prefer_locale_date_order, adjust date_order, or use `YYYY-MM-DD`"
    ))
}

fn parse_first_time(input: &str, formats: &[&str]) -> Option<NaiveTime> {
    formats
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(input, format).ok())
}

#[derive(Clone, Copy, Debug)]
struct DateCandidate {
    order: DateOrder,
    date: NaiveDate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn parse_config(order: DateOrder) -> DateParseConfig {
        DateParseConfig {
            fallback_order: order,
            prefer_locale_order: false,
            locale_order: None,
        }
    }

    #[test]
    fn parses_relative_duration() {
        let spec = parse_alarm_spec("10m", parse_config(DateOrder::Dmy)).unwrap();
        assert!(matches!(spec, AlarmSpec::Duration(value) if value == Duration::from_secs(600)));
    }

    #[test]
    fn parses_time_only_24h() {
        let spec = parse_alarm_spec("13:30", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::TimeOfDay(value) if value == NaiveTime::from_hms_opt(13, 30, 0).unwrap())
        );
    }

    #[test]
    fn parses_time_only_12h() {
        let spec = parse_alarm_spec("01:30pm", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::TimeOfDay(value) if value == NaiveTime::from_hms_opt(13, 30, 0).unwrap())
        );
    }

    #[test]
    fn parses_dotted_local_datetime() {
        let spec = parse_alarm_spec("12.03.2026 13:30", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn parses_slash_dates_by_configured_order() {
        let dmy = parse_alarm_spec("03/12/2026 13:30", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(dmy, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-12-03 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );

        let mdy = parse_alarm_spec("03/12/2026 13:30", parse_config(DateOrder::Mdy)).unwrap();
        assert!(
            matches!(mdy, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn parses_unambiguous_month_day_year_even_with_dmy_fallback() {
        let spec = parse_alarm_spec("12/31/2026 8:15pm", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-12-31 20:15:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn parses_hyphenated_month_day_year_when_unambiguous() {
        let spec = parse_alarm_spec("12-31-2026 20:15", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-12-31 20:15:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn locale_order_can_override_fallback_for_ambiguous_slash_dates() {
        let spec = parse_alarm_spec(
            "03/04/2026 13:30",
            DateParseConfig {
                fallback_order: DateOrder::Dmy,
                prefer_locale_order: true,
                locale_order: Some(DateOrder::Mdy),
            },
        )
        .unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-04 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn ymd_fallback_rejects_ambiguous_year_last_slash_dates() {
        let result = parse_alarm_spec("03/04/2026 13:30", parse_config(DateOrder::Ymd));
        assert!(result.is_err());
    }

    #[test]
    fn alarm_defaults_to_configured_mode() {
        let command = parse_command(vec!["10m".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Alarm {
                mode_override: None,
                ..
            }
        ));
    }

    #[test]
    fn foreground_flag_is_parsed() {
        let command = parse_command(vec!["-f".to_string(), "10m".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Alarm {
                mode_override: Some(RunMode::Foreground),
                ..
            }
        ));
    }

    #[test]
    fn background_flag_is_parsed() {
        let command = parse_command(vec!["-b".to_string(), "10m".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Alarm {
                mode_override: Some(RunMode::Background),
                ..
            }
        ));
    }

    #[test]
    fn conflicting_mode_flags_are_rejected() {
        let result = parse_command(vec!["-f".to_string(), "-b".to_string(), "10m".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn stop_command_variants_are_parsed() {
        let command = parse_command(vec!["stop".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Stop {
                selector: None,
                all: false
            }
        ));

        let command = parse_command(vec!["stop".to_string(), "--all".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Stop {
                selector: None,
                all: true
            }
        ));

        let command = parse_command(vec!["stop".to_string(), "abc123".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Stop {
                selector: Some(_),
                all: false
            }
        ));
    }

    #[test]
    fn worker_command_is_parsed() {
        let command = parse_command(vec![
            "__worker".to_string(),
            "--alarm-id".to_string(),
            "abc123".to_string(),
            "--target-utc".to_string(),
            "2026-03-12T12:30:00Z".to_string(),
            "--auto-stop-seconds".to_string(),
            "15".to_string(),
            "--volume".to_string(),
            "0.40".to_string(),
            "--sound-file".to_string(),
            "/tmp/alarm.mp3".to_string(),
            "--notifications-enabled".to_string(),
            "true".to_string(),
            "--notifications-clickable".to_string(),
            "true".to_string(),
            "--notifications-timeout-ms".to_string(),
            "0".to_string(),
            "--notifications-show-stop-button".to_string(),
            "true".to_string(),
        ])
        .unwrap();

        assert!(matches!(
            command,
            Command::Worker {
                volume,
                notifications,
                sound_file: Some(_),
                ..
            } if (volume - 0.40).abs() < f32::EPSILON
                && notifications.enabled
                && notifications.clickable
                && notifications.timeout_ms == 0
                && notifications.show_stop_button
        ));
    }

    #[test]
    fn volume_shortcuts_are_parsed() {
        let command = parse_command(vec!["volume".to_string(), "0.35".to_string()]).unwrap();
        assert!(
            matches!(command, Command::VolumeSet { volume } if (volume - 0.35).abs() < f64::EPSILON)
        );

        let command = parse_command(vec!["volume".to_string(), "test".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::VolumeTest {
                volume_override: None
            }
        ));
    }

    #[test]
    fn parses_year_first_slash_datetime() {
        let spec = parse_alarm_spec("2026/03/12 13:30", parse_config(DateOrder::Dmy)).unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }
}
