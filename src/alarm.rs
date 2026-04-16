use chrono::{DateTime, Utc};
#[cfg(target_os = "linux")]
use notify_rust::{Hint, NotificationHandle};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use notify_rust::{Notification, Timeout};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source, source::SineWave};
use std::fs::File;
use std::io::{self, BufReader, Write};
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
use crate::types::{AlarmAudioConfig, AlarmNotificationConfig, AppResult, StopControl};

pub fn run_alarm_session(
    alarm_id: Option<&str>,
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: &AlarmAudioConfig,
    renderer: Option<&mut ForegroundRenderer>,
    log_events: bool,
    detached: bool,
    notifications: AlarmNotificationConfig,
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
    if detached {
        send_background_alarm_notification(alarm_id, notifications, &control);
    }
    ring_alarm(audio, auto_stop_seconds, &control, log_events);
    Ok(())
}

pub fn run_background_worker(
    alarm_id: String,
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: AlarmAudioConfig,
    notifications: AlarmNotificationConfig,
) -> AppResult<()> {
    let _guard = ActiveAlarmGuard::new(alarm_id.clone())?;
    run_alarm_session(
        Some(&alarm_id),
        target_utc,
        auto_stop_seconds,
        &audio,
        None,
        false,
        true,
        notifications,
    )
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
    let signal_wake_tx = wake_tx.clone();

    ctrlc::set_handler(move || {
        signal_stop.store(true, Ordering::Relaxed);
        let _ = signal_wake_tx.try_send(());
    })
    .map_err(|err| format!("failed to install signal handler: {err}"))?;

    Ok(StopControl {
        stop,
        wake_tx,
        wake_rx,
    })
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
            AlarmPlayer::BellOnly { .. } => {}
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
            AlarmPlayer::BellOnly { next_pulse_at } => {
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

fn send_background_alarm_notification(
    alarm_id: Option<&str>,
    notifications: AlarmNotificationConfig,
    control: &StopControl,
) {
    if !notifications.enabled {
        return;
    }

    if let Err(err) = notify_background_alarm(alarm_id, notifications, control) {
        eprintln!("tix: background notification failed ({err})");
    }
}

#[cfg(target_os = "macos")]
fn notify_background_alarm(
    alarm_id: Option<&str>,
    notifications: AlarmNotificationConfig,
    _control: &StopControl,
) -> io::Result<()> {
    let mut notification = Notification::new();
    notification
        .summary("tix alarm")
        .body(&notification_body(alarm_id, false, false))
        .timeout(notification_timeout(notifications.timeout_ms))
        .show()
        .map(|_| ())
        .map_err(|err| io::Error::other(format!("desktop notification failed: {err}")))
}

#[cfg(target_os = "linux")]
fn notify_background_alarm(
    alarm_id: Option<&str>,
    notifications: AlarmNotificationConfig,
    control: &StopControl,
) -> io::Result<()> {
    let mut notification = Notification::new();
    notification
        .summary("tix alarm")
        .body(&notification_body(
            alarm_id,
            notifications.clickable,
            notifications.show_stop_button,
        ))
        .timeout(notification_timeout(notifications.timeout_ms));

    if notifications.timeout_ms == 0 {
        notification.hint(Hint::Resident(true));
    }
    if notifications.clickable {
        // "default" maps body-click activation on XDG notification servers.
        notification.action("default", "Stop alarm");
        if notifications.show_stop_button {
            notification.action("stop", "Stop");
        }
    }

    let handle = notification
        .show()
        .map_err(|err| io::Error::other(format!("desktop notification failed: {err}")))?;
    if notifications.clickable {
        spawn_notification_action_listener(handle, control);
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn notify_background_alarm(
    _alarm_id: Option<&str>,
    _notifications: AlarmNotificationConfig,
    _control: &StopControl,
) -> io::Result<()> {
    Err(io::Error::other(
        "background notifications are not supported on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn notification_timeout(timeout_ms: u32) -> Timeout {
    if timeout_ms == 0 {
        Timeout::Never
    } else {
        Timeout::Milliseconds(timeout_ms)
    }
}

fn notification_body(alarm_id: Option<&str>, clickable: bool, show_stop_button: bool) -> String {
    match alarm_id {
        Some(alarm_id) if clickable && show_stop_button => {
            format!("alarm ringing. click or press stop or run tix stop {alarm_id}")
        }
        Some(alarm_id) if clickable => {
            format!("alarm ringing. click to stop or run tix stop {alarm_id}")
        }
        Some(alarm_id) => format!("alarm ringing. run tix stop {alarm_id}"),
        None => "alarm ringing".to_string(),
    }
}

#[cfg(target_os = "linux")]
fn spawn_notification_action_listener(handle: NotificationHandle, control: &StopControl) {
    let stop = control.stop.clone();
    let wake_tx = control.wake_tx.clone();

    thread::spawn(move || {
        handle.wait_for_action(move |action| {
            if matches!(action, "default" | "stop") {
                stop.store(true, Ordering::Relaxed);
                let _ = wake_tx.try_send(());
            }
        });
    });
}
