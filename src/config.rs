use chrono_tz::Tz;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::types::{AppResult, Config, DateOrder, ForegroundConfig, RunMode, TimeNotation};

pub fn config_root() -> AppResult<PathBuf> {
    let Some(home) = env::var_os("HOME") else {
        return Err("HOME is not set; cannot resolve ~/.config/tix".to_string());
    };
    Ok(PathBuf::from(home).join(".config").join("tix"))
}

pub fn config_file_path() -> AppResult<PathBuf> {
    Ok(config_root()?.join("config.toml"))
}

pub fn load_or_create_config() -> AppResult<Config> {
    let path = config_file_path()?;
    if path.exists() {
        return load_config(&path);
    }

    let config = bootstrap_config()?;
    save_config(&path, &config)?;
    Ok(config)
}

pub fn load_existing_config_or_default() -> AppResult<Config> {
    let path = config_file_path()?;
    if path.exists() {
        load_config(&path)
    } else {
        Ok(Config::default())
    }
}

pub fn load_config(path: &Path) -> AppResult<Config> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("failed to read config {}: {err}", path.display()))?;
    let config: Config = toml::from_str(&raw)
        .map_err(|err| format!("failed to parse config {}: {err}", path.display()))?;
    config.validate()?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &Config) -> AppResult<()> {
    let Some(parent) = path.parent() else {
        return Err(format!(
            "config path {} has no parent directory",
            path.display()
        ));
    };
    fs::create_dir_all(parent)
        .map_err(|err| format!("failed to create config dir {}: {err}", parent.display()))?;

    let mut rendered = render_config(config)?;
    rendered.push('\n');
    fs::write(path, rendered)
        .map_err(|err| format!("failed to write config {}: {err}", path.display()))
}

pub fn render_config(config: &Config) -> AppResult<String> {
    toml::to_string_pretty(config).map_err(|err| format!("failed to render config to TOML: {err}"))
}

pub fn apply_config_update(config: &mut Config, key: &str, value: &str) -> AppResult<()> {
    match key {
        "timezone" => {
            validate_timezone(value)?;
            config.timezone = value.to_string();
        }
        "date_order" | "date-order" => {
            config.date_order = value.parse()?;
        }
        "time_notation" | "time-notation" => {
            config.time_notation = value.parse()?;
        }
        "default_mode" | "default-mode" => {
            config.default_mode = value.parse()?;
        }
        "auto_stop_seconds" | "auto-stop-seconds" => {
            config.auto_stop_seconds = value
                .parse()
                .map_err(|_| "auto_stop_seconds must be an unsigned integer".to_string())?;
        }
        "volume" => {
            config.volume = parse_volume(value)?;
        }
        "sound_file" | "sound-file" => {
            config.sound_file = parse_sound_file_value(value)?;
        }
        "foreground.refresh_interval_ms" | "foreground.refresh-interval-ms" => {
            config.foreground.refresh_interval_ms = value.parse().map_err(|_| {
                "foreground.refresh_interval_ms must be an unsigned integer".to_string()
            })?;
        }
        "foreground.show_current_datetime" | "foreground.show-current-datetime" => {
            config.foreground.show_current_datetime = parse_bool(value)?;
        }
        "foreground.show_target_datetime" | "foreground.show-target-datetime" => {
            config.foreground.show_target_datetime = parse_bool(value)?;
        }
        "foreground.show_remaining" | "foreground.show-remaining" => {
            config.foreground.show_remaining = parse_bool(value)?;
        }
        "foreground.show_input" | "foreground.show-input" => {
            config.foreground.show_input = parse_bool(value)?;
        }
        "foreground.timer_style" | "foreground.timer-style" => {
            config.foreground.timer_style = value.parse()?;
        }
        _ => {
            return Err(
                "supported config keys: timezone, date_order, time_notation, default_mode, auto_stop_seconds, volume, sound_file, foreground.refresh_interval_ms, foreground.show_current_datetime, foreground.show_target_datetime, foreground.show_remaining, foreground.show_input, foreground.timer_style"
                    .to_string(),
            );
        }
    }
    config.validate()
}

pub fn validate_timezone(value: &str) -> AppResult<()> {
    value
        .parse::<Tz>()
        .map(|_| ())
        .map_err(|_| format!("invalid timezone `{value}`"))
}

impl Config {
    pub fn validate(&self) -> AppResult<()> {
        self.parsed_timezone()?;
        validate_volume(self.volume)?;
        self.foreground.validate()?;
        Ok(())
    }

    pub fn parsed_timezone(&self) -> AppResult<Tz> {
        self.timezone
            .parse::<Tz>()
            .map_err(|_| format!("invalid timezone `{}` in config", self.timezone))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timezone: detect_default_timezone(),
            date_order: DateOrder::Dmy,
            time_notation: TimeNotation::H24,
            default_mode: RunMode::Background,
            auto_stop_seconds: 0,
            volume: 0.20,
            sound_file: None,
            foreground: ForegroundConfig::default(),
        }
    }
}

fn detect_default_timezone() -> String {
    match iana_time_zone::get_timezone() {
        Ok(timezone) if timezone.parse::<Tz>().is_ok() => timezone,
        _ => "UTC".to_string(),
    }
}

fn bootstrap_config() -> AppResult<Config> {
    let defaults = Config::default();
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(defaults);
    }

    println!("Creating {}", config_file_path()?.display());
    println!("Press Enter to accept the default in brackets.");

    let timezone = prompt_timezone(&defaults.timezone)?;
    let date_order = prompt_date_order(defaults.date_order)?;
    let time_notation = prompt_time_notation(defaults.time_notation)?;
    let default_mode = prompt_default_mode(defaults.default_mode)?;
    let auto_stop_seconds = prompt_auto_stop(defaults.auto_stop_seconds)?;
    let volume = prompt_volume(defaults.volume)?;

    Ok(Config {
        timezone,
        date_order,
        time_notation,
        default_mode,
        auto_stop_seconds,
        volume,
        sound_file: defaults.sound_file,
        foreground: defaults.foreground,
    })
}

fn prompt_timezone(default: &str) -> AppResult<String> {
    let input = prompt_line(&format!("timezone [{default}]: "))?;
    if input.is_empty() {
        return Ok(default.to_string());
    }
    validate_timezone(&input)?;
    Ok(input)
}

fn prompt_date_order(default: DateOrder) -> AppResult<DateOrder> {
    loop {
        let input = prompt_line(&format!("date order [{default}] (dmy/mdy/ymd): "))?;
        if input.is_empty() {
            return Ok(default);
        }
        match input.parse() {
            Ok(value) => return Ok(value),
            Err(err) => eprintln!("{err}"),
        }
    }
}

fn prompt_time_notation(default: TimeNotation) -> AppResult<TimeNotation> {
    loop {
        let input = prompt_line(&format!("time notation [{default}] (24h/12h): "))?;
        if input.is_empty() {
            return Ok(default);
        }
        match input.parse() {
            Ok(value) => return Ok(value),
            Err(err) => eprintln!("{err}"),
        }
    }
}

fn prompt_default_mode(default: RunMode) -> AppResult<RunMode> {
    loop {
        let input = prompt_line(&format!(
            "default mode [{default}] (background/foreground): "
        ))?;
        if input.is_empty() {
            return Ok(default);
        }
        match input.parse() {
            Ok(value) => return Ok(value),
            Err(err) => eprintln!("{err}"),
        }
    }
}

fn prompt_auto_stop(default: u64) -> AppResult<u64> {
    loop {
        let input = prompt_line(&format!("auto-stop seconds [{default}]: "))?;
        if input.is_empty() {
            return Ok(default);
        }
        match input.parse::<u64>() {
            Ok(value) => return Ok(value),
            Err(_) => eprintln!("auto-stop seconds must be an unsigned integer"),
        }
    }
}

fn prompt_volume(default: f64) -> AppResult<f64> {
    loop {
        let input = prompt_line(&format!("volume [{default:.2}] (0.0-1.0): "))?;
        if input.is_empty() {
            return Ok(default);
        }
        match parse_volume(&input) {
            Ok(value) => return Ok(value),
            Err(err) => eprintln!("{err}"),
        }
    }
}

fn prompt_line(prompt: &str) -> AppResult<String> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|err| format!("failed to flush prompt: {err}"))?;

    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .map_err(|err| format!("failed to read user input: {err}"))?;
    Ok(buffer.trim().to_string())
}

impl ForegroundConfig {
    pub fn validate(&self) -> AppResult<()> {
        if self.refresh_interval_ms == 0 {
            return Err("foreground.refresh_interval_ms must be greater than 0".to_string());
        }
        Ok(())
    }

    pub fn effective_refresh_interval_ms(&self) -> u64 {
        self.refresh_interval_ms.max(100)
    }
}

pub fn parse_volume(value: &str) -> AppResult<f64> {
    let volume = value
        .parse::<f64>()
        .map_err(|_| "volume must be a number between 0.0 and 1.0".to_string())?;
    validate_volume(volume)?;
    Ok(volume)
}

pub fn validate_volume(volume: f64) -> AppResult<()> {
    if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
        return Err("volume must be between 0.0 and 1.0".to_string());
    }
    Ok(())
}

pub fn parse_bool(value: &str) -> AppResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err("boolean value must be one of: true, false".to_string()),
    }
}

fn parse_sound_file_value(value: &str) -> AppResult<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || matches!(trimmed.to_ascii_lowercase().as_str(), "none" | "off") {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

pub fn resolve_sound_file_path(sound_file: &str) -> AppResult<PathBuf> {
    let raw = sound_file.trim();
    if raw.is_empty() {
        return Err("sound_file cannot be empty".to_string());
    }

    let path = if let Some(rest) = raw.strip_prefix("~/") {
        let Some(home) = env::var_os("HOME") else {
            return Err("HOME is not set; cannot expand `~/` in sound_file".to_string());
        };
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(raw)
    };

    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(config_root()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_shape_still_loads_with_defaults() {
        let config: Config = toml::from_str(
            r#"
timezone = "Europe/Berlin"
date_order = "dmy"
time_notation = "24h"
auto_stop_seconds = 0
"#,
        )
        .unwrap();

        assert_eq!(config.default_mode, RunMode::Background);
        assert!((config.volume - 0.20).abs() < f64::EPSILON);
        assert!(config.sound_file.is_none());
        assert!(config.foreground.show_remaining);
    }

    #[test]
    fn provided_config_shape_parses_cleanly() {
        let config: Config = toml::from_str(
            r#"
timezone = "Europe/Berlin"
date_order = "dmy"
time_notation = "24h"
default_mode = "background"
auto_stop_seconds = 0
volume = 0.3
sound_file = "/home/x/Music/3.mp3"

[foreground]
refresh_interval_ms = 250
show_current_datetime = true
show_target_datetime = true
show_remaining = true
show_input = true
timer_style = "digital"
"#,
        )
        .unwrap();

        assert_eq!(config.timezone, "Europe/Berlin");
        assert_eq!(config.default_mode, RunMode::Background);
        assert!((config.volume - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.sound_file.as_deref(), Some("/home/x/Music/3.mp3"));
        assert!(config.foreground.show_current_datetime);
    }

    #[test]
    fn nested_foreground_keys_can_be_updated() {
        let mut config = Config::default();
        apply_config_update(&mut config, "foreground.timer_style", "human").unwrap();
        apply_config_update(&mut config, "foreground.show_input", "false").unwrap();
        apply_config_update(&mut config, "default_mode", "foreground").unwrap();
        apply_config_update(&mut config, "volume", "0.35").unwrap();

        assert_eq!(config.foreground.timer_style.to_string(), "human");
        assert!(!config.foreground.show_input);
        assert_eq!(config.default_mode, RunMode::Foreground);
        assert!((config.volume - 0.35).abs() < f64::EPSILON);
    }
}
