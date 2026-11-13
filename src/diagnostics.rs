//! A small rotating log of what the app did, to attach to bug reports:
//! scans, patch writes and restores, AI calls, launches and update checks.
//! It never holds API keys, prompts or file contents; lines are scrubbed of
//! anything that looks like a key as a last guard.
//!
//! The log is `drukhal-manager.log` in `~/.local/share/DrukhalManager/logs`
//! on the host (`%LOCALAPPDATA%\DrukhalManager\logs` on Windows), with up
//! to two older files beside it, each about 1 MB.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How big a log file grows before it's rotated.
const MAX_BYTES: u64 = 1024 * 1024;
/// How many older files are kept beside the current one.
const KEEP: usize = 2;
const NAME: &str = "drukhal-manager";

/// Writes a line to the log, and to stderr. `area` is a short tag, such as
/// `scan` or `ai`.
macro_rules! log {
    ($area:expr, $($arg:tt)*) => {
        $crate::diagnostics::write($area, &format!($($arg)*))
    };
}
pub(crate) use log;

pub fn dir() -> PathBuf {
    crate::platform::data_dir().join("logs")
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

pub fn write(area: &str, message: &str) {
    let line = line(SystemTime::now(), area, message);
    eprint!("{line}");
    // Tests don't touch the user's log.
    if cfg!(test) {
        return;
    }
    let Ok(mut log) = LOG.lock() else { return };
    let log = log.get_or_insert_with(|| Log::new(dir(), MAX_BYTES));
    // A log that can't be written mustn't get in the way.
    let _ = log.append(&line);
}

/// Logs that `what` was done, or why it failed, and passes `result` on.
pub fn outcome<T, E: std::fmt::Display>(area: &str, what: impl FnOnce() -> String, result: Result<T, E>) -> Result<T, E> {
    match &result {
        Ok(_) => write(area, &what()),
        Err(e) => write(area, &format!("Failed: {}: {e}", what())),
    }
    result
}

/// The last `n` lines logged, oldest first, across rotated files.
pub fn tail(n: usize) -> Vec<String> {
    Log::new(dir(), MAX_BYTES).tail(n)
}

fn line(at: SystemTime, area: &str, message: &str) -> String {
    let message = scrub(message.trim()).replace('\n', "\n    ");
    format!("{} [{area}] {message}\n", timestamp(at))
}

/// `at` in UTC, as `2026-11-12 19:48:46`.
fn timestamp(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

/// Hides anything that looks like a secret: `key=` query values, bearer
/// tokens, and `sk-` style API keys.
fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let secret_end = |s: &str| s.find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')' | ',')).unwrap_or(s.len());
    loop {
        let found = ["key=", "Bearer ", "sk-"].iter().filter_map(|p| rest.find(p).map(|i| (i, *p))).min();
        let Some((at, prefix)) = found else { break };
        // `sk-` only counts at the start of a word, so `task-1` is kept.
        let starts_word = at == 0 || !rest[..at].ends_with(|c: char| c.is_alphanumeric());
        let head = at + prefix.len();
        out.push_str(&rest[..head]);
        rest = &rest[head..];
        if prefix != "sk-" || starts_word {
            let end = secret_end(rest);
            if end > 0 {
                out.push_str("[hidden]");
                rest = &rest[end..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Log {
    dir: PathBuf,
    max: u64,
    file: Option<File>,
    size: u64,
}

impl Log {
    fn new(dir: PathBuf, max: u64) -> Self {
        Log { dir, max, file: None, size: 0 }
    }

    /// The current file, then the older ones, `.1` the most recent.
    fn path(&self, age: usize) -> PathBuf {
        if age == 0 { self.dir.join(format!("{NAME}.log")) } else { self.dir.join(format!("{NAME}.{age}.log")) }
    }

    fn append(&mut self, line: &str) -> io::Result<()> {
        if self.file.is_none() {
            fs::create_dir_all(&self.dir)?;
            let file = OpenOptions::new().create(true).append(true).open(self.path(0))?;
            self.size = file.metadata()?.len();
            self.file = Some(file);
        }
        if self.size > 0 && self.size + line.len() as u64 > self.max {
            self.file = None;
            self.rotate()?;
            self.file = Some(OpenOptions::new().create(true).append(true).open(self.path(0))?);
            self.size = 0;
        }
        let file = self.file.as_mut().expect("opened above");
        file.write_all(line.as_bytes())?;
        self.size += line.len() as u64;
        Ok(())
    }

    fn rotate(&self) -> io::Result<()> {
        for age in (0..KEEP).rev() {
            let from = self.path(age);
            if from.exists() {
                fs::rename(&from, self.path(age + 1))?;
            }
        }
        Ok(())
    }

    fn tail(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for age in 0..=KEEP {
            if lines.len() >= n {
                break;
            }
            let Ok(text) = fs::read_to_string(self.path(age)) else { continue };
            let older: Vec<String> = text.lines().map(str::to_owned).collect();
            lines.splice(0..0, older);
        }
        let skip = lines.len().saturating_sub(n);
        lines.split_off(skip)
    }
}

/// The text to paste into a bug report: versions, the system, and the last
/// lines of the log.
pub fn report(facts: &[(&str, String)], lines: usize) -> String {
    let mut text = String::new();
    for (name, value) in facts {
        text.push_str(&format!("{name}: {value}\n"));
    }
    let tail = tail(lines);
    text.push_str(&format!("\nLast {} log lines (times in UTC):\n", tail.len()));
    for line in tail {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// The distribution's name (the host's, in the Flatpak), or the Windows
/// version.
pub fn distro() -> String {
    crate::platform::os_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_times_in_utc() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(1_794_512_926)), "2026-11-12 19:48:46");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29 00:00:00", "leap day");
    }

    #[test]
    fn hides_secrets() {
        assert_eq!(scrub("GET https://api.steampowered.com/x?key=ABC123&itemcount=1 failed"), "GET https://api.steampowered.com/x?key=[hidden]&itemcount=1 failed");
        assert_eq!(scrub("authorization: Bearer sk-or-v1-abc def"), "authorization: Bearer [hidden] def");
        assert_eq!(scrub("bad key sk-ant-api03-xyz"), "bad key sk-[hidden]");
        assert_eq!(scrub("task-1 and disk-2 stay"), "task-1 and disk-2 stay");
        assert_eq!(line(UNIX_EPOCH, "ai", "two\nlines"), "1970-01-01 00:00:00 [ai] two\n    lines\n");
    }

    #[test]
    fn rotates_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("drukhal-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut log = Log::new(dir.clone(), 30);
        for i in 0..10 {
            log.append(&format!("line {i:02} ......\n")).unwrap();
        }
        // Two 15-byte lines per file: the current one and two older ones.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 3);
        assert_eq!(fs::read_to_string(log.path(0)).unwrap(), "line 08 ......\nline 09 ......\n");
        assert_eq!(log.tail(3), ["line 07 ......", "line 08 ......", "line 09 ......"]);
        assert_eq!(log.tail(100).len(), 6, "older lines are gone");
        // Reopened, it carries on where it left off.
        let mut again = Log::new(dir.clone(), 30);
        again.append("line 10 ......\n").unwrap();
        assert_eq!(again.tail(2), ["line 09 ......", "line 10 ......"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
