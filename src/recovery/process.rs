//! Best-effort process liveness and lifetime probing.
//!
//! No extra dependencies and no `unsafe` in our code; everything is either
//! plain `/proc` parsing, or the platform's own command-line tools. Where
//! the OS cannot be queried reliably we report [`ProcessState::Unknown`],
//! and the recovery logic treats that as "hands off".
//!
//! The lifetime token is opaque and only meaningful when compared with a
//! token produced by the same code on the same machine. If the token of a
//! running pid differs from the one recorded in a lock file, the pid was
//! reused and the lock owner is gone.

use std::sync::OnceLock;

/// What we know about a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProcessState {
    /// The process appears to be running. `start_token` identifies this
    /// particular lifetime; it changes if the pid is recycled.
    Alive {
        /// Opaque process-lifetime token; `None` when the platform cannot tell.
        start_token: Option<String>,
    },
    /// The process is certainly gone.
    Dead,
    /// Could not tell either way. Never treated as a reason to delete anything.
    Unknown {
        /// Why the probe gave up.
        reason: String,
    },
}

/// Token identifying the lifetime of the current process.
///
/// Cached: the value cannot change while we are alive, and probing costs
/// real time on some platforms.
pub(crate) fn current_start_token() -> Option<String> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| probe_platform_token(std::process::id()))
        .clone()
}

/// Probe an arbitrary pid.
pub(crate) fn probe_process(pid: u32) -> ProcessState {
    #[cfg(target_os = "linux")]
    {
        linux::probe(pid)
    }
    #[cfg(target_os = "windows")]
    {
        windows::probe(pid)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        other_unix::probe(pid)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        ProcessState::Unknown {
            reason: "no process probe for this platform".into(),
        }
    }
}

fn probe_platform_token(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux::start_token(pid)
    }
    #[cfg(target_os = "windows")]
    {
        windows::start_token(pid)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        other_unix::start_token(pid)
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;

    use super::ProcessState;

    pub(super) fn probe(pid: u32) -> ProcessState {
        match fs::metadata(format!("/proc/{pid}")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProcessState::Dead,
            Err(e) => ProcessState::Unknown {
                reason: format!("could not stat /proc/{pid}: {e}"),
            },
            Ok(_) => match start_token(pid) {
                Some(token) => ProcessState::Alive {
                    start_token: Some(token),
                },
                None => ProcessState::Unknown {
                    reason: format!("/proc/{pid} exists but its stat is not readable"),
                },
            },
        }
    }

    pub(super) fn start_token(pid: u32) -> Option<String> {
        let btime = boot_time()?;
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let start = stat_start_time(&stat)?;
        Some(format!("linux:{btime}:{start}"))
    }

    fn boot_time() -> Option<String> {
        let stat = fs::read_to_string("/proc/stat").ok()?;
        stat.lines()
            .find_map(|line| line.strip_prefix("btime "))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    }

    /// Field 22 (`starttime`) of `/proc/<pid>/stat`, in clock ticks.
    ///
    /// Field 2 (`comm`) may contain spaces and parens, so we anchor on the
    /// last `)` and index from field 3.
    fn stat_start_time(stat: &str) -> Option<u64> {
        let close = stat.rfind(')')?;
        stat[close + 1..]
            .split_whitespace()
            .nth(19)
            .and_then(|v| v.parse().ok())
    }

    #[cfg(test)]
    mod tests {
        use super::stat_start_time;

        fn synthetic_stat(start: u64) -> String {
            let mut fields = vec!["S".to_string(), "1".into(), "2".into(), "3".into()];
            // pad through field 21, then put starttime at field 22
            while fields.len() < 19 {
                fields.push("0".into());
            }
            fields.push(start.to_string());
            format!("4242 (proc with spaces) {}", fields.join(" "))
        }

        #[test]
        fn parses_start_time_with_spaces_in_comm() {
            let stat = synthetic_stat(987654321);
            assert_eq!(stat_start_time(&stat), Some(987654321));
        }

        #[test]
        fn parses_start_time_with_parens_in_comm() {
            let stat = synthetic_stat(42).replace("proc with spaces", "weird ) name");
            assert_eq!(stat_start_time(&stat), Some(42));
        }

        #[test]
        fn rejects_garbage() {
            assert_eq!(stat_start_time("not a stat line"), None);
            assert_eq!(stat_start_time(""), None);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::process::Command;

    use super::ProcessState;

    pub(super) fn probe(pid: u32) -> ProcessState {
        match tasklist(pid) {
            Tasklist::Alive => ProcessState::Alive {
                start_token: start_token(pid),
            },
            Tasklist::Dead => ProcessState::Dead,
            Tasklist::Unknown(reason) => ProcessState::Unknown { reason },
        }
    }

    enum Tasklist {
        Alive,
        Dead,
        Unknown(String),
    }

    /// `tasklist` exits 0 and prints a localized info line when nothing
    /// matches, so liveness is decided by parsing the CSV row, never by
    /// the exit code or by matching the info text.
    fn tasklist(pid: u32) -> Tasklist {
        let filter = format!("PID eq {pid}");
        let out = match Command::new("tasklist")
            .args(["/FO", "CSV", "/NH", "/FI", &filter])
            .output()
        {
            Ok(out) => out,
            Err(e) => return Tasklist::Unknown(format!("could not run tasklist: {e}")),
        };
        if !out.status.success() {
            return Tasklist::Unknown(format!("tasklist exited with {}", out.status));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            return Tasklist::Unknown("tasklist returned no output".into());
        }
        if csv_row_has_pid(&stdout, pid) {
            Tasklist::Alive
        } else {
            Tasklist::Dead
        }
    }

    fn csv_row_has_pid(stdout: &str, pid: u32) -> bool {
        let wanted = pid.to_string();
        stdout.lines().any(|line| {
            line.split(',')
                .nth(1)
                .map(|field| field.trim().trim_matches('"').trim() == wanted)
                .unwrap_or(false)
        })
    }

    /// Round-trip ISO-8601 UTC string from .NET; stable for a process lifetime.
    pub(super) fn start_token(pid: u32) -> Option<String> {
        let script = format!("(Get-Process -Id {pid}).StartTime.ToUniversalTime().ToString('o')");
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.trim();
        if line.is_empty() || line.lines().count() != 1 {
            return None;
        }
        Some(format!("windows:{line}"))
    }

    #[cfg(test)]
    mod tests {
        use super::csv_row_has_pid;

        #[test]
        fn csv_parser_finds_pid_even_with_commas_in_mem_column() {
            let out = "\"pwsh.exe\",\"29136\",\"Console\",\"1\",\"72,800 K\"\r\n";
            assert!(csv_row_has_pid(out, 29136));
            assert!(!csv_row_has_pid(out, 1234));
        }

        #[test]
        fn csv_parser_ignores_info_lines() {
            assert!(!csv_row_has_pid("INFO: No tasks are running.\r\n", 29136));
            assert!(!csv_row_has_pid("", 29136));
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod other_unix {
    use std::process::Command;

    use super::ProcessState;

    pub(super) fn probe(pid: u32) -> ProcessState {
        match Command::new("kill").args(["-0", &pid.to_string()]).output() {
            Ok(out) if out.status.success() => ProcessState::Alive {
                start_token: start_token(pid),
            },
            // kill(1) uses exit 1 for "no such process", anything else is ambiguous
            Ok(out) if out.status.code() == Some(1) => ProcessState::Dead,
            Ok(out) => ProcessState::Unknown {
                reason: format!("kill -0 exited with {}", out.status),
            },
            Err(e) => ProcessState::Unknown {
                reason: format!("could not run kill: {e}"),
            },
        }
    }

    pub(super) fn start_token(pid: u32) -> Option<String> {
        let out = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.trim();
        if line.is_empty() {
            None
        } else {
            Some(format!("unix:{line}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_reports_alive_with_a_token() {
        let pid = std::process::id();
        match probe_process(pid) {
            ProcessState::Alive { start_token } => {
                assert!(start_token.is_some(), "current process must have a token");
                assert_eq!(Some(start_token.unwrap()), current_start_token());
            }
            other => panic!("current process reported {other:?}"),
        }
    }

    #[test]
    fn absurd_pid_is_dead_or_unknown_never_alive() {
        // 2^31-2 is beyond any realistic pid on any supported platform
        match probe_process(0x7FFF_FFFE) {
            ProcessState::Alive { .. } => panic!("out-of-range pid reported alive"),
            ProcessState::Dead | ProcessState::Unknown { .. } => {}
        }
    }
}
