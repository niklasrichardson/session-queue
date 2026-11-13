//! The trusted inputs of `cq watch --to-agy-session`, built once at start,
//! and the `Link` to the launching Antigravity session.
//!
//! Nothing here reads `std::env` or `Root::from_env`. The root is the
//! build's `CQ_DEFAULT_ROOT`; the policy is the `[agy-session.NAME]` table
//! of its `config.toml`; the binary and the address are tied to the
//! launching session's own process. The root, `config.toml` and `cq.exe`,
//! and every folder above them, must not be writable from the sandbox.
//! Every refusal is exit 64, before any queue write.
//!
//! The language server address and the CSRF token are never printed,
//! logged or recorded; no error here holds the address.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::config::{self, Config};
use crate::error::{CqError, Result};
use crate::exchange::{imp, valid_sid};
use crate::message::breaks_line;
use crate::names::check_name;
use crate::root::Root;
use crate::wake::{AGENTAPI_VAR, CONVERSATION_VAR, Link};

/// The process names of an Antigravity session: the CLI and the App's
/// language server.
const LAUNCHERS: [&str; 2] = ["agy.exe", "language_server.exe"];
/// The most parents walked to find the launching session.
const MAX_DEPTH: usize = 32;
/// The largest `config.toml` read.
const CONFIG_CAP: usize = 64 * 1024;
/// The longest a reachability connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest deadline the policy may set.
const MAX_MINUTES: u64 = 24 * 60;

/// The `[agy-session.NAME]` table, read strictly: an unknown key or a bad
/// value is exit 64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub conversation: String,
    pub allow_from: Vec<String>,
    /// In v1, exactly one: the SID the exchange folder grants write to.
    /// The root checks refuse a write grant to it too.
    pub sandbox_sids: Vec<String>,
    pub ack_minutes: u64,
    pub reply_minutes: u64,
    pub resume_minutes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    conversation: String,
    allow_from: Vec<String>,
    sandbox_sids: Vec<String>,
    #[serde(default = "ack_default")]
    ack_minutes: u64,
    #[serde(default = "reply_default")]
    reply_minutes: u64,
    #[serde(default = "resume_default")]
    resume_minutes: u64,
}

fn ack_default() -> u64 {
    15
}
fn reply_default() -> u64 {
    60
}
fn resume_default() -> u64 {
    10
}

impl Policy {
    /// The `[agy-session.NAME]` table in `text`.
    pub fn parse(text: &str, name: &str) -> Result<Policy> {
        let bad = |why: String| {
            CqError::usage(format!("[agy-session.{name}] in {}: {why}", config::FILE))
        };
        let table: toml::Table =
            toml::from_str(text).map_err(|e| bad(format!("not valid TOML ({})", e.message())))?;
        let value = table
            .get("agy-session")
            .and_then(|t| t.get(name))
            .ok_or_else(|| bad("there is no such table; add it to use session mode".into()))?;
        let t: Table = value
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| bad(e.message().to_owned()))?;
        if t.conversation.is_empty() || t.conversation.chars().any(breaks_line) {
            return Err(bad("conversation must be one non-empty line".into()));
        }
        if t.allow_from.is_empty() {
            return Err(bad("allow_from must name at least one sender".into()));
        }
        for n in &t.allow_from {
            check_name("an allow_from name", n).map_err(|e| bad(e.message))?;
        }
        match t.sandbox_sids.as_slice() {
            [sid] if valid_sid(sid) => {}
            [_] => return Err(bad("sandbox_sids holds an invalid SID".into())),
            _ => return Err(bad("sandbox_sids must hold exactly one SID".into())),
        }
        for (key, m) in [
            ("ack_minutes", t.ack_minutes),
            ("reply_minutes", t.reply_minutes),
            ("resume_minutes", t.resume_minutes),
        ] {
            if !(1..=MAX_MINUTES).contains(&m) {
                return Err(bad(format!("{key} must be 1 to {MAX_MINUTES}")));
            }
        }
        Ok(Policy {
            conversation: t.conversation,
            allow_from: t.allow_from,
            sandbox_sids: t.sandbox_sids,
            ack_minutes: t.ack_minutes,
            reply_minutes: t.reply_minutes,
            resume_minutes: t.resume_minutes,
        })
    }
}

/// One process in a snapshot.
#[derive(Debug, Clone)]
pub struct Entry {
    pub pid: u32,
    pub parent: u32,
    /// The executable's file name.
    pub name: String,
}

/// An open process.
pub trait Process {
    /// The creation time, in Windows file time units.
    fn created(&self) -> u64;
    /// The full path of its image.
    fn image(&self) -> io::Result<PathBuf>;
    /// It has not exited.
    fn alive(&self) -> bool;
}

/// What session mode asks Windows. Tests use a fake.
pub trait System {
    fn processes(&self) -> io::Result<Vec<Entry>>;
    fn open(&self, pid: u32) -> io::Result<Box<dyn Process>>;
    /// The listening TCP sockets and the process that owns each.
    fn listeners(&self) -> io::Result<Vec<(SocketAddr, u32)>>;
}

/// Where the inputs come from. Only `main` builds the real one.
pub struct Sources<'a> {
    /// The build's `CQ_DEFAULT_ROOT`.
    pub build_root: Option<PathBuf>,
    /// This `cq.exe`.
    pub exe: PathBuf,
    /// This process.
    pub pid: u32,
    /// Reads one environment variable. Asked only for the address and the
    /// cross-check variables.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub system: Box<dyn System>,
}

/// Everything session mode uses, checked. No `Debug`: the link holds the
/// address.
pub struct SessionInputs {
    pub root: Root,
    pub policy: Policy,
    /// The other settings in the pinned `config.toml`, and its warning.
    pub settings: Config,
    pub warning: Option<String>,
    /// The conversation every wake goes to. `conversation_of` picks its
    /// source; nothing else reads it from the policy or the environment.
    pub conversation: String,
    /// The launching session's image, run as `agentapi`.
    pub agentapi: PathBuf,
    /// The launching session's process ID.
    pub launcher: u32,
    pub link: SessionLink,
    /// The root, `config.toml` and `cq.exe` folders, held open without
    /// delete sharing for the watcher's life.
    pub pins: Vec<OwnedHandle>,
}

impl SessionInputs {
    pub fn build(name: &str, src: Sources) -> Result<SessionInputs> {
        let path = src.build_root.filter(|p| p.is_absolute()).ok_or_else(|| {
            CqError::usage(
                "this cq was built without an absolute CQ_DEFAULT_ROOT, which session mode needs; \
                 rebuild it with CQ_DEFAULT_ROOT set",
            )
        })?;
        let config_path = path.join(config::FILE);
        // A first pass with the checks that need no policy, to read it.
        pin(&path, false, None, &[], "the queue root")?;
        let (_, first) = pin(&config_path, true, Some(CONFIG_CAP), &[], "config.toml")?;
        let first = first.unwrap_or_default();
        let text = config::text_of(&first)
            .map_err(|why| CqError::usage(format!("{}: {why}", config::FILE)))?;
        let policy = Policy::parse(text, name)?;

        // The full checks, with the policy's sandbox SIDs, on what stays
        // pinned.
        let sids = &policy.sandbox_sids;
        let mut pins = pin(&path, false, None, sids, "the queue root")?.0;
        let (more, bytes) = pin(&config_path, true, Some(CONFIG_CAP), sids, "config.toml")?;
        pins.extend(more);
        if bytes.as_deref() != Some(first.as_slice()) {
            return Err(CqError::usage(format!(
                "{} changed while cq read it; start again",
                config::FILE
            )));
        }
        if src.exe.as_os_str().to_string_lossy().contains(' ') {
            return Err(CqError::usage(
                "the cq.exe path holds a space; install cq in a folder without spaces",
            ));
        }
        pins.extend(pin(&src.exe, true, None, sids, "cq.exe")?.0);
        let root = Root::pinned(path);
        if !root.ready() {
            return Err(CqError::usage(
                "the queue root is not initialised; run `cq init` first",
            ));
        }

        let conversation = conversation_of(&policy, src.env)?;
        let (launcher, process, agentapi) = find_launcher(&*src.system, src.pid)?;
        if let Some(named) = (src.env)(AGENTAPI_VAR).filter(|v| !v.is_empty())
            && !same_file(Path::new(&named), &agentapi)
        {
            return Err(CqError::usage(format!(
                "{AGENTAPI_VAR} names another program than the launching session's own; \
                 session mode runs only that one"
            )));
        }
        let address = (src.env)(crate::wake::ADDRESS_VAR)
            .and_then(|v| v.into_string().ok())
            .ok_or_else(|| {
                CqError::usage(format!(
                    "{} is not set; start the watcher from an Antigravity session",
                    crate::wake::ADDRESS_VAR
                ))
            })?;
        let address = Address::parse(&address).ok_or_else(|| {
            CqError::usage(format!(
                "{} is not a loopback host and port",
                crate::wake::ADDRESS_VAR
            ))
        })?;
        let listeners = src
            .system
            .listeners()
            .map_err(|e| CqError::usage(format!("list the listening sockets: {e}")))?;
        if let Some(why) = address.problem(&listeners, launcher) {
            return Err(CqError::usage(format!(
                "{} does not belong to the launching session: {why}",
                crate::wake::ADDRESS_VAR
            )));
        }

        let (settings, warning) = Config::from_bytes(&first, root.path());
        Ok(SessionInputs {
            root,
            policy,
            settings,
            warning,
            conversation,
            agentapi,
            launcher,
            link: SessionLink {
                process,
                pid: launcher,
                address,
                system: src.system,
            },
            pins,
        })
    }
}

/// `exchange::imp::pin_trusted`, with a refusal as exit 64.
fn pin(
    path: &Path,
    file: bool,
    read: Option<usize>,
    sids: &[String],
    what: &str,
) -> Result<(Vec<OwnedHandle>, Option<Vec<u8>>)> {
    imp::pin_trusted(path, file, read, sids).map_err(|e| {
        CqError::usage(format!(
            "{what} fails the session-mode checks: {e}. Its path must have no links or \
             junctions and must not be writable from the sandbox"
        ))
    })
}

/// The one place that picks where the conversation ID comes from: the
/// owner's policy. `CONVERSATION_VAR` only cross-checks it.
pub fn conversation_of(policy: &Policy, env: &dyn Fn(&str) -> Option<OsString>) -> Result<String> {
    cross_check(CONVERSATION_VAR, env, &policy.conversation)?;
    Ok(policy.conversation.clone())
}

/// When the cross-check variable `var` is set, it must name `conversation`.
fn cross_check(
    var: &str,
    env: &dyn Fn(&str) -> Option<OsString>,
    conversation: &str,
) -> Result<()> {
    if var.is_empty() {
        return Ok(());
    }
    match env(var).filter(|v| !v.is_empty()) {
        Some(v) if v != conversation => Err(CqError::usage(format!(
            "{var} names another conversation than the policy in {}",
            config::FILE
        ))),
        _ => Ok(()),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy()),
        _ => false,
    }
}

/// The nearest `agy.exe` or `language_server.exe` above `pid`, its open
/// process and its image. Each parent must have started before its child,
/// so a reused process ID is not taken for a parent.
fn find_launcher(system: &dyn System, pid: u32) -> Result<(u32, Box<dyn Process>, PathBuf)> {
    let none = || {
        CqError::usage(
            "no agy.exe or language_server.exe above this process; start the watcher from an \
             Antigravity session",
        )
    };
    let entries = system
        .processes()
        .map_err(|e| CqError::usage(format!("list the processes: {e}")))?;
    let entries: HashMap<u32, Entry> = entries.into_iter().map(|e| (e.pid, e)).collect();
    let open = |pid: u32| {
        system
            .open(pid)
            .map_err(|e| CqError::usage(format!("open process {pid}: {e}")))
    };
    let mut child = open(pid)?;
    let mut child_pid = pid;
    for _ in 0..MAX_DEPTH {
        let Some(parent) = entries.get(&child_pid).map(|e| e.parent) else {
            break;
        };
        let Some(entry) = entries
            .get(&parent)
            .filter(|_| parent != 0 && parent != child_pid)
        else {
            break;
        };
        let process = open(parent)?;
        if process.created() >= child.created() {
            return Err(CqError::usage(format!(
                "process {parent} started after its child {child_pid}, so it is not its parent; \
                 the parent chain is broken"
            )));
        }
        if LAUNCHERS.iter().any(|l| entry.name.eq_ignore_ascii_case(l)) {
            let image = process
                .image()
                .map_err(|e| CqError::usage(format!("read the image of process {parent}: {e}")))?;
            let file = image.file_name().map(|f| f.to_string_lossy().into_owned());
            if !file.is_some_and(|f| f.eq_ignore_ascii_case(&entry.name)) {
                return Err(CqError::usage(format!(
                    "process {parent}'s image is not {}",
                    entry.name
                )));
            }
            return Ok((parent, process, image));
        }
        child = process;
        child_pid = parent;
    }
    Err(none())
}

/// A loopback language server address: the IPs a connect may reach, and
/// the port. No `Debug`, so it is never printed.
pub struct Address {
    ips: Vec<IpAddr>,
    port: u16,
}

impl Address {
    /// `[scheme://]host:port[/...]` with host `127.0.0.1`, `localhost` or
    /// `[::1]`.
    fn parse(text: &str) -> Option<Address> {
        let rest = text.split_once("://").map_or(text, |(_, r)| r);
        let host_port = rest.split('/').next()?;
        let (host, port) = host_port.rsplit_once(':')?;
        let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let ips = match host {
            "127.0.0.1" => vec![v4],
            "[::1]" => vec![v6],
            h if h.eq_ignore_ascii_case("localhost") => vec![v4, v6],
            _ => return None,
        };
        Some(Address { ips, port })
    }

    /// Why `listeners` show the address is not `pid`'s: another process
    /// listens where a connect could land, or nothing does.
    fn problem(&self, listeners: &[(SocketAddr, u32)], pid: u32) -> Option<&'static str> {
        let mut found = false;
        for (at, owner) in listeners {
            let takes = at.port() == self.port
                && self.ips.iter().any(|ip| {
                    at.ip() == *ip || (at.ip().is_unspecified() && at.is_ipv4() == ip.is_ipv4())
                });
            if !takes {
                continue;
            }
            if *owner != pid {
                return Some("another process listens on it");
            }
            found = true;
        }
        (!found).then_some("nothing listens on it")
    }
}

/// The launching session, as the watcher checks it while it runs.
pub struct SessionLink {
    process: Box<dyn Process>,
    pid: u32,
    address: Address,
    system: Box<dyn System>,
}

impl Link for SessionLink {
    fn alive(&mut self) -> bool {
        self.process.alive()
    }

    fn reachable(&mut self) -> bool {
        self.address.ips.iter().any(|ip| {
            TcpStream::connect_timeout(&SocketAddr::new(*ip, self.address.port), CONNECT_TIMEOUT)
                .is_ok()
        })
    }

    fn owned(&mut self) -> bool {
        self.system
            .listeners()
            .is_ok_and(|l| self.address.problem(&l, self.pid).is_none())
    }
}

/// Windows itself.
pub struct Os;

mod os {
    use std::ffi::OsString;
    use std::io;
    use std::mem::{offset_of, size_of, zeroed};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::PathBuf;
    use std::ptr::{null_mut, read_unaligned};

    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
    };

    use super::{Entry, Os, Process, System};

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;

    fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: successful Win32 call returned a new owned handle.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    fn wide_name(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    struct OsProcess {
        h: OwnedHandle,
        created: u64,
    }

    impl Process for OsProcess {
        fn created(&self) -> u64 {
            self.created
        }

        fn image(&self) -> io::Result<PathBuf> {
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            // SAFETY: buf holds len u16 values; len is updated to the
            // length used.
            let ok = unsafe {
                QueryFullProcessImageNameW(
                    self.h.as_raw_handle(),
                    PROCESS_NAME_WIN32,
                    buf.as_mut_ptr(),
                    &mut len,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
        }

        fn alive(&self) -> bool {
            // SAFETY: valid handle with SYNCHRONIZE access; a zero wait only
            // checks.
            unsafe { WaitForSingleObject(self.h.as_raw_handle(), 0) == WAIT_TIMEOUT }
        }
    }

    /// The listeners of `family`: the rows from byte `rows` on, each read
    /// by `row`.
    fn table<R>(
        family: u32,
        rows: usize,
        row: impl Fn(&R) -> (SocketAddr, u32),
    ) -> io::Result<Vec<(SocketAddr, u32)>> {
        let mut size = 0u32;
        let mut buf: Vec<u64> = Vec::new();
        loop {
            // SAFETY: buf holds `size` bytes (or is empty with size 0, which
            // only asks for the size).
            let err = unsafe {
                GetExtendedTcpTable(
                    if buf.is_empty() {
                        null_mut()
                    } else {
                        buf.as_mut_ptr().cast()
                    },
                    &mut size,
                    0,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            match err {
                NO_ERROR if !buf.is_empty() => break,
                NO_ERROR | ERROR_INSUFFICIENT_BUFFER => {
                    buf = vec![0u64; (size as usize).div_ceil(8) + 1];
                    size = (buf.len() * 8) as u32;
                }
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        let bytes = buf.as_ptr().cast::<u8>();
        let len = size as usize;
        if len < 4 {
            return Err(io::Error::other("short TCP table"));
        }
        // SAFETY: the table starts with its u32 row count.
        let n = unsafe { read_unaligned(bytes.cast::<u32>()) } as usize;
        if rows + n * size_of::<R>() > len {
            return Err(io::Error::other("short TCP table"));
        }
        Ok((0..n)
            .map(|i| {
                // SAFETY: checked above that row i lies inside the buffer.
                let r: R = unsafe { read_unaligned(bytes.add(rows + i * size_of::<R>()).cast()) };
                row(&r)
            })
            .collect())
    }

    fn port(raw: u32) -> u16 {
        u16::from_be((raw & 0xFFFF) as u16)
    }

    impl System for Os {
        fn processes(&self) -> io::Result<Vec<Entry>> {
            // SAFETY: plain FFI call; the result is checked.
            let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
            // SAFETY: a plain-data struct; zero is a valid value.
            let mut e: PROCESSENTRY32W = unsafe { zeroed() };
            e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut out = Vec::new();
            // SAFETY: valid snapshot handle and a sized entry.
            let mut ok = unsafe { Process32FirstW(snap.as_raw_handle(), &mut e) };
            while ok != 0 {
                out.push(Entry {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    name: wide_name(&e.szExeFile),
                });
                // SAFETY: as above.
                ok = unsafe { Process32NextW(snap.as_raw_handle(), &mut e) };
            }
            Ok(out)
        }

        fn open(&self, pid: u32) -> io::Result<Box<dyn Process>> {
            // SAFETY: plain FFI call; the result is checked.
            let h = owned(unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            })?;
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: valid handle with query access; four out structs.
            let ok = unsafe {
                GetProcessTimes(
                    h.as_raw_handle(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let created =
                (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            Ok(Box::new(OsProcess { h, created }))
        }

        fn listeners(&self) -> io::Result<Vec<(SocketAddr, u32)>> {
            let mut out = table::<MIB_TCPROW_OWNER_PID>(
                AF_INET,
                offset_of!(MIB_TCPTABLE_OWNER_PID, table),
                |r| {
                    let ip = Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes());
                    (
                        SocketAddr::new(ip.into(), port(r.dwLocalPort)),
                        r.dwOwningPid,
                    )
                },
            )?;
            out.extend(table::<MIB_TCP6ROW_OWNER_PID>(
                AF_INET6,
                offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
                |r| {
                    let ip = Ipv6Addr::from(r.ucLocalAddr);
                    (
                        SocketAddr::new(ip.into(), port(r.dwLocalPort)),
                        r.dwOwningPid,
                    )
                },
            )?);
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests;
