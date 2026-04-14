use chrono::{DateTime, Utc};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(target_os = "linux")]
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::mem;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command as ProcessCommand, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::config_root;
use crate::types::{ActiveAlarmState, AlarmAudioConfig, AppResult};

pub fn schedule_background_alarm(
    spec_text: &str,
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: &AlarmAudioConfig,
) -> AppResult<ActiveAlarmState> {
    let alarm_id = generate_alarm_id()?;
    let pid = spawn_background_worker(&alarm_id, target_utc, auto_stop_seconds, audio)?;
    let state = ActiveAlarmState {
        id: alarm_id,
        pid,
        spec_text: spec_text.to_string(),
        target_utc: target_utc.to_rfc3339(),
        created_at_utc: Utc::now().to_rfc3339(),
        auto_stop_seconds,
        volume: audio.volume,
        sound_file: audio.sound_file.clone(),
    };

    let path = alarm_state_file_path(&state.id)?;
    if let Err(err) = save_alarm_state(&path, &state) {
        let _ = terminate_process(pid);
        return Err(err);
    }

    if !is_tracked_worker_process(state.pid, Some(&state.id))? {
        clear_alarm_state(&path)?;
        return Err("background alarm worker exited before it could be tracked".to_string());
    }

    Ok(state)
}

pub fn active_alarm_states() -> AppResult<Vec<ActiveAlarmState>> {
    let dir = alarms_root()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();
    let entries = fs::read_dir(&dir)
        .map_err(|err| format!("failed to read alarms dir {}: {err}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to read alarms dir entry: {err}"))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }

        let Some(state) = load_alarm_state(&path)? else {
            continue;
        };

        if !is_tracked_worker_process(state.pid, Some(&state.id))? {
            clear_alarm_state(&path)?;
            continue;
        }

        states.push(state);
    }

    states.sort_by(|left, right| {
        left.target_utc
            .cmp(&right.target_utc)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(states)
}

pub fn resolve_alarm_selector(
    states: &[ActiveAlarmState],
    selector: &str,
) -> AppResult<ActiveAlarmState> {
    if let Some(state) = states.iter().find(|state| state.id == selector) {
        return Ok(state.clone());
    }

    let mut matches = states
        .iter()
        .filter(|state| state.id.starts_with(selector))
        .cloned()
        .collect::<Vec<_>>();

    match matches.len() {
        0 => Err(format!("no active alarm matches `{selector}`")),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "alarm selector `{selector}` is ambiguous; use a longer prefix or the full id"
        )),
    }
}

pub fn remove_alarm_state_by_id(alarm_id: &str) -> AppResult<()> {
    clear_alarm_state(&alarm_state_file_path(alarm_id)?)
}

pub fn terminate_process(pid: u32) -> AppResult<()> {
    terminate_process_impl(pid)
}

pub fn parse_state_target_utc(state: &ActiveAlarmState) -> AppResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&state.target_utc)
        .map(|target| target.with_timezone(&Utc))
        .map_err(|err| format!("invalid stored target time `{}`: {err}", state.target_utc))
}

pub struct ActiveAlarmGuard {
    path: PathBuf,
    alarm_id: String,
}

impl ActiveAlarmGuard {
    pub fn new(alarm_id: String) -> AppResult<Self> {
        Ok(Self {
            path: alarm_state_file_path(&alarm_id)?,
            alarm_id,
        })
    }
}

impl Drop for ActiveAlarmGuard {
    fn drop(&mut self) {
        let _ = clear_alarm_state_if_matches(&self.path, &self.alarm_id);
    }
}

fn alarms_root() -> AppResult<PathBuf> {
    Ok(config_root()?.join("alarms"))
}

fn alarm_state_file_path(alarm_id: &str) -> AppResult<PathBuf> {
    Ok(alarms_root()?.join(format!("{alarm_id}.toml")))
}

fn generate_alarm_id() -> AppResult<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system time error while generating alarm id: {err}"))?;
    Ok(format!("{:x}-{:x}", now.as_nanos(), process::id()))
}

fn load_alarm_state(path: &Path) -> AppResult<Option<ActiveAlarmState>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)
        .map_err(|err| format!("failed to read alarm state {}: {err}", path.display()))?;
    let state = toml::from_str(&raw)
        .map_err(|err| format!("failed to parse alarm state {}: {err}", path.display()))?;
    Ok(Some(state))
}

fn save_alarm_state(path: &Path, state: &ActiveAlarmState) -> AppResult<()> {
    let Some(parent) = path.parent() else {
        return Err(format!(
            "alarm state path {} has no parent directory",
            path.display()
        ));
    };
    fs::create_dir_all(parent).map_err(|err| {
        format!(
            "failed to create alarm state dir {}: {err}",
            parent.display()
        )
    })?;

    let mut rendered = toml::to_string_pretty(state)
        .map_err(|err| format!("failed to render alarm state to TOML: {err}"))?;
    rendered.push('\n');
    fs::write(path, rendered)
        .map_err(|err| format!("failed to write alarm state {}: {err}", path.display()))
}

fn clear_alarm_state(path: &Path) -> AppResult<()> {
    if !path.exists() {
        return Ok(());
    }

    fs::remove_file(path)
        .map_err(|err| format!("failed to remove alarm state {}: {err}", path.display()))
}

fn clear_alarm_state_if_matches(path: &Path, alarm_id: &str) -> AppResult<()> {
    let Some(state) = load_alarm_state(path)? else {
        return Ok(());
    };

    if state.id == alarm_id {
        clear_alarm_state(path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn spawn_background_worker(
    alarm_id: &str,
    target_utc: DateTime<Utc>,
    auto_stop_seconds: u64,
    audio: &AlarmAudioConfig,
) -> AppResult<u32> {
    let executable =
        env::current_exe().map_err(|err| format!("failed to resolve current executable: {err}"))?;

    let stdin_null = OpenOptions::new()
        .read(true)
        .open("/dev/null")
        .map_err(|err| format!("failed to open /dev/null for stdin: {err}"))?;
    let stdout_null = OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|err| format!("failed to open /dev/null for stdout: {err}"))?;

    let mut command = ProcessCommand::new(executable);
    command
        .arg("__worker")
        .arg("--alarm-id")
        .arg(alarm_id)
        .arg("--target-utc")
        .arg(target_utc.to_rfc3339())
        .arg("--auto-stop-seconds")
        .arg(auto_stop_seconds.to_string())
        .arg("--volume")
        .arg(audio.volume.to_string())
        .stdin(Stdio::from(stdin_null))
        .stdout(Stdio::from(stdout_null))
        .stderr(Stdio::inherit());

    if let Some(sound_file) = &audio.sound_file {
        command.arg("--sound-file").arg(sound_file);
    }

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }

    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn background alarm worker: {err}"))?;
    Ok(child.id())
}

#[cfg(not(unix))]
fn spawn_background_worker(
    _alarm_id: &str,
    _target_utc: DateTime<Utc>,
    _auto_stop_seconds: u64,
    _audio: &AlarmAudioConfig,
) -> AppResult<u32> {
    Err("background alarms are only supported on Unix right now; use --foreground".to_string())
}

#[cfg(unix)]
fn terminate_process_impl(pid: u32) -> AppResult<()> {
    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(()),
        _ => Err(format!("failed to stop alarm worker {pid}: {err}")),
    }
}

#[cfg(not(unix))]
fn terminate_process_impl(_pid: u32) -> AppResult<()> {
    Err("stopping background alarms is only supported on Unix right now".to_string())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> AppResult<bool> {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return Ok(true);
    }

    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(false),
        Some(code) if code == libc::EPERM => Ok(true),
        _ => Err(format!("failed to inspect process {pid}: {err}")),
    }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> AppResult<bool> {
    Ok(false)
}

fn is_tracked_worker_process(pid: u32, alarm_id: Option<&str>) -> AppResult<bool> {
    if !process_is_alive(pid)? {
        return Ok(false);
    }
    process_matches_tix_worker(pid, alarm_id)
}

#[cfg(target_os = "linux")]
fn process_matches_tix_worker(pid: u32, alarm_id: Option<&str>) -> AppResult<bool> {
    let expected_executable = current_executable_path()?;
    let process_executable = match linux_process_executable_path(pid) {
        Ok(path) => path,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to inspect process executable for {pid}: {err}"
            ));
        }
    };
    if !paths_equivalent(&expected_executable, &process_executable) {
        return Ok(false);
    }

    let cmdline_path = PathBuf::from(format!("/proc/{pid}/cmdline"));
    let raw = match fs::read(&cmdline_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to inspect process command line {}: {err}",
                cmdline_path.display()
            ));
        }
    };

    Ok(cmdline_matches_worker(&raw, &expected_executable, alarm_id))
}

#[cfg(target_os = "macos")]
fn process_matches_tix_worker(pid: u32, alarm_id: Option<&str>) -> AppResult<bool> {
    let expected_executable = current_executable_path()?;
    let process_executable = match macos_process_executable_path(pid) {
        Ok(path) => path,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to inspect process executable for {pid}: {err}"
            ));
        }
    };
    if !paths_equivalent(&expected_executable, &process_executable) {
        return Ok(false);
    }

    let raw = match macos_process_arguments(pid) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to inspect process arguments for {pid}: {err}"
            ));
        }
    };

    Ok(macos_procargs_match_worker(
        &raw,
        &expected_executable,
        alarm_id,
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_matches_tix_worker(_pid: u32, _alarm_id: Option<&str>) -> AppResult<bool> {
    Ok(false)
}

fn current_executable_path() -> AppResult<PathBuf> {
    env::current_exe().map_err(|err| format!("failed to resolve current executable: {err}"))
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }

    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(any(test, target_os = "linux"))]
fn cmdline_matches_worker(raw: &[u8], expected_executable: &Path, alarm_id: Option<&str>) -> bool {
    let mut parts = split_nul_terminated(raw);
    let Some(executable) = parts.next() else {
        return false;
    };
    if !path_bytes_match(executable, expected_executable) {
        return false;
    }

    let mut has_worker_marker = false;
    let mut has_alarm_id = alarm_id.is_none();

    for part in parts {
        if part == b"__worker" {
            has_worker_marker = true;
        }
        if let Some(alarm_id) = alarm_id
            && part == alarm_id.as_bytes()
        {
            has_alarm_id = true;
        }
    }

    has_worker_marker && has_alarm_id
}

#[cfg(any(test, target_os = "linux"))]
fn split_nul_terminated(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    raw.split(|byte| *byte == 0).filter(|part| !part.is_empty())
}

fn path_bytes_match(raw: &[u8], expected_path: &Path) -> bool {
    paths_equivalent(Path::new(OsStr::from_bytes(raw)), expected_path)
}

#[cfg(target_os = "linux")]
fn linux_process_executable_path(pid: u32) -> io::Result<PathBuf> {
    fs::read_link(PathBuf::from(format!("/proc/{pid}/exe")))
}

#[cfg(target_os = "macos")]
fn macos_process_executable_path(pid: u32) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        libc::proc_pidpath(
            pid as i32,
            buffer.as_mut_ptr() as *mut _,
            libc::PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };

    if written <= 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(code) if code == libc::ESRCH => io::Error::from(io::ErrorKind::NotFound),
            _ => err,
        });
    }

    buffer.truncate(written as usize);
    Ok(PathBuf::from(OsStr::from_bytes(&buffer)))
}

#[cfg(target_os = "macos")]
fn macos_process_arguments(pid: u32) -> io::Result<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut size = 0_usize;
    let size_status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if size_status == -1 {
        return Err(io::Error::last_os_error());
    }

    let mut raw = vec![0_u8; size];
    let read_status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            raw.as_mut_ptr() as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read_status == -1 {
        return Err(io::Error::last_os_error());
    }

    raw.truncate(size);
    Ok(raw)
}

#[cfg(target_os = "macos")]
fn macos_procargs_match_worker(
    raw: &[u8],
    expected_executable: &Path,
    alarm_id: Option<&str>,
) -> bool {
    let Some(argument_count_bytes) = raw.get(..mem::size_of::<libc::c_int>()) else {
        return false;
    };
    let mut argc = 0_i32;
    unsafe {
        libc::memcpy(
            &mut argc as *mut _ as *mut _,
            argument_count_bytes.as_ptr() as *const _,
            mem::size_of::<libc::c_int>(),
        );
    }
    if argc < 1 {
        return false;
    }

    let Some((_, mut remainder)) = next_nul_terminated(&raw[mem::size_of::<libc::c_int>()..]) else {
        return false;
    };
    while remainder.first() == Some(&0) {
        remainder = &remainder[1..];
    }

    let mut has_worker_marker = false;
    let mut has_alarm_id = alarm_id.is_none();
    let mut saw_executable = false;

    for index in 0..argc {
        let Some((argument, next)) = next_nul_terminated(remainder) else {
            return false;
        };
        if index == 0 {
            saw_executable = path_bytes_match(argument, expected_executable);
        } else if argument == b"__worker" {
            has_worker_marker = true;
        } else if let Some(alarm_id) = alarm_id
            && argument == alarm_id.as_bytes()
        {
            has_alarm_id = true;
        }
        remainder = next;
    }

    saw_executable && has_worker_marker && has_alarm_id
}

#[cfg(target_os = "macos")]
fn next_nul_terminated(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = raw.iter().position(|byte| *byte == 0)?;
    Some((&raw[..end], &raw[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state(id: &str) -> ActiveAlarmState {
        ActiveAlarmState {
            id: id.to_string(),
            pid: 1,
            spec_text: "10m".to_string(),
            target_utc: "2026-03-12T12:30:00Z".to_string(),
            created_at_utc: "2026-03-12T12:20:00Z".to_string(),
            auto_stop_seconds: 0,
            volume: 0.3,
            sound_file: None,
        }
    }

    #[test]
    fn exact_alarm_selector_matches() {
        let states = vec![sample_state("abc123"), sample_state("def456")];
        let state = resolve_alarm_selector(&states, "def456").unwrap();
        assert_eq!(state.id, "def456");
    }

    #[test]
    fn unique_prefix_alarm_selector_matches() {
        let states = vec![sample_state("abc123"), sample_state("def456")];
        let state = resolve_alarm_selector(&states, "ab").unwrap();
        assert_eq!(state.id, "abc123");
    }

    #[test]
    fn ambiguous_prefix_alarm_selector_rejects() {
        let states = vec![sample_state("abc123"), sample_state("abd456")];
        let result = resolve_alarm_selector(&states, "a");
        assert!(result.is_err());
    }

    #[test]
    fn missing_alarm_selector_rejects() {
        let states = vec![sample_state("abc123")];
        let result = resolve_alarm_selector(&states, "zzz");
        assert!(result.is_err());
    }

    #[test]
    fn linux_cmdline_match_requires_executable_marker_and_alarm_id() {
        let executable = Path::new("/tmp/tix");
        let raw = b"/tmp/tix\0__worker\0--alarm-id\0abc123\0";

        assert!(cmdline_matches_worker(raw, executable, Some("abc123")));
        assert!(!cmdline_matches_worker(raw, executable, Some("zzz")));
        assert!(!cmdline_matches_worker(
            b"/tmp/other\0__worker\0--alarm-id\0abc123\0",
            executable,
            Some("abc123")
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_procargs_match_requires_executable_marker_and_alarm_id() {
        let executable = Path::new("/tmp/tix");
        let mut raw = Vec::new();
        raw.extend_from_slice(&(4_i32).to_ne_bytes());
        raw.extend_from_slice(b"/tmp/tix\0");
        raw.extend_from_slice(&[0, 0]);
        raw.extend_from_slice(b"/tmp/tix\0__worker\0--alarm-id\0abc123\0");

        assert!(macos_procargs_match_worker(&raw, executable, Some("abc123")));
        assert!(!macos_procargs_match_worker(&raw, executable, Some("zzz")));
    }
}
