//! Linux process identity for background ownership recovery.
//!
//! Procfs reads belong on background workers, never the HTML request path.
//! Identities are valid only within the boot validated by the shared region.

use std::io;

/// PID plus kernel start ticks prevents granting ownership to a reused PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Kernel process identifier in the worker PID namespace.
    pub pid: u32,
    /// Kernel start time (stat field 22), relative to this boot.
    pub start_ticks: u64,
}

/// Unknown is deliberately distinct from dead: inability to inspect a process
/// must never authorize concurrent ownership of its work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessLiveness {
    /// The observed start time matches a running process.
    Alive,
    /// The PID is absent or belongs to a different process.
    Dead,
    /// Inspection failed or the leader exited while threads may survive.
    Unknown,
}

impl ProcessIdentity {
    /// Read on the worker after fork; callers must not cache the master's PID.
    pub fn current() -> io::Result<Self> {
        let pid = std::process::id();
        let (_, start_ticks) = read_stat(pid)?;
        Ok(Self { pid, start_ticks })
    }

    /// A single bounded procfs read, with no polling or timeout-based stealing.
    pub fn liveness(self) -> ProcessLiveness {
        let stat = read_stat(self.pid);
        if matches!(&stat, Err(error) if error.kind() == io::ErrorKind::NotFound) {
            // hidepid can make a live process invisible in procfs. Confirm
            // absence in the kernel; EPERM still means it may be alive.
            if let Ok(pid) = i32::try_from(self.pid) {
                if pid > 0 {
                    // SAFETY: signal zero performs a permission/existence
                    // check only; it neither signals nor mutates the process.
                    let result = unsafe { libc::kill(pid, 0) };
                    if result == -1
                        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                    {
                        return ProcessLiveness::Dead;
                    }
                }
            }
        }
        classify(self, stat)
    }
}

fn read_stat(pid: u32) -> io::Result<(u8, u64)> {
    // PID zero is not a userspace process identity.
    if pid == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "zero owner PID"));
    }
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat"))?)
}

fn classify(owner: ProcessIdentity, stat: io::Result<(u8, u64)>) -> ProcessLiveness {
    match stat {
        Ok((_, ticks)) if ticks != owner.start_ticks => ProcessLiveness::Dead,
        // A zombie process leader may still have live threads (pthread_exit).
        // Only absence or PID reuse proves the whole owner is gone.
        Ok((b'Z' | b'X' | b'x', _)) => ProcessLiveness::Unknown,
        Ok(_) => ProcessLiveness::Alive,
        Err(_) => ProcessLiveness::Unknown,
    }
}

fn parse_stat(stat: &str) -> io::Result<(u8, u64)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed process stat");
    // comm (field 2) can itself contain ')' and whitespace. Numeric fields
    // following its final ')' cannot, so split there rather than at whitespace.
    let (_, tail) = stat.rsplit_once(')').ok_or_else(invalid)?;
    let mut fields = tail.split_ascii_whitespace();
    let state = fields.next().filter(|s| s.len() == 1).ok_or_else(invalid)?;
    // Tail starts with field 3 (state); starttime is field 22.
    let ticks = fields
        .nth(18)
        .ok_or_else(invalid)?
        .parse::<u64>()
        .map_err(|_| invalid())?;
    Ok((state.as_bytes()[0], ticks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn current_process_and_reused_pid() {
        let identity = ProcessIdentity::current().unwrap();
        assert_eq!(identity.pid, std::process::id());
        assert_eq!(identity.liveness(), ProcessLiveness::Alive);
        let reused = ProcessIdentity {
            start_ticks: identity.start_ticks.wrapping_add(1),
            ..identity
        };
        assert_eq!(reused.liveness(), ProcessLiveness::Dead);
    }

    #[test]
    fn command_parentheses_do_not_shift_starttime() {
        let stat = format!(
            "123 (worker ) with (spaces)) S {} 987654 0 0",
            vec!["0"; 18].join(" ")
        );
        assert_eq!(parse_stat(&stat).unwrap(), (b'S', 987654));
        for bad in [
            "",
            "123 (worker)",
            "123 (worker) SS 0",
            "123 (worker) S abc",
        ] {
            assert!(parse_stat(bad).is_err());
        }
    }

    #[test]
    fn uncertain_liveness_never_means_dead() {
        let owner = ProcessIdentity {
            pid: 123,
            start_ticks: 42,
        };
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Interrupted,
        ] {
            assert_eq!(
                classify(owner, Err(io::Error::from(kind))),
                ProcessLiveness::Unknown
            );
        }
        assert_eq!(
            classify(owner, Err(io::Error::from(io::ErrorKind::NotFound))),
            ProcessLiveness::Unknown
        );
        assert_eq!(
            ProcessIdentity {
                pid: 0,
                start_ticks: 0
            }
            .liveness(),
            ProcessLiveness::Unknown
        );
        for state in *b"ZXx" {
            assert_eq!(classify(owner, Ok((state, 42))), ProcessLiveness::Unknown);
        }
    }

    #[test]
    fn reaped_independent_process_is_dead() {
        let mut child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let identity = ProcessIdentity {
            pid: child.id(),
            start_ticks: read_stat(child.id()).unwrap().1,
        };
        let alive = identity.liveness();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(alive, ProcessLiveness::Alive);
        assert_eq!(identity.liveness(), ProcessLiveness::Dead);
    }
}
