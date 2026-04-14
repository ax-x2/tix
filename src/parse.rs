use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};

use crate::config::parse_volume;
use crate::types::{AlarmSpec, AppResult, Command, DateOrder, RunMode};

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
  tix 12.03.2026 13:30
  tix status
  tix stop 17fd2a
  tix stop --all
  tix volume
  tix volume 0.35
  tix volume test

Notes:
  - CLI mode flags override the configured default mode.
  - slash dates use the configured date_order from ~/.config/tix/config.toml.
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

pub fn parse_alarm_spec(input: &str, date_order: DateOrder) -> AppResult<AlarmSpec> {
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

    if let Some(datetime) = parse_local_datetime(&normalized, date_order) {
        return Ok(AlarmSpec::Absolute(datetime));
    }

    Err(format!(
        "unsupported time format `{normalized}`\n\
supported examples: `10m`, `1h30m`, `13:30`, `01:30pm`, `12.03.2026 13:30`, `2026-03-12 13:30`"
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

fn parse_local_datetime(input: &str, date_order: DateOrder) -> Option<NaiveDateTime> {
    const ISO_FORMATS: &[&str] = &[
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %I:%M%p",
        "%Y-%m-%d %I:%M %p",
        "%Y-%m-%d %I:%M:%S%p",
        "%Y-%m-%d %I:%M:%S %p",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%I:%M%p",
        "%Y-%m-%dT%I:%M %p",
        "%Y-%m-%dT%I:%M:%S%p",
        "%Y-%m-%dT%I:%M:%S %p",
    ];
    const DOT_DMY_FORMATS: &[&str] = &[
        "%d.%m.%Y %H:%M",
        "%d.%m.%Y %H:%M:%S",
        "%d.%m.%Y %I:%M%p",
        "%d.%m.%Y %I:%M %p",
        "%d.%m.%Y %I:%M:%S%p",
        "%d.%m.%Y %I:%M:%S %p",
    ];
    const SLASH_DMY_FORMATS: &[&str] = &[
        "%d/%m/%Y %H:%M",
        "%d/%m/%Y %H:%M:%S",
        "%d/%m/%Y %I:%M%p",
        "%d/%m/%Y %I:%M %p",
        "%d/%m/%Y %I:%M:%S%p",
        "%d/%m/%Y %I:%M:%S %p",
    ];
    const SLASH_MDY_FORMATS: &[&str] = &[
        "%m/%d/%Y %H:%M",
        "%m/%d/%Y %H:%M:%S",
        "%m/%d/%Y %I:%M%p",
        "%m/%d/%Y %I:%M %p",
        "%m/%d/%Y %I:%M:%S%p",
        "%m/%d/%Y %I:%M:%S %p",
    ];
    const SLASH_YMD_FORMATS: &[&str] = &[
        "%Y/%m/%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%d %I:%M%p",
        "%Y/%m/%d %I:%M %p",
        "%Y/%m/%d %I:%M:%S%p",
        "%Y/%m/%d %I:%M:%S %p",
    ];

    parse_first_datetime(input, ISO_FORMATS)
        .or_else(|| parse_first_datetime(input, DOT_DMY_FORMATS))
        .or_else(|| {
            let slash_formats = match date_order {
                DateOrder::Dmy => SLASH_DMY_FORMATS,
                DateOrder::Mdy => SLASH_MDY_FORMATS,
                DateOrder::Ymd => SLASH_YMD_FORMATS,
            };
            parse_first_datetime(input, slash_formats)
        })
}

fn parse_first_time(input: &str, formats: &[&str]) -> Option<NaiveTime> {
    formats
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(input, format).ok())
}

fn parse_first_datetime(input: &str, formats: &[&str]) -> Option<NaiveDateTime> {
    formats
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(input, format).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_relative_duration() {
        let spec = parse_alarm_spec("10m", DateOrder::Dmy).unwrap();
        assert!(matches!(spec, AlarmSpec::Duration(value) if value == Duration::from_secs(600)));
    }

    #[test]
    fn parses_time_only_24h() {
        let spec = parse_alarm_spec("13:30", DateOrder::Dmy).unwrap();
        assert!(
            matches!(spec, AlarmSpec::TimeOfDay(value) if value == NaiveTime::from_hms_opt(13, 30, 0).unwrap())
        );
    }

    #[test]
    fn parses_time_only_12h() {
        let spec = parse_alarm_spec("01:30pm", DateOrder::Dmy).unwrap();
        assert!(
            matches!(spec, AlarmSpec::TimeOfDay(value) if value == NaiveTime::from_hms_opt(13, 30, 0).unwrap())
        );
    }

    #[test]
    fn parses_dotted_local_datetime() {
        let spec = parse_alarm_spec("12.03.2026 13:30", DateOrder::Dmy).unwrap();
        assert!(
            matches!(spec, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
    }

    #[test]
    fn parses_slash_dates_by_configured_order() {
        let dmy = parse_alarm_spec("03/12/2026 13:30", DateOrder::Dmy).unwrap();
        assert!(
            matches!(dmy, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-12-03 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );

        let mdy = parse_alarm_spec("03/12/2026 13:30", DateOrder::Mdy).unwrap();
        assert!(
            matches!(mdy, AlarmSpec::Absolute(value) if value == NaiveDateTime::parse_from_str("2026-03-12 13:30:00", "%Y-%m-%d %H:%M:%S").unwrap())
        );
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
        ])
        .unwrap();

        assert!(matches!(
            command,
            Command::Worker {
                volume,
                sound_file: Some(_),
                ..
            } if (volume - 0.40).abs() < f32::EPSILON
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
}
