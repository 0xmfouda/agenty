---
title: Sandbox Reference
description: Complete reference for the SandboxPolicy configuration.
---

## SandboxPolicy

Defined in `agenty_tools::sandbox`.

Controls how the `bash` tool isolates child processes on Linux. Each field maps to a specific kernel mechanism.

```rust
pub struct SandboxPolicy {
    pub allow_network: bool,
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
    pub timeout: Duration,
    pub max_memory_bytes: u64,
    pub max_pids: u64,
}
```

## Fields

### `allow_network`

**Type:** `bool`
**Default:** `false`

When `false`, the child process is placed in a new network namespace with no configured interfaces. This blocks all TCP, UDP, and DNS traffic.

**Kernel mechanism:** `unshare(CLONE_NEWUSER | CLONE_NEWNET)`. Falls back to `unshare(CLONE_NEWNET)` if unprivileged user namespaces are disabled. Skipped entirely if neither call succeeds.

### `read_paths`

**Type:** `Vec<PathBuf>`
**Default:** `["/"]`

Paths the sandboxed process may read from. Read access includes:

| Landlock flag | Allows |
|---|---|
| `ACCESS_FS_EXECUTE` | Executing files |
| `ACCESS_FS_READ_FILE` | Reading file contents |
| `ACCESS_FS_READ_DIR` | Listing directory entries |

Paths that do not exist at spawn time are silently skipped.

**Kernel mechanism:** Landlock ABI v1 `path_beneath` rules.

### `write_paths`

**Type:** `Vec<PathBuf>`
**Default:** `["/tmp"]`

Paths the sandboxed process may read from and write to. Write access includes all read flags plus:

| Landlock flag | Allows |
|---|---|
| `ACCESS_FS_WRITE_FILE` | Writing to files |
| `ACCESS_FS_REMOVE_DIR` | Removing directories |
| `ACCESS_FS_REMOVE_FILE` | Removing files |
| `ACCESS_FS_MAKE_CHAR` | Creating character devices |
| `ACCESS_FS_MAKE_DIR` | Creating directories |
| `ACCESS_FS_MAKE_REG` | Creating regular files |
| `ACCESS_FS_MAKE_SOCK` | Creating sockets |
| `ACCESS_FS_MAKE_FIFO` | Creating FIFOs |
| `ACCESS_FS_MAKE_BLOCK` | Creating block devices |
| `ACCESS_FS_MAKE_SYM` | Creating symbolic links |

**Kernel mechanism:** Landlock ABI v1 `path_beneath` rules with full access mask.

### `timeout`

**Type:** `Duration`
**Default:** `30 seconds`

Maximum wall-clock time the child process may run. After this duration, the parent sends `SIGKILL` and the tool returns an error.

**Mechanism:** A watchdog thread in the parent polls every 100ms and calls `kill(pid, SIGKILL)` when the deadline is exceeded.

### `max_memory_bytes`

**Type:** `u64`
**Default:** `536870912` (512 MiB)

Maximum virtual address space for the child process. Set to `0` to leave unlimited.

**Kernel mechanism:** `setrlimit(RLIMIT_AS, ...)`.

### `max_pids`

**Type:** `u64`
**Default:** `64`

Maximum number of processes the user may have (including the sandboxed shell and its children). Set to `0` to leave unlimited.

**Kernel mechanism:** `setrlimit(RLIMIT_NPROC, ...)`.

## Default policy

```rust
SandboxPolicy {
    allow_network: false,
    read_paths: vec![PathBuf::from("/")],
    write_paths: vec![PathBuf::from("/tmp")],
    timeout: Duration::from_secs(30),
    max_memory_bytes: 512 * 1024 * 1024,
    max_pids: 64,
}
```

## spawn_sandboxed

```rust
pub fn spawn_sandboxed(command: &str, policy: &SandboxPolicy) -> Result<Output, String>;
```

Spawns `sh -c <command>` with the sandbox applied. On non-Linux platforms, always returns `Err`.

The function:

1. Pre-computes `CString` representations of all paths (before fork).
2. Configures the child's stdin, stdout, and stderr as pipes.
3. Uses `pre_exec` to apply sandbox layers in the child between `fork()` and `exec()`.
4. Spawns the process and starts a watchdog thread for the timeout.
5. Waits for the child to finish or be killed.
6. Returns the `std::process::Output` with captured stdout and stderr.

## Supported architectures

Landlock syscall numbers are defined for:

- `x86_64`
- `aarch64`

Other architectures will skip Landlock (the other sandbox layers still apply).

## Graceful degradation

Every sandbox layer is best-effort. The order of application inside `pre_exec`:

| Order | Layer | Failure behavior |
|---|---|---|
| 1 | Network namespace | Skipped; child retains host networking. |
| 2 | RLIMIT_AS | Skipped; no memory cap. |
| 3 | RLIMIT_NPROC | Skipped; no process cap. |
| 4 | Landlock | Skipped; child retains full filesystem access. |

The timeout layer runs in the parent process and always works regardless of child capabilities.
