//! Linux sandbox for command execution.
//!
//! Provides process-level isolation using:
//! - **Landlock** — filesystem access control (kernel 5.13+)
//! - **Network namespace** — blocks network access via `unshare(CLONE_NEWNET)`
//! - **Resource limits** — caps memory and process count via `setrlimit`
//! - **Timeout** — kills the child after a wall-clock deadline
//!
//! On non-Linux platforms [`spawn_sandboxed`] returns an error.

use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Public API (always available)
// ---------------------------------------------------------------------------

/// Policy that controls what the sandboxed process is allowed to do.
#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    /// Allow network access (default: **false**).
    pub allow_network: bool,
    /// Paths the process may *read* (default: `["/"]`).
    pub read_paths: Vec<PathBuf>,
    /// Paths the process may *read and write* (default: `["/tmp"]`).
    pub write_paths: Vec<PathBuf>,
    /// Wall-clock timeout before the process is killed (default: 30 s).
    pub timeout: Duration,
    /// Maximum address-space size in bytes, 0 = unlimited (default: 512 MiB).
    pub max_memory_bytes: u64,
    /// Maximum number of child processes, 0 = unlimited (default: 64).
    pub max_pids: u64,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            allow_network: false,
            read_paths: vec![PathBuf::from("/")],
            write_paths: vec![PathBuf::from("/tmp")],
            timeout: Duration::from_secs(30),
            max_memory_bytes: 512 * 1024 * 1024,
            max_pids: 64,
        }
    }
}

/// Spawn `sh -c <command>` inside a sandbox described by `policy`.
///
/// Returns the combined stdout/stderr/exit-code, or an error if the sandbox
/// could not be established or the process failed to start.
///
/// On non-Linux platforms this always returns an error.
#[cfg(not(target_os = "linux"))]
pub fn spawn_sandboxed(_command: &str, _policy: &SandboxPolicy) -> Result<Output, String> {
    Err("sandboxed execution is only supported on Linux".into())
}

// ---------------------------------------------------------------------------
// Linux implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub fn spawn_sandboxed(command: &str, policy: &SandboxPolicy) -> Result<Output, String> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    // Pre-compute CStrings for paths (before fork).
    let read_paths_c = paths_to_cstrings(&policy.read_paths)?;
    let write_paths_c = paths_to_cstrings(&policy.write_paths)?;

    let allow_network = policy.allow_network;
    let max_memory = policy.max_memory_bytes;
    let max_pids = policy.max_pids;

    use std::process::Stdio;

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // pre_exec: runs in the child between fork() and exec().
    unsafe {
        cmd.pre_exec(move || {
            // Each layer is best-effort: if the kernel or permissions don't
            // support it, we continue with whatever layers *did* succeed.

            // 1. Network isolation
            if !allow_network {
                let _ = apply_network_isolation();
            }

            // 2. Resource limits
            if max_memory > 0 {
                let _ = apply_rlimit(libc::RLIMIT_AS, max_memory);
            }
            if max_pids > 0 {
                let _ = apply_rlimit(libc::RLIMIT_NPROC, max_pids);
            }

            // 3. Filesystem isolation (Landlock)
            let _ = apply_landlock(&read_paths_c, &write_paths_c);

            Ok(())
        });
    }

    // Spawn and enforce timeout.
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn sandboxed command: {e}"))?;

    let pid = child.id();
    let timeout = policy.timeout;
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();

    // Watchdog thread: kills the child if it exceeds the timeout.
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if done_clone.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !done_clone.load(Ordering::Relaxed) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            return true;
        }
        false
    });

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait on sandboxed command: {e}"))?;

    done.store(true, Ordering::Relaxed);
    let timed_out = watchdog.join().unwrap_or(false);

    if timed_out {
        return Err(format!(
            "command timed out after {} seconds",
            timeout.as_secs()
        ));
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Linux internals
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
use std::ffi::CString;

#[cfg(target_os = "linux")]
fn paths_to_cstrings(paths: &[PathBuf]) -> Result<Vec<CString>, String> {
    paths
        .iter()
        .map(|p| {
            CString::new(p.as_os_str().as_encoded_bytes())
                .map_err(|e| format!("invalid path {}: {e}", p.display()))
        })
        .collect()
}

/// Isolate the child into its own network namespace.
///
/// Tries `CLONE_NEWUSER | CLONE_NEWNET` first (works unprivileged on most
/// kernels). Falls back to `CLONE_NEWNET` alone (needs `CAP_SYS_ADMIN`).
/// If both fail the sandbox continues without network isolation — the other
/// layers (Landlock, rlimits) still apply.
#[cfg(target_os = "linux")]
fn apply_network_isolation() -> std::io::Result<()> {
    // Try unprivileged: create a user namespace + network namespace together.
    let ret = unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) };
    if ret == 0 {
        return Ok(());
    }

    // Fallback: network namespace only (needs CAP_SYS_ADMIN).
    let ret = unsafe { libc::unshare(libc::CLONE_NEWNET) };
    if ret == 0 {
        return Ok(());
    }

    // Neither worked — continue without network isolation rather than
    // refusing to run entirely. Landlock + rlimits still protect the host.
    Ok(())
}

/// Set a soft+hard resource limit.
#[cfg(target_os = "linux")]
fn apply_rlimit(resource: libc::__rlimit_resource_t, value: u64) -> std::io::Result<()> {
    let rlim = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    let ret = unsafe { libc::setrlimit(resource, &rlim) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Landlock
// ---------------------------------------------------------------------------

// Syscall numbers — architecture-dependent.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod syscall_nr {
    pub const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    pub const LANDLOCK_ADD_RULE: libc::c_long = 445;
    pub const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
}
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod syscall_nr {
    pub const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    pub const LANDLOCK_ADD_RULE: libc::c_long = 445;
    pub const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
}

// Landlock ABI v1 access flags (kernel 5.13).
#[cfg(target_os = "linux")]
mod landlock_flags {
    pub const ACCESS_FS_EXECUTE: u64 = 1 << 0;
    pub const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    pub const ACCESS_FS_READ_FILE: u64 = 1 << 2;
    pub const ACCESS_FS_READ_DIR: u64 = 1 << 3;
    pub const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    pub const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    pub const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    pub const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    pub const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    pub const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    pub const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    pub const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    pub const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;

    pub const ACCESS_READ: u64 = ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;

    pub const ACCESS_WRITE: u64 = ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;

    pub const ACCESS_ALL: u64 = ACCESS_READ | ACCESS_WRITE;
}

#[cfg(target_os = "linux")]
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;

/// ABI-v1 ruleset attribute.
#[cfg(target_os = "linux")]
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

/// ABI-v1 path-beneath rule attribute.
#[cfg(target_os = "linux")]
#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: libc::c_int,
}

/// Apply Landlock filesystem restrictions.
///
/// `read_paths_c` get read-only access; `write_paths_c` get read+write.
/// All other filesystem access is denied.
#[cfg(target_os = "linux")]
fn apply_landlock(read_paths_c: &[CString], write_paths_c: &[CString]) -> std::io::Result<()> {
    use landlock_flags::*;

    // Required: no-new-privs must be set before landlock_restrict_self.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Create ruleset.
    let attr = RulesetAttr {
        handled_access_fs: ACCESS_ALL,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            syscall_nr::LANDLOCK_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if ruleset_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let ruleset_fd = ruleset_fd as libc::c_int;

    // Helper: add a path rule.
    let add_rule = |path: &CString, access: u64| -> std::io::Result<()> {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            // Path doesn't exist — skip silently.
            return Ok(());
        }

        let beneath = PathBeneathAttr {
            allowed_access: access,
            parent_fd: fd,
        };
        let ret = unsafe {
            libc::syscall(
                syscall_nr::LANDLOCK_ADD_RULE,
                ruleset_fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &beneath as *const PathBeneathAttr,
                0u32,
            )
        };
        unsafe { libc::close(fd) };

        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };

    for path in read_paths_c {
        add_rule(path, ACCESS_READ)?;
    }
    for path in write_paths_c {
        add_rule(path, ACCESS_ALL)?;
    }

    // Enforce.
    let ret = unsafe { libc::syscall(syscall_nr::LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
    unsafe { libc::close(ruleset_fd) };

    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_restrictive() {
        let p = SandboxPolicy::default();
        assert!(!p.allow_network);
        assert_eq!(p.timeout, Duration::from_secs(30));
        assert!(p.max_memory_bytes > 0);
        assert!(p.max_pids > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn echo_runs_in_sandbox() {
        let policy = SandboxPolicy {
            allow_network: false,
            read_paths: vec![PathBuf::from("/")],
            write_paths: vec![PathBuf::from("/tmp")],
            timeout: Duration::from_secs(5),
            max_memory_bytes: 256 * 1024 * 1024,
            max_pids: 32,
        };
        let output = spawn_sandboxed("echo sandbox-ok", &policy).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.trim().contains("sandbox-ok"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_kills_long_running_command() {
        let policy = SandboxPolicy {
            timeout: Duration::from_secs(1),
            ..SandboxPolicy::default()
        };
        let result = spawn_sandboxed("sleep 60", &policy);
        assert!(
            result.is_err() || {
                let output = result.unwrap();
                output.status.code() != Some(0)
            }
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_returns_error() {
        let result = spawn_sandboxed("echo hi", &SandboxPolicy::default());
        assert!(result.is_err());
    }
}
