use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use std::fmt::Write as FmtWrite;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crate::schedule::write_alarm_time;
use crate::types::{ForegroundConfig, TimeNotation, TimerStyle};

pub struct ForegroundRenderer {
    enabled: bool,
    settings: ForegroundConfig,
    timezone: Tz,
    time_notation: TimeNotation,
    target_local: DateTime<Tz>,
    target_utc: DateTime<Utc>,
    spec_text: String,
    title_line: String,
    input_line: String,
    current_line: String,
    target_line: String,
    remaining_line: String,
    rendered_lines: usize,
}

impl ForegroundRenderer {
    pub fn new(
        settings: ForegroundConfig,
        timezone: Tz,
        time_notation: TimeNotation,
        target_local: DateTime<Tz>,
        spec_text: &str,
    ) -> Self {
        Self {
            enabled: io::stdout().is_terminal(),
            settings,
            timezone,
            time_notation,
            target_local,
            target_utc: target_local.with_timezone(&Utc),
            spec_text: spec_text.to_string(),
            title_line: String::from("tix | foreground"),
            input_line: String::with_capacity(spec_text.len() + 7),
            current_line: String::with_capacity(48),
            target_line: String::with_capacity(48),
            remaining_line: String::with_capacity(32),
            rendered_lines: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.settings.effective_refresh_interval_ms())
    }

    pub fn render(&mut self, now_utc: DateTime<Utc>) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let now_local = now_utc.with_timezone(&self.timezone);
        let remaining = (self.target_utc - now_utc)
            .to_std()
            .unwrap_or(Duration::ZERO);
        let mut lines = [&self.title_line[..], "", "", "", ""];
        let mut line_count = 1_usize;

        if self.settings.show_input {
            self.input_line.clear();
            self.input_line.push_str("input: ");
            self.input_line.push_str(&self.spec_text);
            lines[line_count] = &self.input_line;
            line_count += 1;
        }
        if self.settings.show_current_datetime {
            self.current_line.clear();
            self.current_line.push_str("current: ");
            write_alarm_time(now_local, self.time_notation, &mut self.current_line);
            lines[line_count] = &self.current_line;
            line_count += 1;
        }
        if self.settings.show_target_datetime {
            self.target_line.clear();
            self.target_line.push_str("target: ");
            write_alarm_time(self.target_local, self.time_notation, &mut self.target_line);
            lines[line_count] = &self.target_line;
            line_count += 1;
        }
        if self.settings.show_remaining {
            self.remaining_line.clear();
            self.remaining_line.push_str("remaining: ");
            format_remaining_into(
                remaining,
                self.settings.timer_style,
                &mut self.remaining_line,
            );
            lines[line_count] = &self.remaining_line;
            line_count += 1;
        }

        let rendered_lines = self.redraw(&lines[..line_count])?;
        self.rendered_lines = rendered_lines;
        Ok(())
    }

    pub fn clear(&mut self) -> io::Result<()> {
        if !self.enabled || self.rendered_lines == 0 {
            return Ok(());
        }

        let mut stdout = io::stdout().lock();
        write!(stdout, "\r")?;
        if self.rendered_lines > 1 {
            write!(stdout, "\x1b[{}A", self.rendered_lines - 1)?;
        }
        write!(stdout, "\x1b[J")?;
        stdout.flush()?;
        self.rendered_lines = 0;
        Ok(())
    }

    fn redraw(&self, lines: &[&str]) -> io::Result<usize> {
        let mut stdout = io::stdout().lock();
        if self.rendered_lines > 0 {
            write!(stdout, "\r")?;
            if self.rendered_lines > 1 {
                write!(stdout, "\x1b[{}A", self.rendered_lines - 1)?;
            }
            write!(stdout, "\x1b[J")?;
        }

        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stdout)?;
            }
            write!(stdout, "{line}")?;
        }
        stdout.flush()?;
        Ok(lines.len())
    }
}

#[cfg(test)]
fn format_remaining(remaining: Duration, timer_style: TimerStyle) -> String {
    let mut rendered = String::with_capacity(24);
    format_remaining_into(remaining, timer_style, &mut rendered);
    rendered
}

fn format_remaining_into(remaining: Duration, timer_style: TimerStyle, out: &mut String) {
    match timer_style {
        TimerStyle::Digital => format_remaining_digital_into(remaining, out),
        TimerStyle::Human => format_remaining_human_into(remaining, out),
    }
}

fn format_remaining_digital_into(remaining: Duration, out: &mut String) {
    let total_seconds = remaining.as_secs();
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        let _ = write!(out, "{days}d {hours:02}:{minutes:02}:{seconds:02}");
    } else {
        let _ = write!(out, "{hours:02}:{minutes:02}:{seconds:02}");
    }
}

fn format_remaining_human_into(remaining: Duration, out: &mut String) {
    let total_seconds = remaining.as_secs();
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        let _ = write!(out, "{days}d");
    }
    if hours > 0 || !out.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = write!(out, "{hours}h");
    }
    if minutes > 0 || !out.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = write!(out, "{minutes}m");
    }
    if !out.is_empty() {
        out.push(' ');
    }
    let _ = write!(out, "{seconds}s");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_remaining_formatting_matches_expected_text() {
        assert_eq!(
            format_remaining(Duration::from_secs(90_061), TimerStyle::Human),
            "1d 1h 1m 1s"
        );
    }

    #[test]
    fn digital_remaining_formatting_matches_expected_text() {
        assert_eq!(
            format_remaining(Duration::from_secs(90_061), TimerStyle::Digital),
            "1d 01:01:01"
        );
    }
}
