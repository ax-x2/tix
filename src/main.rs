mod alarm;
mod config;
mod display;
mod parse;
mod schedule;
mod state;
mod types;

use std::env;
use std::fmt::{Display, Write as FmtWrite};
use std::time::Duration;

use alarm::{run_alarm_session, run_background_worker, test_alarm_audio};
use config::{
    apply_config_update, config_file_path, load_existing_config_or_default, load_or_create_config,
    parse_volume, render_config, save_config, validate_timezone,
};
use display::ForegroundRenderer;
use parse::{HELP, parse_alarm_spec, parse_command};
use schedule::{format_alarm_time, resolve_alarm_with_now};
use state::{
    active_alarm_states, parse_state_target_utc, remove_alarm_state_by_id, resolve_alarm_selector,
    schedule_background_alarm, terminate_process,
};
use types::{AlarmAudioConfig, AppResult, Command, Config, RunMode};

fn main() {
    if let Err(err) = run() {
        eprintln!("tix: {err}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    match parse_command(env::args().skip(1))? {
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::Status => show_alarm_status(),
        Command::Stop { selector, all } => stop_active_alarms(selector, all),
        Command::ConfigPath => {
            println!("{}", config_file_path()?.display());
            Ok(())
        }
        Command::ConfigInit => {
            let path = config_file_path()?;
            let config = load_or_create_config()?;
            print_section("Config Initialized");
            print_detail("Path", path.display());
            println!();
            print!("{}", render_config(&config)?);
            Ok(())
        }
        Command::ConfigShow => {
            let path = config_file_path()?;
            let config = load_or_create_config()?;
            print_section("Config");
            print_detail("Path", path.display());
            println!();
            print!("{}", render_config(&config)?);
            Ok(())
        }
        Command::ConfigSet { key, value } => {
            let path = config_file_path()?;
            let mut config = load_or_create_config()?;
            apply_config_update(&mut config, &key, &value)?;
            save_config(&path, &config)?;
            print_section("Config Updated");
            print_detail("Path", path.display());
            print_detail("Key", &key);
            print_detail("Value", &value);
            println!();
            print!("{}", render_config(&config)?);
            Ok(())
        }
        Command::VolumeShow => show_volume(),
        Command::VolumeSet { volume } => set_volume(volume),
        Command::VolumeTest { volume_override } => test_volume(volume_override),
        Command::Alarm {
            spec_text,
            dry_run,
            timezone_override,
            mode_override,
        } => {
            let mut config = load_or_create_config()?;
            if let Some(timezone) = timezone_override {
                validate_timezone(&timezone)?;
                config.timezone = timezone;
            }
            run_alarm_command(config, &spec_text, dry_run, mode_override)
        }
        Command::Worker {
            alarm_id,
            target_utc,
            auto_stop_seconds,
            volume,
            sound_file,
            notifications,
        } => run_background_worker(
            alarm_id,
            target_utc,
            auto_stop_seconds,
            AlarmAudioConfig { volume, sound_file },
            notifications,
        ),
    }
}

fn run_alarm_command(
    config: Config,
    spec_text: &str,
    dry_run: bool,
    mode_override: Option<RunMode>,
) -> AppResult<()> {
    let timezone = config.parsed_timezone()?;
    let spec = parse_alarm_spec(spec_text, config.date_parse_config())?;
    let now = chrono::Utc::now().with_timezone(&timezone);
    let target = resolve_alarm_with_now(spec, timezone, now)?;
    let target_utc = target.with_timezone(&chrono::Utc);
    let remaining = (target_utc - chrono::Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO);
    let audio = AlarmAudioConfig {
        volume: config.volume as f32,
        sound_file: config.sound_file.clone(),
    };
    let effective_mode = RunMode::resolve(mode_override, config.default_mode);

    print_section(if dry_run {
        "Alarm Preview"
    } else {
        "Alarm Ready"
    });
    print_detail("Input", spec_text);
    print_detail("Target", format_alarm_time(target, config.time_notation));
    print_detail("Remaining", format_pretty_duration(remaining));
    print_detail("Mode", mode_label(effective_mode));

    if dry_run {
        println!();
        println!("Preview only. No alarm started.");
        return Ok(());
    }

    if effective_mode == RunMode::Foreground {
        println!();
        println!("Foreground mode. Press Ctrl-C to cancel or stop the ringing alarm.");
        let mut renderer = ForegroundRenderer::new(
            config.foreground,
            timezone,
            config.time_notation,
            target,
            spec_text,
        );
        let renderer_opt = if renderer.enabled() {
            Some(&mut renderer)
        } else {
            None
        };
        return run_alarm_session(
            None,
            target_utc,
            config.auto_stop_seconds,
            &audio,
            renderer_opt,
            true,
            false,
            config.notifications.clone().into(),
        );
    }

    let state = schedule_background_alarm(
        spec_text,
        target_utc,
        config.auto_stop_seconds,
        &audio,
        config.notifications.clone().into(),
    )?;
    println!();
    print_section("Background Worker");
    print_detail("ID", &state.id);
    print_detail("PID", state.pid);
    print_detail("Stop", format!("tix stop {}", state.id));
    Ok(())
}

fn show_volume() -> AppResult<()> {
    let config = load_or_create_config()?;
    println!("{:.2}", config.volume);
    Ok(())
}

fn set_volume(volume: f64) -> AppResult<()> {
    let path = config_file_path()?;
    let mut config = load_or_create_config()?;
    config.volume = volume;
    config.validate()?;
    save_config(&path, &config)?;
    print_section("Volume Updated");
    print_detail("Value", format!("{:.2}", config.volume));
    Ok(())
}

fn test_volume(volume_override: Option<f64>) -> AppResult<()> {
    let mut config = load_or_create_config()?;
    if let Some(volume) = volume_override {
        config.volume = parse_volume(&volume.to_string())?;
    }

    print_section("Volume Test");
    print_detail("Volume", format!("{:.2}", config.volume));
    print_detail(
        "Sound",
        config
            .sound_file
            .as_deref()
            .unwrap_or("built-in tone fallback"),
    );
    println!();

    test_alarm_audio(&AlarmAudioConfig {
        volume: config.volume as f32,
        sound_file: config.sound_file,
    })
}

fn show_alarm_status() -> AppResult<()> {
    let states = active_alarm_states()?;
    if states.is_empty() {
        println!("No active alarms.");
        return Ok(());
    }

    let config = load_existing_config_or_default()?;
    let timezone = config.parsed_timezone()?;
    let now_utc = chrono::Utc::now();

    print_section("Active Alarms");
    print_detail("Count", states.len());
    for (index, state) in states.iter().enumerate() {
        let target_utc = parse_state_target_utc(state)?;
        let target_local = target_utc.with_timezone(&timezone);
        let remaining = (target_utc - now_utc).to_std().unwrap_or(Duration::ZERO);

        println!();
        print_subsection(&format!("[{}] {}", index + 1, state.id));
        print_detail("PID", state.pid);
        print_detail("Input", &state.spec_text);
        print_detail(
            "Target",
            format_alarm_time(target_local, config.time_notation),
        );
        print_detail("Remaining", format_pretty_duration(remaining));
        print_detail("Created", &state.created_at_utc);
    }
    Ok(())
}

fn stop_active_alarms(selector: Option<String>, all: bool) -> AppResult<()> {
    let states = active_alarm_states()?;
    if states.is_empty() {
        println!("No active alarms.");
        return Ok(());
    }

    if all {
        for state in &states {
            terminate_process(state.pid)?;
            remove_alarm_state_by_id(&state.id)?;
        }
        print_section("Alarms Stopped");
        print_detail("Count", states.len());
        return Ok(());
    }

    let state = match selector {
        Some(selector) => resolve_alarm_selector(&states, &selector)?,
        None if states.len() == 1 => states[0].clone(),
        None => {
            let ids = states
                .iter()
                .map(|state| state.id.clone())
                .collect::<Vec<_>>();
            return Err(format!(
                "multiple active alarms found; use `tix stop <id>` or `tix stop --all`\nactive ids: {}",
                ids.join(", ")
            ));
        }
    };

    terminate_process(state.pid)?;
    remove_alarm_state_by_id(&state.id)?;
    print_section("Alarm Stopped");
    print_detail("ID", &state.id);
    print_detail("PID", state.pid);
    Ok(())
}

fn print_section(title: &str) {
    const WIDTH: usize = 56;
    let prefix = format!("== {title} ");
    let fill = "-".repeat(WIDTH.saturating_sub(prefix.len()).max(4));
    println!("{prefix}{fill}");
}

fn print_subsection(title: &str) {
    println!("-- {title}");
}

fn print_detail(label: &str, value: impl Display) {
    println!("  {:<12} {value}", format!("{label}:"));
}

fn mode_label(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Background => "background",
        RunMode::Foreground => "foreground",
    }
}

fn format_pretty_duration(duration: Duration) -> String {
    let rounded_secs = duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() >= 500_000_000));
    if rounded_secs == 0 && duration > Duration::ZERO {
        return "less than 1s".to_string();
    }
    if rounded_secs == 0 {
        return "0s".to_string();
    }

    let days = rounded_secs / 86_400;
    let hours = (rounded_secs % 86_400) / 3_600;
    let minutes = (rounded_secs % 3_600) / 60;
    let seconds = rounded_secs % 60;
    let mut rendered = String::with_capacity(32);

    if days > 0 {
        let _ = write!(rendered, "{days}d");
    }
    if hours > 0 || !rendered.is_empty() {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        let _ = write!(rendered, "{hours}h");
    }
    if minutes > 0 || !rendered.is_empty() {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        let _ = write!(rendered, "{minutes}m");
    }
    if !rendered.is_empty() {
        rendered.push(' ');
    }
    let _ = write!(rendered, "{seconds}s");
    rendered
}
