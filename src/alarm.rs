use chrono::{DateTime, Utc};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source, source::SineWave};
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::resolve_sound_file_path;
use crate::display::ForegroundRenderer;
use crate::state::ActiveAlarmGuard;
use crate::types::{AlarmAudioConfig, AppResult, StopControl};

pub fn run_alarm_session(
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: &AlarmAudioConfig,
    renderer: Option<&mut ForegroundRenderer>,
    log_events: bool,
    detached: bool,
) -> AppResult<()> {
    let control = install_stop_signal_handler()?;
    wait_until(target_utc, &control, renderer)?;
    if control.stop.load(Ordering::Relaxed) {
        if log_events {
            println!("alarm cancelled before trigger.");
        }
        return Ok(());
    }

    if log_events {
        println!("alarm ringing. press ctrl-c to stop.");
    }
    ring_alarm(audio, auto_stop_seconds, &control, log_events, detached);
    Ok(())
}

pub fn run_background_worker(
    alarm_id: String,
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: AlarmAudioConfig,
) -> AppResult<()> {
    let _guard = ActiveAlarmGuard::new(alarm_id)?;
    run_alarm_session(target_utc, auto_stop_seconds, &audio, None, false, true)
}

pub fn test_alarm_audio(audio: &AlarmAudioConfig) -> AppResult<()> {
    let audio = audio.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);

    thread::spawn(move || {
        let result = match AlarmPlayer::new(&audio) {
            Ok(mut player) => player.start_test(),
            Err(err) => {
                eprintln!("tix: audio backend unavailable ({err}); using terminal bell fallback");
                bell_pulse();
                Ok(())
            }
        };
        let _ = done_tx.send(result);
    });

    match done_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("tix: volume test timed out; using terminal bell fallback");
            bell_pulse();
            Ok(())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("volume test worker disconnected unexpectedly".to_string())
        }
    }
}

fn install_stop_signal_handler() -> AppResult<StopControl> {
    let stop = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    let signal_stop = stop.clone();

    ctrlc::set_handler(move || {
        signal_stop.store(true, Ordering::Relaxed);
        let _ = wake_tx.try_send(());
    })
    .map_err(|err| format!("failed to install signal handler: {err}"))?;

    Ok(StopControl { stop, wake_rx })
}

fn wait_until(
    target_utc: DateTime<Utc>,
    control: &StopControl,
    mut renderer: Option<&mut ForegroundRenderer>,
) -> AppResult<()> {
    if let Some(renderer) = renderer.as_deref_mut() {
        renderer
            .render(Utc::now())
            .map_err(|err| format!("failed to render foreground display: {err}"))?;
    }

    while !control.stop.load(Ordering::Relaxed) {
        let now = Utc::now();
        if now >= target_utc {
            break;
        }

        if let Some(renderer) = renderer.as_deref_mut() {
            renderer
                .render(now)
                .map_err(|err| format!("failed to render foreground display: {err}"))?;
        }

        let remaining = (target_utc - now).to_std().unwrap_or(Duration::ZERO);
        let coarse = next_wait_slice(remaining);
        let timeout = if let Some(renderer) = renderer.as_deref() {
            coarse.min(renderer.refresh_interval()).min(remaining)
        } else {
            coarse.min(remaining)
        };

        match control.wake_rx.recv_timeout(timeout) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some(renderer) = renderer {
        renderer
            .clear()
            .map_err(|err| format!("failed to clear foreground display: {err}"))?;
    }
    Ok(())
}

fn next_wait_slice(remaining: Duration) -> Duration {
    if remaining > Duration::from_secs(300) {
        Duration::from_secs(30)
    } else if remaining > Duration::from_secs(30) {
        Duration::from_secs(5)
    } else if remaining > Duration::from_secs(5) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(200)
    }
}

fn ring_alarm(
    audio: &AlarmAudioConfig,
    auto_stop_seconds: u64,
    control: &StopControl,
    log_events: bool,
    detached: bool,
) {
    let auto_stop = if auto_stop_seconds == 0 {
        None
    } else {
        Some(Duration::from_secs(auto_stop_seconds))
    };
    let started = Instant::now();

    let mut player = match AlarmPlayer::new(audio) {
        Ok(player) => player,
        Err(err) => {
            eprintln!("tix: audio backend unavailable ({err}); using terminal bell fallback");
            AlarmPlayer::BellOnly {
                next_pulse_at: Instant::now(),
                detached,
                notified: false,
            }
        }
    };

    player.start_ringing();

    while !control.stop.load(Ordering::Relaxed) {
        if let Some(limit) = auto_stop
            && started.elapsed() >= limit
        {
            if log_events {
                println!("alarm auto-stopped after {auto_stop_seconds}s.");
            }
            break;
        }

        player.tick();
        match control.wake_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

enum AlarmPlayer {
    Player {
        _sink: MixerDeviceSink,
        player: Player,
        playback: PlaybackKind,
    },
    BellOnly {
        next_pulse_at: Instant,
        detached: bool,
        notified: bool,
    },
}

enum PlaybackKind {
    CustomSound { sound_file: String },
    TonePulse,
}

impl AlarmPlayer {
    fn new(audio: &AlarmAudioConfig) -> AppResult<Self> {
        let mut sink = DeviceSinkBuilder::open_default_sink()
            .map_err(|err| format!("failed to open default audio output: {err}"))?;
        sink.log_on_drop(false);
        let player = Player::connect_new(sink.mixer());
        player.set_volume(audio.volume);

        let playback = match &audio.sound_file {
            Some(sound_file) => PlaybackKind::CustomSound {
                sound_file: sound_file.clone(),
            },
            None => PlaybackKind::TonePulse,
        };

        Ok(Self::Player {
            _sink: sink,
            player,
            playback,
        })
    }

    fn start_ringing(&mut self) {
        match self {
            AlarmPlayer::Player {
                player, playback, ..
            } => match playback {
                PlaybackKind::CustomSound { sound_file } => {
                    if let Err(err) = start_looping_sound(player, sound_file) {
                        eprintln!("tix: custom sound failed ({err}); falling back to tone");
                        *playback = PlaybackKind::TonePulse;
                        append_tone_pulse(player);
                    }
                }
                PlaybackKind::TonePulse => append_tone_pulse(player),
            },
            AlarmPlayer::BellOnly {
                detached,
                notified,
                ..
            } => {
                if *detached && !*notified {
                    send_background_alarm_notification();
                    *notified = true;
                }
            }
        }
    }

    fn start_test(&mut self) -> AppResult<()> {
        match self {
            AlarmPlayer::Player {
                player, playback, ..
            } => {
                let wait_for = match playback {
                    PlaybackKind::CustomSound { sound_file } => {
                        if let Err(err) = start_test_sound(player, sound_file) {
                            eprintln!(
                                "tix: custom sound test failed ({err}); testing fallback tone"
                            );
                            append_test_tone(player);
                            Duration::from_millis(900)
                        } else {
                            Duration::from_secs(2)
                        }
                    }
                    PlaybackKind::TonePulse => {
                        append_test_tone(player);
                        Duration::from_millis(900)
                    }
                };
                thread::sleep(wait_for);
                Ok(())
            }
            AlarmPlayer::BellOnly { .. } => {
                bell_pulse();
                Ok(())
            }
        }
    }

    fn tick(&mut self) {
        match self {
            AlarmPlayer::Player {
                player, playback, ..
            } => {
                if matches!(playback, PlaybackKind::TonePulse) && player.empty() {
                    append_tone_pulse(player);
                }
            }
            AlarmPlayer::BellOnly {
                next_pulse_at,
                detached,
                notified,
            } => {
                if *detached && !*notified {
                    send_background_alarm_notification();
                    *notified = true;
                }
                let now = Instant::now();
                if now >= *next_pulse_at {
                    bell_pulse();
                    *next_pulse_at = now + Duration::from_secs(1);
                }
            }
        }
    }
}

fn start_looping_sound(player: &Player, sound_file: &str) -> AppResult<()> {
    let file = open_sound_file(sound_file)?;
    let decoder = Decoder::new_looped(BufReader::new(file))
        .map_err(|err| format!("failed to decode sound file `{sound_file}`: {err}"))?;
    player.append(decoder);
    Ok(())
}

fn start_test_sound(player: &Player, sound_file: &str) -> AppResult<()> {
    let file = open_sound_file(sound_file)?;
    let decoder = Decoder::new(BufReader::new(file))
        .map_err(|err| format!("failed to decode sound file `{sound_file}`: {err}"))?;
    player.append(decoder.take_duration(Duration::from_secs(2)));
    Ok(())
}

fn open_sound_file(sound_file: &str) -> AppResult<File> {
    let path = resolve_sound_file_path(sound_file)?;
    File::open(&path).map_err(|err| format!("failed to open sound file {}: {err}", path.display()))
}

fn append_tone_pulse(player: &Player) {
    player.append(
        SineWave::new(880.0)
            .take_duration(Duration::from_millis(350))
            .amplify(1.0),
    );
}

fn append_test_tone(player: &Player) {
    player.append(
        SineWave::new(880.0)
            .take_duration(Duration::from_millis(750))
            .amplify(1.0),
    );
}

fn bell_pulse() {
    eprint!("\x07");
    let _ = io::stderr().flush();
}

fn send_background_alarm_notification() {
    if let Err(err) = notify_background_alarm() {
        eprintln!("tix: background fallback notification failed ({err})");
    }
}

#[cfg(target_os = "macos")]
fn notify_background_alarm() -> io::Result<()> {
    let status = ProcessCommand::new("/usr/bin/osascript")
        .arg("-e")
        .arg("display notification \"alarm ringing\" with title \"tix\"")
        .arg("-e")
        .arg("beep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("osascript exited with a non-zero status"))
    }
}

#[cfg(target_os = "linux")]
fn notify_background_alarm() -> io::Result<()> {
    let status = ProcessCommand::new("notify-send")
        .arg("tix")
        .arg("alarm ringing")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("notify-send exited with a non-zero status"))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn notify_background_alarm() -> io::Result<()> {
    Err(io::Error::other(
        "background notifications are not supported on this platform",
    ))
}
