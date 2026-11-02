//! Blocking, fail-closed desktop app-server adapter. No GUI or authentication changes.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const TAIL_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_TAILS: usize = 512;
static QUERY_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppServer {
    pid: u32,
    identity: String,
    parent: u32,
    parent_identity: String,
    executable: PathBuf,
    home: PathBuf,
}

// A timed-out native read may finish later, but can never signal anything. Only
// one query can remain outstanding, so retries cannot accumulate stuck workers.
fn bounded<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    bounded_with(QUERY_TIMEOUT, &QUERY_ACTIVE, read)
}

fn bounded_with<T: Send + 'static>(
    timeout: Duration,
    active: &'static AtomicBool,
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if active.swap(true, Ordering::AcqRel) {
        return Err("process query already in progress".into());
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("codex-process-read".into())
        .spawn(move || {
            struct Release(&'static AtomicBool);
            impl Drop for Release {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let release = Release(active);
            let result = read();
            drop(release);
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        active.store(false, Ordering::Release);
        return Err("cannot start process query".into());
    }
    rx.recv_timeout(timeout)
        .map_err(|_| "process query timed out or failed".to_string())?
}

pub(crate) fn discover(home: &Path) -> Result<Vec<AppServer>, String> {
    let home = home.to_path_buf();
    bounded(move || platform::discover(&home))
}

pub(crate) fn end_if_same(server: &AppServer, home: &Path) -> Result<(), String> {
    let expected = server.clone();
    let home = home.to_path_buf();
    // Both potentially blocking reads are inside this deadline. The worker has
    // no signal callback: even a late successful result cannot end a process.
    let validation = bounded(move || {
        let current = platform::inspect(expected.pid, &home)?;
        validate(&expected, current.as_ref(), || scan_sessions(&home))
    });
    end_after_validation(validation, || platform::end(server))
}

fn validate(
    expected: &AppServer,
    current: Option<&AppServer>,
    busy: impl FnOnce() -> Result<bool, String>,
) -> Result<(), String> {
    if current != Some(expected) {
        return Err("app-server identity, ownership or home changed".into());
    }
    if busy()? {
        return Err("Codex session activity is running".into());
    }
    Ok(())
}

fn end_after_validation(
    validation: Result<(), String>,
    signal: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    validation?;
    signal()
}

// No environment or command content appears in errors or retained snapshots.
#[cfg(any(target_os = "macos", test))]
fn process_home(env: &[&[u8]]) -> Result<PathBuf, String> {
    let value = |name: &[u8]| -> Result<Option<PathBuf>, String> {
        let mut found = None;
        for item in env {
            if let Some(v) = item.strip_prefix(name) {
                if found.is_some() || v.is_empty() {
                    return Err("ambiguous process home".into());
                }
                let s = std::str::from_utf8(v).map_err(|_| "unreadable process home")?;
                let p = PathBuf::from(s);
                if !p.is_absolute() {
                    return Err("relative process home is unsupported".into());
                }
                found = Some(p);
            }
        }
        Ok(found)
    };
    if let Some(p) = value(b"CODEX_HOME=")? {
        return Ok(p);
    }
    Ok(value(b"HOME=")?
        .ok_or("process home unavailable")?
        .join(".codex"))
}

#[cfg(any(target_os = "macos", test))]
fn own_arguments(args: &[&[u8]]) -> bool {
    if args
        .iter()
        .any(|a| *a == b"--listen" || a.starts_with(b"--listen="))
    {
        return false;
    }
    // Only known global config overrides before the subcommand. Never match an
    // app-server word buried in exec input or another subcommand's arguments.
    let mut i = 1;
    while i < args.len() {
        if args[i] == b"-c" || args[i] == b"--config" {
            if i + 1 >= args.len() {
                return false;
            }
            i += 2;
        } else if args[i].starts_with(b"--config=") {
            i += 1;
        } else {
            break;
        }
    }
    if args.get(i) != Some(&b"app-server".as_slice()) {
        return false;
    }
    i += 1;
    while i < args.len() {
        match args[i] {
            b"--analytics-default-enabled" => i += 1,
            b"-c" | b"--config" if i + 1 < args.len() => i += 2,
            a if a.starts_with(b"--config=") => i += 1,
            _ => return false,
        }
    }
    true
}

#[cfg(any(target_os = "macos", test))]
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    for bundle in exe.ancestors() {
        if !matches!(bundle.file_name()?.to_str()?, "ChatGPT.app" | "Codex.app") {
            continue;
        }
        let relative = exe.strip_prefix(bundle).ok()?;
        if matches!(
            relative.to_str()?,
            "Contents/Resources/codex"
                | "Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex"
        ) {
            return Some(bundle.to_path_buf());
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn app_parent(exe: &Path, bundle: &Path) -> bool {
    matches!(
        exe.strip_prefix(bundle).ok().and_then(Path::to_str),
        Some("Contents/MacOS/ChatGPT" | "Contents/MacOS/Codex")
    )
}

#[cfg(any(target_os = "macos", test))]
type ProcArgs<'a> = (Vec<&'a [u8]>, Vec<&'a [u8]>);

// KERN_PROCARGS2: argc, executable NUL, padding, argc NUL-terminated
// arguments, then environment. Unlike ps -E this preserves spaces and '='.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs(bytes: &[u8]) -> Result<ProcArgs<'_>, String> {
    let raw: [u8; 4] = bytes
        .get(..4)
        .ok_or("short process arguments")?
        .try_into()
        .unwrap();
    let argc = i32::from_ne_bytes(raw);
    if !(1..=4096).contains(&argc) {
        return Err("invalid argument count".into());
    }
    let mut rest = &bytes[4..];
    let end = rest
        .iter()
        .position(|b| *b == 0)
        .ok_or("incomplete process executable")?;
    rest = &rest[end + 1..];
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    let mut args = Vec::new();
    for _ in 0..argc {
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or("incomplete process arguments")?;
        args.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
    if !rest.ends_with(&[0]) {
        return Err("incomplete process environment".into());
    }
    Ok((
        args,
        rest.split(|b| *b == 0).filter(|s| !s.is_empty()).collect(),
    ))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::{mem, os::unix::ffi::OsStrExt};

    fn info(pid: u32) -> Result<libc::proc_bsdinfo, String> {
        if pid <= 1 || pid > i32::MAX as u32 {
            return Err("invalid process id".into());
        }
        // SAFETY: the kernel fills a correctly sized, aligned POD buffer.
        let mut value: libc::proc_bsdinfo = unsafe { mem::zeroed() };
        let size = mem::size_of_val(&value) as i32;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut value as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read != size || value.pbi_pid != pid {
            return Err("process metadata unavailable".into());
        }
        let uid = unsafe { libc::geteuid() };
        if value.pbi_uid != uid || value.pbi_ruid != uid || value.pbi_svuid != uid {
            return Err("process is not owned by the current user".into());
        }
        Ok(value)
    }

    fn identity(i: &libc::proc_bsdinfo) -> String {
        format!("{}:{}:{}", i.pbi_pid, i.pbi_start_tvsec, i.pbi_start_tvusec)
    }

    fn executable(pid: u32) -> Result<PathBuf, String> {
        let mut bytes = vec![0u8; 4096];
        let n = unsafe {
            libc::proc_pidpath(pid as i32, bytes.as_mut_ptr().cast(), bytes.len() as u32)
        };
        if n <= 0 || n as usize >= bytes.len() {
            return Err("process executable unavailable".into());
        }
        bytes.truncate(n as usize);
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
    }

    fn args(pid: u32) -> Result<Vec<u8>, String> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
        let mut bytes = vec![0u8; 1024 * 1024];
        let mut size = bytes.len();
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || size >= bytes.len() {
            return Err("process arguments unavailable".into());
        }
        bytes.truncate(size);
        Ok(bytes)
    }

    pub(super) fn inspect(pid: u32, home: &Path) -> Result<Option<AppServer>, String> {
        let before = info(pid)?;
        let exe = executable(pid)?;
        let Some(bundle) = app_bundle(&exe) else {
            return Ok(None);
        };
        let bytes = args(pid)?;
        let (argv, env) = parse_procargs(&bytes)?;
        if !own_arguments(&argv) {
            return Ok(None);
        }
        let actual_home =
            fs::canonicalize(process_home(&env)?).map_err(|_| "process home unavailable")?;
        let home = fs::canonicalize(home).map_err(|_| "requested home unavailable")?;
        if actual_home != home {
            return Ok(None);
        }
        // Direct parent is intentionally stricter than matching any ancestor:
        // app-spawned terminals/editors must not confer app ownership on helpers.
        let parent = info(before.pbi_ppid)?;
        if !app_parent(&executable(parent.pbi_pid)?, &bundle) {
            return Ok(None);
        }
        let after = info(pid)?;
        if identity(&before) != identity(&after) || before.pbi_ppid != after.pbi_ppid {
            return Err("process changed during inspection".into());
        }
        Ok(Some(AppServer {
            pid,
            identity: identity(&after),
            parent: parent.pbi_pid,
            parent_identity: identity(&parent),
            executable: exe,
            home,
        }))
    }

    pub(super) fn discover(home: &Path) -> Result<Vec<AppServer>, String> {
        let deadline = Instant::now() + QUERY_TIMEOUT;
        let mut pids = vec![0u32; MAX_ENTRIES];
        let size = (pids.len() * mem::size_of::<u32>()) as i32;
        let n = unsafe {
            libc::proc_listpids(
                4, // PROC_UID_ONLY, Darwin sys/proc_info.h (not exported by libc)
                libc::geteuid(),
                pids.as_mut_ptr().cast(),
                size,
            )
        };
        if n <= 0 || n >= size || n as usize % mem::size_of::<u32>() != 0 {
            return Err("process listing unavailable or truncated".into());
        }
        pids.truncate(n as usize / mem::size_of::<u32>());
        let mut found = Vec::new();
        for pid in pids.into_iter().filter(|p| *p > 1) {
            if Instant::now() >= deadline {
                return Err("process query timed out".into());
            }
            // Processes can exit while listing. Only identified bundled codex
            // candidates require full inspection; never guess through its errors.
            let Ok(exe) = executable(pid) else {
                continue;
            };
            if app_bundle(&exe).is_some() {
                if let Some(server) = inspect(pid, home)? {
                    found.push(server);
                }
            }
        }
        found.sort_by_key(|s| s.pid);
        Ok(found)
    }

    pub(super) fn end(server: &AppServer) -> Result<(), String> {
        // An exec can replace the image without changing its birth timestamp.
        let bundle = app_bundle(&server.executable).ok_or("unrecognized app bundle")?;
        if executable(server.pid)? != server.executable
            || !app_parent(&executable(server.parent)?, &bundle)
        {
            return Err("process executable changed before signal".into());
        }
        // Final kernel-only check follows the session recheck, with no logging,
        // filesystem traversal, shell, or other work before SIGTERM. macOS has
        // no public pidfd: a residual exit/reuse race across these syscalls remains.
        let parent = info(server.parent)?;
        let current = info(server.pid)?;
        if identity(&parent) != server.parent_identity
            || identity(&current) != server.identity
            || current.pbi_ppid != server.parent
        {
            return Err("process identity changed before signal".into());
        }
        #[cfg(test)]
        {
            Err("production signalling disabled in tests".into())
        }
        #[cfg(not(test))]
        {
            // Magpie proc.End on Unix: SIGTERM, never SIGKILL or a process group.
            if unsafe { libc::kill(server.pid as i32, libc::SIGTERM) } == 0 {
                Ok(())
            } else {
                Err("app-server SIGTERM failed".into())
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;
    const UNSUPPORTED: &str =
        "safe Codex app-server process verification unsupported on this platform";
    pub(super) fn discover(_: &Path) -> Result<Vec<AppServer>, String> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn inspect(_: u32, _: &Path) -> Result<Option<AppServer>, String> {
        Err(UNSUPPORTED.into())
    }
    pub(super) fn end(_: &AppServer) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn turn_may_be_running(home: &Path) -> bool {
    session_state(home).unwrap_or(true)
}

/// True means an open turn; Err means unknown, never proof of inactivity.
pub(crate) fn session_state(home: &Path) -> Result<bool, String> {
    let home = home.to_path_buf();
    bounded(move || scan_sessions(&home))
}

fn scan_sessions(home: &Path) -> Result<bool, String> {
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut dirs = vec![(home.join("sessions"), 0usize)];
    let mut entries = 0;
    let mut tails = 0;
    let mut busy = false;
    while let Some((dir, depth)) = dirs.pop() {
        if depth > 8 || Instant::now() >= deadline {
            return Err("session scan depth or time limit exceeded".into());
        }
        let metadata = fs::symlink_metadata(&dir).map_err(|_| "session directory unreadable")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("session directory is not a regular directory".into());
        }
        for entry in fs::read_dir(dir).map_err(|_| "session directory unreadable")? {
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err("session entry limit exceeded".into());
            }
            if Instant::now() >= deadline {
                return Err("session scan time limit exceeded".into());
            }
            let entry = entry.map_err(|_| "session entry unreadable")?;
            let kind = entry
                .file_type()
                .map_err(|_| "session entry type unreadable")?;
            if kind.is_symlink() {
                return Err("session symlink is unsupported".into());
            }
            if kind.is_dir() {
                dirs.push((entry.path(), depth + 1));
                continue;
            }
            if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if !kind.is_file() {
                return Err("session is not a regular file".into());
            }
            // Waiting for approval or a long tool can leave an open turn's
            // mtime unchanged indefinitely. Age is never evidence of idle.
            tails += 1;
            if tails > MAX_TAILS {
                return Err("session tail limit exceeded".into());
            }
            busy |= tail_busy(&entry.path())?;
        }
    }
    Ok(busy)
}

fn tail_busy(path: &Path) -> Result<bool, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file: File = options.open(path).map_err(|_| "session file unreadable")?;
    let before = file.metadata().map_err(|_| "session metadata unreadable")?;
    if !before.is_file() {
        return Err("session is not a regular file".into());
    }
    let offset = before.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "session seek failed")?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(TAIL_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| "session tail unreadable")?;
    let after = file.metadata().map_err(|_| "session metadata unreadable")?;
    if before.len() != after.len()
        || before
            .modified()
            .map_err(|_| "session timestamp unreadable")?
            != after
                .modified()
                .map_err(|_| "session timestamp unreadable")?
    {
        return Err("session changed while reading".into());
    }
    tail_state(&bytes, offset != 0)
}

fn tail_state(bytes: &[u8], truncated: bool) -> Result<bool, String> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err("session tail empty or incomplete".into());
    }
    let bytes = if truncated {
        let Some(end) = bytes.iter().position(|b| *b == b'\n') else {
            return Err("session tail has no complete record".into());
        };
        &bytes[end + 1..]
    } else {
        bytes
    };
    let mut state = None;
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            return Err("session record is invalid JSON".into());
        };
        let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
            return Err("session record type missing".into());
        };
        if kind != "event_msg" {
            continue;
        }
        let Some(event) = value
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(|v| v.as_str())
        else {
            return Err("session event type missing".into());
        };
        match event {
            "task_started" => state = Some(true),
            "task_complete" | "turn_aborted" => state = Some(false),
            _ => {}
        }
    }
    state.ok_or_else(|| "session tail has no lifecycle event".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit opt-in only: this is the Codex home itself, not its parent HOME.
    // Never call end_if_same here, even though signalling is disabled in tests.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "read-only live diagnostic; requires CC_SWITCH_CODEX_DIAGNOSTIC_HOME"]
    fn live_discovery_read_only() {
        let Some(home) = std::env::var_os("CC_SWITCH_CODEX_DIAGNOSTIC_HOME") else {
            return;
        };
        let home = PathBuf::from(home);
        assert!(
            home.is_absolute() && home.is_dir(),
            "diagnostic requires an explicit absolute Codex home directory"
        );
        let servers = match discover(&home) {
            Ok(servers) => servers,
            Err(_) => panic!("read-only process discovery failed"),
        };
        let busy = turn_may_be_running(&home);
        println!("matched={} busy={busy}", servers.len());
        assert!(!servers.is_empty(), "no matching app-owned app-server");
    }

    fn server() -> AppServer {
        AppServer {
            pid: 99,
            identity: "99:123:456".into(),
            parent: 98,
            parent_identity: "98:122:1".into(),
            executable: "/Applications/Codex.app/Contents/Resources/codex".into(),
            home: "/fixture".into(),
        }
    }
    fn event(kind: &str) -> String {
        format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\"}}}}\n")
    }

    #[test]
    fn refuses_changed_identity_home_parent_or_busy_without_signalling() {
        let s = server();
        let never = || -> Result<(), String> { panic!("must not signal") };
        assert!(end_after_validation(validate(&s, None, || Ok(false)), never).is_err());
        for changed in [
            AppServer {
                identity: "reused".into(),
                ..s.clone()
            },
            AppServer {
                home: "/other".into(),
                ..s.clone()
            },
            AppServer {
                parent_identity: "reused".into(),
                ..s.clone()
            },
        ] {
            assert!(
                end_after_validation(validate(&s, Some(&changed), || Ok(false)), never).is_err()
            );
        }
        assert!(end_after_validation(validate(&s, Some(&s), || Ok(true)), never).is_err());
        assert!(
            end_after_validation(validate(&s, Some(&s), || Err("unknown".into())), never).is_err()
        );
        let called = std::cell::Cell::new(false);
        assert!(
            end_after_validation(validate(&s, Some(&s), || Ok(false)), || {
                called.set(true);
                Ok(())
            })
            .is_ok()
        );
        assert!(called.get());
    }

    #[test]
    fn strict_bundle_parent_and_subcommand() {
        let exe = Path::new("/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex");
        let bundle = app_bundle(exe).unwrap();
        assert!(app_parent(
            Path::new("/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"),
            &bundle
        ));
        assert!(!app_parent(
            Path::new("/Applications/Other.app/Contents/MacOS/ChatGPT"),
            &bundle
        ));
        assert!(app_bundle(Path::new("/usr/local/bin/codex")).is_none());
        assert!(app_bundle(Path::new(
            "/Applications/Other.app/Contents/Resources/codex"
        ))
        .is_none());
        assert!(own_arguments(&[
            b"codex",
            b"-c",
            b"x=true",
            b"app-server",
            b"--analytics-default-enabled",
            b"-c",
            b"plugins.test.enabled=true"
        ]));
        for args in [
            vec![b"codex".as_slice(), b"exec", b"app-server"],
            vec![b"codex", b"app-server", b"--listen", b"stdio://"],
            vec![b"codex", b"app-server", b"--listen=stdio://"],
            vec![b"codex", b"app-server", b"daemon", b"start"],
        ] {
            assert!(!own_arguments(&args));
        }
    }

    #[test]
    fn structured_arguments_do_not_confuse_env_with_arguments() {
        let home = if cfg!(windows) {
            "C:/Users/real name"
        } else {
            "/Users/real name"
        };
        let codex_home = format!("{home}/x=y");
        let home_env = format!("HOME={home}");
        let mut raw = 3i32.to_ne_bytes().to_vec();
        raw.extend_from_slice(
            format!("/bin/codex\0\0codex\0app-server\0--analytics-default-enabled\0{home_env}\0CODEX_HOME={codex_home}\0\0")
                .as_bytes(),
        );
        let (args, env) = parse_procargs(&raw).unwrap();
        assert!(own_arguments(&args));
        assert_eq!(process_home(&env).unwrap(), Path::new(&codex_home));
        assert_eq!(
            process_home(&[home_env.as_bytes()]).unwrap(),
            Path::new(home).join(".codex")
        );
        assert!(process_home(&[b"CODEX_HOME=relative", home_env.as_bytes()]).is_err());
        assert!(process_home(&[home_env.as_bytes(), home_env.as_bytes()]).is_err());
        assert!(process_home(&[]).is_err());
        assert!(parse_procargs(&raw[..raw.len() - 2]).is_err());
    }

    #[test]
    fn session_states_and_partial_or_unknown_data_fail_closed() {
        let start = event("task_started");
        let done = event("task_complete");
        assert_eq!(tail_state(start.as_bytes(), false), Ok(true));
        assert_eq!(
            tail_state(format!("{start}{done}").as_bytes(), false),
            Ok(false)
        );
        assert_eq!(
            tail_state(event("turn_aborted").as_bytes(), false),
            Ok(false)
        );
        assert!(tail_state(format!("{start}{done}{start}").as_bytes(), false).unwrap());
        for bytes in [
            b"".as_slice(),
            b"broken\n",
            b"{}\n",
            b"{\"type\":\"session_meta\"}\n",
            b"{\"type\":\"event_msg\"}\n",
        ] {
            assert!(tail_state(bytes, false).is_err());
        }
        assert!(tail_state(done.trim_end().as_bytes(), false).is_err());
        assert_eq!(
            tail_state(format!("fragment\n{done}").as_bytes(), true),
            Ok(false)
        );
        assert!(tail_state(done.as_bytes(), true).is_err());
    }

    #[test]
    fn scanner_reads_regular_bounded_tails_and_distinguishes_unknown() {
        let temp = tempfile::tempdir().unwrap();
        assert!(scan_sessions(temp.path()).is_err()); // missing is unknown
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        assert_eq!(scan_sessions(temp.path()), Ok(false));
        let path = sessions.join("rollout-test.jsonl");
        fs::write(&path, event("task_started")).unwrap();
        assert_eq!(scan_sessions(temp.path()), Ok(true));
        fs::write(&path, event("task_complete")).unwrap();
        assert_eq!(scan_sessions(temp.path()), Ok(false));
        fs::write(&path, vec![b'x'; TAIL_BYTES as usize + 100]).unwrap();
        assert!(tail_busy(&path).is_err());
        assert!(scan_sessions(temp.path()).is_err());
    }

    #[test]
    fn old_open_turn_stays_busy_until_a_terminal_event() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let path = sessions.join("rollout-old.jsonl");
        let old = std::time::SystemTime::now() - Duration::from_secs(24 * 60 * 60);
        fs::write(&path, event("task_started")).unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        assert_eq!(scan_sessions(temp.path()), Ok(true));
        fs::write(&path, event("task_complete")).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        assert_eq!(scan_sessions(temp.path()), Ok(false));
    }

    #[test]
    fn history_entry_limit_is_unknown_not_busy_or_idle() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        for i in 0..=MAX_ENTRIES {
            File::create(sessions.join(format!("entry-{i}.txt"))).unwrap();
        }
        assert!(scan_sessions(temp.path()).is_err());
    }

    #[test]
    fn completed_history_beyond_32_tails_is_idle_but_513_is_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        let done = event("task_complete");
        for i in 0..171 {
            fs::write(sessions.join(format!("rollout-{i}.jsonl")), &done).unwrap();
        }
        assert_eq!(scan_sessions(temp.path()), Ok(false));
        for i in 171..=MAX_TAILS {
            fs::write(sessions.join(format!("rollout-{i}.jsonl")), &done).unwrap();
        }
        assert!(scan_sessions(temp.path()).is_err());
    }

    #[test]
    fn timed_out_read_cannot_signal_even_when_it_later_succeeds() {
        static ACTIVE: AtomicBool = AtomicBool::new(false);
        let (release, wait) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let validation = bounded_with(Duration::ZERO, &ACTIVE, move || {
            wait.recv().map_err(|_| "test read cancelled")?;
            let result = validate(&server(), Some(&server()), || Ok(false));
            finished.send(()).map_err(|_| "test receiver gone")?;
            result
        });
        // The channel keeps the read incomplete: no sleeping or native queries.
        assert!(validation.is_err());
        assert!(bounded_with(Duration::ZERO, &ACTIVE, || Ok(())).is_err());
        let signals = std::cell::Cell::new(0);
        assert!(end_after_validation(validation, || {
            signals.set(signals.get() + 1);
            Ok(())
        })
        .is_err());
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(signals.get(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn scanner_refuses_symlinks_and_nonregular_files() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        std::os::unix::fs::symlink("missing", sessions.join("rollout.jsonl")).unwrap();
        assert!(scan_sessions(temp.path()).is_err());
        assert!(tail_busy(&sessions.join("rollout.jsonl")).is_err());
        assert!(tail_busy(&sessions).is_err());
    }
}
