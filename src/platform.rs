//! Process helpers: stop an old watcher, check stdin for input without
//! blocking, connect to a Claude Code session's inbox, and find the
//! `claude` process that runs this one.
//!
//! Windows and macOS are the targets; the Unix version also works on Linux.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use crate::error::{CqError, Result};

use imp::Procs;
pub use imp::{InboxConn, open_inbox, stdin_has_input, terminate};

/// How long `stdin_has_input` waits for the first byte on an open, empty
/// pipe. A pipeline writer (`echo ... | cq done`) can start after `cq`;
/// a heredoc is written before `cq` starts.
const PIPE_GRACE: Duration = Duration::from_millis(100);

/// True when the lowercase file name `theirs` is our executable `mine`, or
/// a copy of it renamed by an install. An install on Windows renames the
/// live `cq.exe`, and an old watcher keeps running from the renamed file.
/// A renamed copy is `<mine>.old` or `<stem>.old`, then `-`, `.` or the
/// end: `cq.exe.old-0156`, `cq.old-2026-10-28-2333.exe`, `cq.old.exe`.
/// Other names are refused, so a reused process ID is never stopped.
fn is_cq_name(theirs: &OsStr, mine: &OsStr) -> bool {
    if theirs == mine {
        return true;
    }
    let theirs = theirs.as_encoded_bytes();
    let stem = Path::new(mine).file_stem().map(OsStr::as_encoded_bytes);
    [Some(mine.as_encoded_bytes()), stem]
        .into_iter()
        .flatten()
        .any(|base| {
            theirs
                .strip_prefix(base)
                .and_then(|rest| rest.strip_prefix(b".old"))
                .is_some_and(|rest| matches!(rest.first(), None | Some(b'-' | b'.')))
        })
}

/// How many parent processes `claude_ancestor` looks at.
const MAX_ANCESTORS: usize = 10;

/// What `claude_ancestor` needs to know about one process.
struct ProcInfo {
    ppid: u32,
    /// Comparable between processes on one computer: Windows creation time,
    /// macOS microseconds since 1970, Linux clock ticks since boot.
    start: u64,
    claude: bool,
}

/// The ID and start time of the nearest process above this one that runs
/// `claude` (the Claude Code CLI), up to `MAX_ANCESTORS` levels. `None`
/// when there is none, or a parent looks newer than its child: then the
/// parent exited and its ID was reused, so the chain is broken.
pub fn claude_ancestor() -> Option<(u32, u64)> {
    let procs = Procs::snapshot()?;
    let mut child = procs.info(std::process::id())?;
    for _ in 0..MAX_ANCESTORS {
        let pid = child.ppid;
        if pid <= 1 {
            return None;
        }
        let parent = procs.info(pid)?;
        if parent.start > child.start {
            return None;
        }
        if parent.claude {
            return Some((pid, parent.start));
        }
        child = parent;
    }
    None
}

/// True when a process name or executable path ends in `claude`, or
/// `claude.exe`, in any case.
fn is_claude_name(name: &OsStr) -> bool {
    Path::new(name)
        .file_name()
        .map(|n| n.to_ascii_lowercase())
        .is_some_and(|n| n == "claude" || n == "claude.exe")
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::ptr::null_mut;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileSizeEx, GetFileType,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
        WaitForSingleObject,
    };

    use super::{CqError, PIPE_GRACE, Result};

    /// A client end of a Claude Code session's inbox named pipe.
    pub type InboxConn = std::fs::File;

    /// Connect to a session's inbox named pipe (`\\.\pipe\...`), write only.
    /// Fails at once when the pipe does not exist or every instance is
    /// busy; the caller retries.
    pub fn open_inbox(path: &std::ffi::OsStr) -> std::io::Result<InboxConn> {
        std::fs::OpenOptions::new().write(true).open(path)
    }

    /// Owns a process handle and closes it on drop.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcess and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> Option<Handle> {
        // SAFETY: plain FFI call; a null result means failure.
        let h = unsafe { OpenProcess(access, 0, pid) };
        (!h.is_null()).then_some(Handle(h))
    }

    /// Bytes waiting in a pipe, or `None` when the pipe is closed and empty
    /// (or is not a pipe after all).
    fn pipe_bytes(h: HANDLE) -> Option<u32> {
        let mut avail = 0u32;
        // SAFETY: no buffer is passed; avail is a live u32.
        let ok = unsafe { PeekNamedPipe(h, null_mut(), 0, null_mut(), &mut avail, null_mut()) };
        (ok != 0).then_some(avail)
    }

    /// True when stdin holds input, checked without reading it and without
    /// blocking: bytes waiting in a pipe, or a non-empty file (Git Bash may
    /// pass a heredoc either way). A console, `NUL`, a closed pipe, or an
    /// open pipe that stays empty for `PIPE_GRACE` means no input.
    pub fn stdin_has_input() -> bool {
        let h = std::io::stdin().as_raw_handle() as HANDLE;
        if h.is_null() || h as isize == -1 {
            return false;
        }
        // SAFETY: GetFileType accepts any handle value.
        match unsafe { GetFileType(h) } {
            FILE_TYPE_PIPE => {
                let until = Instant::now() + PIPE_GRACE;
                loop {
                    match pipe_bytes(h) {
                        Some(0) if Instant::now() < until => {
                            sleep(Duration::from_millis(10));
                        }
                        Some(n) => return n > 0,
                        None => return false,
                    }
                }
            }
            FILE_TYPE_DISK => {
                let mut size = 0i64;
                // SAFETY: size is a live i64.
                unsafe { GetFileSizeEx(h, &mut size) != 0 && size > 0 }
            }
            // A console or NUL (FILE_TYPE_CHAR), or unknown.
            _ => false,
        }
    }

    fn image_name(h: HANDLE) -> Option<OsString> {
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        // SAFETY: buf holds len u16 values; len is updated to the length used.
        let ok = unsafe {
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| OsString::from_wide(&buf[..len as usize]))
    }

    /// Stop the watcher with this process ID and wait up to 5 seconds for it
    /// to exit. Refuses a process that is not a `cq` executable.
    pub fn terminate(pid: u32) -> Result<()> {
        let Some(h) = open(
            pid,
            PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
        ) else {
            // Already gone (or not ours to open); the lock retry decides.
            return Ok(());
        };
        let theirs =
            image_name(h.0).and_then(|p| Path::new(&p).file_name().map(|n| n.to_ascii_lowercase()));
        let mine = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
        if !matches!((&theirs, &mine), (Some(t), Some(m)) if super::is_cq_name(t, m)) {
            return Err(CqError::error(format!(
                "process {pid} is not a cq watcher ({theirs:?}); not stopping it"
            )));
        }
        // SAFETY: valid handle with PROCESS_TERMINATE and SYNCHRONIZE access.
        unsafe {
            if TerminateProcess(h.0, 1) == 0 {
                return Err(CqError::error(format!(
                    "could not stop the old watcher (process {pid}): {}",
                    std::io::Error::last_os_error()
                )));
            }
            WaitForSingleObject(h.0, 5000);
        }
        Ok(())
    }

    /// Every process's parent and executable name, from one snapshot.
    pub struct Procs(HashMap<u32, (u32, bool)>);

    impl Procs {
        pub fn snapshot() -> Option<Procs> {
            // SAFETY: plain FFI call; the handle is checked, then closed by Handle.
            let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snap.is_null() || snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let snap = Handle(snap);
            // SAFETY: PROCESSENTRY32W is a plain Win32 output structure.
            let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut map = HashMap::new();
            // SAFETY: snapshot handle and output pointer are valid.
            let mut found = unsafe { Process32FirstW(snap.0, &mut entry) };
            while found != 0 {
                let len = entry.szExeFile.iter().position(|&c| c == 0);
                let exe = OsString::from_wide(&entry.szExeFile[..len.unwrap_or(0)]);
                map.insert(
                    entry.th32ProcessID,
                    (entry.th32ParentProcessID, super::is_claude_name(&exe)),
                );
                // SAFETY: snapshot handle and output pointer remain valid.
                found = unsafe { Process32NextW(snap.0, &mut entry) };
            }
            Some(Procs(map))
        }

        pub fn info(&self, pid: u32) -> Option<super::ProcInfo> {
            let &(ppid, claude) = self.0.get(&pid)?;
            let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: valid handle; four live FILETIME outputs.
            let ok =
                unsafe { GetProcessTimes(h.0, &mut created, &mut exited, &mut kernel, &mut user) };
            let start =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            (ok != 0).then_some(super::ProcInfo {
                ppid,
                start,
                claude,
            })
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, RawFd};
    use std::os::unix::fs::FileTypeExt;
    use std::path::{Path, PathBuf};
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use super::{CqError, PIPE_GRACE, Result};

    /// How long `terminate` waits after SIGTERM before it sends SIGKILL.
    const KILL_AFTER: Duration = Duration::from_secs(5);
    /// A write to a session's inbox socket that takes longer has failed.
    const INBOX_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

    /// A connection to a Claude Code session's inbox socket.
    pub type InboxConn = std::os::unix::net::UnixStream;

    /// Connect to a session's inbox Unix socket.
    pub fn open_inbox(path: &std::ffi::OsStr) -> io::Result<InboxConn> {
        let s = InboxConn::connect(path)?;
        s.set_write_timeout(Some(INBOX_WRITE_TIMEOUT))?;
        Ok(s)
    }

    /// Bytes waiting to be read on `fd` (a pipe or socket), or `None` when
    /// the count is not available.
    fn bytes_waiting(fd: RawFd) -> Option<usize> {
        let mut n: libc::c_int = 0;
        // SAFETY: FIONREAD writes one c_int to a live c_int.
        let r = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) };
        (r == 0).then_some(n.max(0) as usize)
    }

    /// True when stdin holds input, checked without reading it and without
    /// blocking: bytes waiting in a pipe or socket, or a non-empty file. A
    /// terminal, `/dev/null`, a closed pipe, or an open pipe that stays
    /// empty for `PIPE_GRACE` means no input. A socket counts as a pipe:
    /// Node's child-process pipes (Claude Code's tools and hooks) are
    /// socket pairs on Unix.
    pub fn stdin_has_input() -> bool {
        let stdin = io::stdin();
        let fd = stdin.as_fd();
        let Ok(meta) = fd
            .try_clone_to_owned()
            .map(File::from)
            .and_then(|f| f.metadata())
        else {
            return false;
        };
        let kind = meta.file_type();
        if kind.is_file() {
            return meta.len() > 0;
        }
        if !(kind.is_fifo() || kind.is_socket()) {
            // A terminal, /dev/null, or unknown.
            return false;
        }
        let raw = fd.as_raw_fd();
        let until = Instant::now() + PIPE_GRACE;
        loop {
            match bytes_waiting(raw) {
                Some(0) => {}
                Some(_) => return true,
                None => return false,
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let mut p = libc::pollfd {
                fd: raw,
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().clamp(1, 1000) as libc::c_int;
            // SAFETY: one live pollfd.
            let n = unsafe { libc::poll(&mut p, 1, ms) };
            if n > 0 {
                // Readable or hung up: bytes, or the end of the input.
                return bytes_waiting(raw).is_some_and(|b| b > 0);
            }
            if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
            // Timed out or interrupted: check the clock again.
        }
    }

    /// The executable of a running process.
    #[cfg(target_os = "macos")]
    fn exe_path(pid: libc::pid_t) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: buf holds buf.len() bytes; the call writes at most that many.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(PathBuf::from(OsString::from_vec(buf)))
    }

    /// The executable of a running process.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn exe_path(pid: libc::pid_t) -> Option<PathBuf> {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let p = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        // A binary replaced by a rebuild shows as "<path> (deleted)".
        let bytes = p.as_os_str().as_bytes();
        let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
        Some(PathBuf::from(OsString::from_vec(bytes.to_vec())))
    }

    /// The command name of a running process, from `ps`.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    fn exe_path(pid: libc::pid_t) -> Option<PathBuf> {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let name = text.trim();
        (out.status.success() && !name.is_empty()).then(|| PathBuf::from(name))
    }

    fn exe_name(p: &Path) -> Option<OsString> {
        p.file_name().map(|n| n.to_ascii_lowercase())
    }

    /// The bytes before the first NUL.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    fn until_nul(bytes: &[u8]) -> &[u8] {
        bytes.split(|&b| b == 0).next().unwrap_or_default()
    }

    /// True when any of these names is `claude`. A process counts when its
    /// command name, its `argv[0]` or its executable is `claude`: the
    /// native installer starts a versioned file through a `claude` link.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    fn any_claude<'a>(names: impl IntoIterator<Item = &'a [u8]>, pid: libc::pid_t) -> bool {
        use std::os::unix::ffi::OsStrExt;
        names
            .into_iter()
            .any(|n| super::is_claude_name(std::ffi::OsStr::from_bytes(n)))
            || exe_path(pid).is_some_and(|p| super::is_claude_name(p.as_os_str()))
    }

    /// Process details read one process at a time; nothing to snapshot.
    pub struct Procs;

    impl Procs {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
        pub fn snapshot() -> Option<Procs> {
            Some(Procs)
        }

        /// Not supported on other systems: no owner is recorded.
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
        pub fn snapshot() -> Option<Procs> {
            None
        }

        /// From `/proc/<pid>/stat`, `comm`, `cmdline` and `exe`.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        pub fn info(&self, pid: u32) -> Option<super::ProcInfo> {
            let raw = libc::pid_t::try_from(pid).ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // After the command name in brackets come fields 3 onwards:
            // field 4 is the parent ID, field 22 the start time.
            let fields: Vec<&str> = stat
                .get(stat.rfind(')')? + 1..)?
                .split_whitespace()
                .collect();
            let ppid = fields.get(1)?.parse().ok()?;
            let start = fields.get(19)?.parse().ok()?;
            let comm = std::fs::read(format!("/proc/{pid}/comm")).unwrap_or_default();
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let claude = any_claude([comm.trim_ascii_end(), until_nul(&cmdline)], raw);
            Some(super::ProcInfo {
                ppid,
                start,
                claude,
            })
        }

        /// From `proc_pidinfo` and the `KERN_PROCARGS2` arguments.
        #[cfg(target_os = "macos")]
        pub fn info(&self, pid: u32) -> Option<super::ProcInfo> {
            use std::mem::{size_of, zeroed};
            let raw = libc::pid_t::try_from(pid).ok()?;
            // SAFETY: proc_bsdinfo is a plain C output structure.
            let mut bsd: libc::proc_bsdinfo = unsafe { zeroed() };
            let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
            // SAFETY: bsd holds size bytes; the call writes at most that many.
            let n = unsafe {
                libc::proc_pidinfo(
                    raw,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut bsd as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            if n != size {
                return None;
            }
            let comm: Vec<u8> = bsd.pbi_comm.iter().map(|&c| c as u8).collect();
            let name: Vec<u8> = bsd.pbi_name.iter().map(|&c| c as u8).collect();
            let args = procargs(raw).unwrap_or_default();
            let claude = any_claude([until_nul(&comm), until_nul(&name), argv0(&args)], raw);
            Some(super::ProcInfo {
                ppid: bsd.pbi_ppid,
                start: bsd
                    .pbi_start_tvsec
                    .saturating_mul(1_000_000)
                    .saturating_add(bsd.pbi_start_tvusec),
                claude,
            })
        }
    }

    /// A process's `KERN_PROCARGS2` block: the argument count, the
    /// executable path, NUL padding, then the arguments.
    #[cfg(target_os = "macos")]
    fn procargs(pid: libc::pid_t) -> Option<Vec<u8>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut size: libc::size_t = 0;
        // SAFETY: a size query; no buffer is passed.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        // SAFETY: buf holds size bytes; size is updated to the length used.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            return None;
        }
        buf.truncate(size);
        Some(buf)
    }

    /// `argv[0]` from a `KERN_PROCARGS2` block, or nothing.
    #[cfg(target_os = "macos")]
    fn argv0(args: &[u8]) -> &[u8] {
        let Some(rest) = args.get(std::mem::size_of::<libc::c_int>()..) else {
            return &[];
        };
        let path_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let rest = &rest[path_end..];
        let start = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
        until_nul(&rest[start..])
    }

    /// `Ok` when the process runs an executable with our file name, or a
    /// renamed copy of it; else its file name, if known.
    fn check_cq(pid: libc::pid_t) -> std::result::Result<(), Option<OsString>> {
        let theirs = exe_path(pid).and_then(|p| exe_name(&p));
        let mine = std::env::current_exe().ok().and_then(|p| exe_name(&p));
        match (&theirs, &mine) {
            (Some(t), Some(m)) if super::is_cq_name(t, m) => Ok(()),
            _ => Err(theirs),
        }
    }

    /// True while the process exists and we may signal it.
    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks; pid is positive.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Stop the watcher with this process ID: SIGTERM, then SIGKILL if it
    /// has not exited after 5 seconds. Refuses a process that is not a `cq`
    /// executable.
    pub fn terminate(pid: u32) -> Result<()> {
        // 0 and negative values would signal process groups.
        let raw = libc::pid_t::try_from(pid).unwrap_or(0);
        if raw <= 0 || !alive(raw) {
            // Already gone (or not ours to signal); the lock retry decides.
            return Ok(());
        }
        if let Err(theirs) = check_cq(raw) {
            return Err(CqError::error(format!(
                "process {pid} is not a cq watcher ({theirs:?}); not stopping it"
            )));
        }
        // SAFETY: plain FFI call on a positive pid.
        if unsafe { libc::kill(raw, libc::SIGTERM) } != 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(CqError::error(format!(
                "could not stop the old watcher (process {pid}): {e}"
            )));
        }
        let until = Instant::now() + KILL_AFTER;
        while alive(raw) {
            if Instant::now() >= until {
                // Check again: the ID may belong to a new process by now.
                if check_cq(raw).is_ok() {
                    // SAFETY: plain FFI call on a positive pid.
                    unsafe { libc::kill(raw, libc::SIGKILL) };
                }
                break;
            }
            sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

/// An owned agent process. Windows keeps the non-inheritable job handle only
/// in the dispatcher, so even forced dispatcher termination kills the tree.
pub struct Agent {
    pub child: std::process::Child,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
mod agent_windows {
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread,
        THREAD_SUSPEND_RESUME,
    };

    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: successful Win32 call returned a new owned handle.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    // Stable Rust cannot access Child's primary thread handle. A process
    // created suspended has not run any user code; enumerate its initial
    // thread and resume it only after successful job assignment.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: creates a thread snapshot; it is closed by OwnedHandle.
        let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
        // SAFETY: THREADENTRY32 is a plain Win32 output structure.
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // SAFETY: snapshot and output pointer are valid.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: opens only the suspended child's thread.
                let thread =
                    owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
                // SAFETY: thread is an owned handle with suspend/resume access.
                let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
                if previous == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                // A running helper thread is not proof that the suspended
                // primary thread resumed. Continue until a suspended thread ran.
                if previous > 0 {
                    return Ok(());
                }
            }
            // SAFETY: snapshot and output pointer remain valid.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other("could not find suspended agent thread"))
    }

    pub fn spawn(command: &mut std::process::Command) -> io::Result<super::Agent> {
        // SAFETY: null security attributes make the job non-inheritable;
        // null name prevents any other process opening it by name.
        let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        // SAFETY: plain Win32 configuration structure, zero is valid initially.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: live job and correctly sized configuration buffer.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        // SAFETY: Child owns a live process handle. No agent code has run.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if let Err(error) = resume(child.id()) {
            drop(job); // Kills the still-suspended process before returning.
            let _ = child.wait();
            return Err(error);
        }
        Ok(super::Agent { child, job })
    }
}

pub fn spawn_agent(command: &mut std::process::Command) -> std::io::Result<Agent> {
    #[cfg(windows)]
    {
        agent_windows::spawn(command)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        command.spawn().map(|child| Agent { child })
    }
}

/// Stop the owned job/process group, even if its original process just exited.
/// No taskkill invocation or process-ID lookup can race with process exit.
pub fn stop_agent(agent: &mut Agent) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the job handle stays owned by Agent throughout this call.
        if unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(agent.job.as_raw_handle(), 1)
        } == 0
        {
            return Err(CqError::io(
                "stop agent job",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: spawn_agent made this child's PID its process group ID.
        if unsafe { libc::kill(-(agent.child.id() as i32), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(CqError::io("stop agent process group", error));
            }
        }
    }
    agent
        .child
        .wait()
        .map_err(|e| CqError::io("reap agent", e))?;
    Ok(())
}

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// True once Ctrl+C (Windows console) or SIGINT, SIGTERM or SIGHUP (Unix)
/// arrived after `install_interrupt_handler`. Used by `dispatch` and by
/// `watch --to-inbox` and `--to-codex`.
pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn install_interrupt_handler() -> Result<()> {
    INTERRUPTED.store(false, std::sync::atomic::Ordering::Relaxed);
    #[cfg(windows)]
    {
        unsafe extern "system" fn on_interrupt(kind: u32) -> i32 {
            if kind <= 2 {
                INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
                1
            } else {
                0
            }
        }
        // SAFETY: static handler only writes an atomic flag; no borrowed state.
        if unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_interrupt), 1)
        } == 0
        {
            return Err(CqError::io(
                "install interrupt handler",
                std::io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn on_interrupt(_: libc::c_int) {
            INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // SAFETY: static signal handler only writes an atomic flag.
        unsafe {
            if libc::signal(
                libc::SIGINT,
                on_interrupt as *const () as libc::sighandler_t,
            ) == libc::SIG_ERR
                || libc::signal(
                    libc::SIGTERM,
                    on_interrupt as *const () as libc::sighandler_t,
                ) == libc::SIG_ERR
                || libc::signal(
                    libc::SIGHUP,
                    on_interrupt as *const () as libc::sighandler_t,
                ) == libc::SIG_ERR
            {
                return Err(CqError::io(
                    "install signal handler",
                    std::io::Error::last_os_error(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_claude_name, is_cq_name};
    use std::ffi::OsStr;

    #[test]
    fn claude_names() {
        for yes in [
            "claude",
            "claude.exe",
            "Claude.EXE",
            "/usr/local/bin/claude",
        ] {
            assert!(is_claude_name(OsStr::new(yes)), "{yes:?}");
        }
        for no in [
            "node",
            "claude-code",
            "2.1.299",
            "claude.old",
            "",
            "/claude/versions/2",
        ] {
            assert!(!is_claude_name(OsStr::new(no)), "{no:?}");
        }
    }

    fn ok(theirs: &str, mine: &str) -> bool {
        is_cq_name(OsStr::new(theirs), OsStr::new(mine))
    }

    #[test]
    fn accepts_our_name_and_renamed_copies() {
        assert!(ok("cq.exe", "cq.exe"));
        assert!(ok("cq.exe.old-0156", "cq.exe"));
        assert!(ok("cq.old-2026-10-28-2333.exe", "cq.exe"));
        assert!(ok("cq.old.exe", "cq.exe"));
        assert!(ok("cq.exe.old", "cq.exe"));
        assert!(ok("cq", "cq"));
        assert!(ok("cq.old-0156", "cq"));
    }

    #[test]
    fn refuses_other_names() {
        assert!(!ok("notepad.exe", "cq.exe"));
        assert!(!ok("cqx.exe", "cq.exe"));
        assert!(!ok("cq-helper.exe", "cq.exe"));
        assert!(!ok("cq_old.exe", "cq.exe"));
        assert!(!ok("old.cq.exe", "cq.exe"));
        assert!(!ok("cq.helper.exe", "cq.exe"));
        assert!(!ok("cq.exe.bak", "cq.exe"));
        assert!(!ok("cq.older.exe", "cq.exe"));
        assert!(!ok("cq.exe.older", "cq.exe"));
        assert!(!ok("cq.exe.helper", "cq.exe"));
        assert!(!ok("cq.old_1.exe", "cq.exe"));
        assert!(!ok("cq.helper", "cq"));
        assert!(!ok("cqd", "cq"));
        assert!(!ok("", "cq.exe"));
        assert!(!ok("cq.exe", ""));
    }
}
