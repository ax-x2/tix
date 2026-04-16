use chrono::{DateTime, FixedOffset, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::str::FromStr;
use std::sync::{Arc, atomic::AtomicBool, mpsc};
use std::time::Duration;

pub type AppResult<T> = Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub timezone: String,
    pub date_order: DateOrder,
    pub prefer_locale_date_order: bool,
    pub time_notation: TimeNotation,
    pub default_mode: RunMode,
    pub auto_stop_seconds: u64,
    pub volume: f64,
    pub sound_file: Option<String>,
    pub notifications: NotificationConfig,
    pub foreground: ForegroundConfig,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DateOrder {
    #[serde(rename = "dmy")]
    Dmy,
    #[serde(rename = "mdy")]
    Mdy,
    #[serde(rename = "ymd")]
    Ymd,
}

impl Display for DateOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            DateOrder::Dmy => "dmy",
            DateOrder::Mdy => "mdy",
            DateOrder::Ymd => "ymd",
        };
        f.write_str(value)
    }
}

impl FromStr for DateOrder {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dmy" => Ok(Self::Dmy),
            "mdy" => Ok(Self::Mdy),
            "ymd" => Ok(Self::Ymd),
            _ => Err("date_order must be one of: dmy, mdy, ymd".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RunMode {
    #[default]
    #[serde(rename = "background")]
    Background,
    #[serde(rename = "foreground")]
    Foreground,
}

impl Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            RunMode::Background => "background",
            RunMode::Foreground => "foreground",
        };
        f.write_str(value)
    }
}

impl FromStr for RunMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "background" | "bg" | "b" => Ok(Self::Background),
            "foreground" | "fg" | "f" => Ok(Self::Foreground),
            _ => Err("run mode must be one of: background, foreground".to_string()),
        }
    }
}

impl RunMode {
    pub fn resolve(mode_override: Option<Self>, configured: Self) -> Self {
        mode_override.unwrap_or(configured)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeNotation {
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "12h")]
    H12,
}

impl Display for TimeNotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            TimeNotation::H24 => "24h",
            TimeNotation::H12 => "12h",
        };
        f.write_str(value)
    }
}

impl FromStr for TimeNotation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "24h" => Ok(Self::H24),
            "12h" => Ok(Self::H12),
            _ => Err("time_notation must be one of: 24h, 12h".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TimerStyle {
    #[default]
    #[serde(rename = "digital")]
    Digital,
    #[serde(rename = "human")]
    Human,
}

impl Display for TimerStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            TimerStyle::Digital => "digital",
            TimerStyle::Human => "human",
        };
        f.write_str(value)
    }
}

impl FromStr for TimerStyle {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "digital" => Ok(Self::Digital),
            "human" => Ok(Self::Human),
            _ => Err("timer_style must be one of: digital, human".to_string()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ForegroundConfig {
    pub refresh_interval_ms: u64,
    pub show_current_datetime: bool,
    pub show_target_datetime: bool,
    pub show_remaining: bool,
    pub show_input: bool,
    pub timer_style: TimerStyle,
}

impl Default for ForegroundConfig {
    fn default() -> Self {
        Self {
            refresh_interval_ms: 250,
            show_current_datetime: true,
            show_target_datetime: true,
            show_remaining: true,
            show_input: true,
            timer_style: TimerStyle::Digital,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AlarmAudioConfig {
    pub volume: f32,
    pub sound_file: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub clickable: bool,
    pub timeout_ms: u32,
    pub show_stop_button: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            clickable: true,
            timeout_ms: 0,
            show_stop_button: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AlarmNotificationConfig {
    pub enabled: bool,
    pub clickable: bool,
    pub timeout_ms: u32,
    pub show_stop_button: bool,
}

impl From<NotificationConfig> for AlarmNotificationConfig {
    fn from(value: NotificationConfig) -> Self {
        Self {
            enabled: value.enabled,
            clickable: value.clickable,
            timeout_ms: value.timeout_ms,
            show_stop_button: value.show_stop_button,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DateParseConfig {
    pub fallback_order: DateOrder,
    pub prefer_locale_order: bool,
    pub locale_order: Option<DateOrder>,
}

#[derive(Debug)]
pub enum Command {
    Alarm {
        spec_text: String,
        dry_run: bool,
        timezone_override: Option<String>,
        mode_override: Option<RunMode>,
    },
    Worker {
        alarm_id: String,
        target_utc: DateTime<Utc>,
        auto_stop_seconds: u64,
        volume: f32,
        sound_file: Option<String>,
        notifications: AlarmNotificationConfig,
    },
    Status,
    Stop {
        selector: Option<String>,
        all: bool,
    },
    VolumeShow,
    VolumeSet {
        volume: f64,
    },
    VolumeTest {
        volume_override: Option<f64>,
    },
    ConfigInit,
    ConfigShow,
    ConfigPath,
    ConfigSet {
        key: String,
        value: String,
    },
    Help,
}

#[derive(Debug)]
pub enum AlarmSpec {
    Duration(Duration),
    Explicit(DateTime<FixedOffset>),
    Absolute(NaiveDateTime),
    TimeOfDay(NaiveTime),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveAlarmState {
    pub id: String,
    pub pid: u32,
    pub spec_text: String,
    pub target_utc: String,
    pub created_at_utc: String,
    pub auto_stop_seconds: u64,
    pub volume: f32,
    pub sound_file: Option<String>,
}

pub struct StopControl {
    pub stop: Arc<AtomicBool>,
    pub wake_tx: mpsc::SyncSender<()>,
    pub wake_rx: mpsc::Receiver<()>,
}
