mod alarm;
mod config;
mod display;
mod parse;
mod schedule;
mod state;
mod types;

use std::env;
use std::fmt::Display;
use std::time::Duration;

use alarm::{run_alarm_session, run_background_worker, test_alarm_audio};
use config::{
    apply_config_update, config_file_path, load_existing_config_or_default, load_or_create_config,
    parse_volume, render_config, save_config, validate_timezone,
};
use display::ForegroundRenderer;
use parse::{parse_alarm_spec, parse_command, HELP};
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
            print_section("config initialized");
            print_detail("path", path.display());
            println!();
            print!("{}", render_config(&config)?);
            Ok(())
        }
        Command::ConfigShow => {
            let path = config_file_path()?;
            let config = load_or_create_config()?;
            print_section("config");
            print_detail("path", path.display());
            println!();
            print!("{}", render_config(&config)?);
            Ok(())
        }
        Command::ConfigSet { key, value } => {
            let path = config_file_path()?;
            let mut config = load_or_create_config()?;
            apply_config_update(&mut config, &key, &value)?;
            save_config(&path, &config)?;
            print_section("config Updated");
            print_detail("path", path.display());
            print_detail("key", &key);
            print_detail("value", &value);
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
        } => run_background_worker(
            alarm_id,
            target_utc,
            auto_stop_seconds,
            AlarmAudioConfig { volume, sound_file },
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
    let spec = parse_alarm_spec(spec_text, config.date_order)?;
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
        "alarm preview"
    } else {
        "alarm ready"
    });
    print_detail("input", spec_text);
    print_detail("target", format_alarm_time(target, config.time_notation));
    print_detail("remaining", format_pretty_duration(remaining));
    print_detail("mode", mode_label(effective_mode));

    if dry_run {
        println!();
        println!("dry run only. no alarm started.");
        return Ok(());
    }

    if effective_mode == RunMode::Foreground {
        println!();
        println!("foreground mode. press Ctrl-C to cancel or stop the ringing alarm.");
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
            target_utc,
            config.auto_stop_seconds,
            &audio,
            renderer_opt,
            true,
            false,
        );
    }

    let state = schedule_background_alarm(spec_text, target_utc, config.auto_stop_seconds, &audio)?;
    println!();
    print_section("background worker");
    print_detail("id", &state.id);
    print_detail("pid", state.pid);
    print_detail("stop", format!("tix stop {}", state.id));
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
    print_section("volume updated");
    print_detail("value", format!("{:.2}", config.volume));
    Ok(())
}

fn test_volume(volume_override: Option<f64>) -> AppResult<()> {
    let mut config = load_or_create_config()?;
    if let Some(volume) = volume_override {
        config.volume = parse_volume(&volume.to_string())?;
    }

    print_section("volume test");
    print_detail("volume", format!("{:.2}", config.volume));
    print_detail(
        "sound",
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
        println!("no active alarms.");
        return Ok(());
    }

    let config = load_existing_config_or_default()?;
    let timezone = config.parsed_timezone()?;
    let now_utc = chrono::Utc::now();

    print_section("active Alarms");
    print_detail("count", states.len());
    for (index, state) in states.iter().enumerate() {
        let target_utc = parse_state_target_utc(state)?;
        let target_local = target_utc.with_timezone(&timezone);
        let remaining = (target_utc - now_utc).to_std().unwrap_or(Duration::ZERO);

        println!();
        print_subsection(&format!("[{}] {}", index + 1, state.id));
        print_detail("pid", state.pid);
        print_detail("input", &state.spec_text);
        print_detail(
            "target",
            format_alarm_time(target_local, config.time_notation),
        );
        print_detail("remaining", format_pretty_duration(remaining));
        print_detail("created", &state.created_at_utc);
    }
    Ok(())
}

fn stop_active_alarms(selector: Option<String>, all: bool) -> AppResult<()> {
    let states = active_alarm_states()?;
    if states.is_empty() {
        println!("no active alarms.");
        return Ok(());
    }

    if all {
        for state in &states {
            terminate_process(state.pid)?;
            remove_alarm_state_by_id(&state.id)?;
        }
        print_section("alarms stopped");
        print_detail("count", states.len());
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
    print_section("alarm Stopped");
    print_detail("id", &state.id);
    print_detail("pid", state.pid);
    Ok(())
}

fn print_section(title: &str) {
    println!("{title}");
    println!("{}", "=".repeat(title.len()));
}

fn print_subsection(title: &str) {
    println!("{title}");
    println!("{}", "-".repeat(title.len()));
}

fn print_detail(label: &str, value: impl Display) {
    println!("{label:>10}: {value}");
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

    humantime::format_duration(Duration::from_secs(rounded_secs)).to_string()
}
