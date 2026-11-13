use std::cell::RefCell;
use std::fs;
use std::net::TcpListener;
use std::os::windows::fs::OpenOptionsExt;

use super::*;
use crate::error::exit;
use crate::exchange::imp::{MODIFY, allow, descriptor, set_dacl, set_low_label};
use crate::wake::ADDRESS_VAR;

const SID: &str = "S-1-15-3-1024-1-2-3-4-5-6-7-8";
const NAME: &str = "w";
const SELF: u32 = 100;
const SHELL: u32 = 50;
const SERVER: u32 = 40;
const PORT: u16 = 5000;

struct FakeProcess {
    created: u64,
    image: PathBuf,
}

impl Process for FakeProcess {
    fn created(&self) -> u64 {
        self.created
    }
    fn image(&self) -> io::Result<PathBuf> {
        Ok(self.image.clone())
    }
    fn alive(&self) -> bool {
        true
    }
}

/// cq (100) under a shell (50) under the App's language server (40).
#[derive(Clone)]
struct FakeSystem {
    /// pid, parent, name, creation time.
    procs: Vec<(u32, u32, &'static str, u64)>,
    listeners: Vec<(SocketAddr, u32)>,
    server_image: PathBuf,
}

impl FakeSystem {
    fn new(server_image: PathBuf) -> Self {
        FakeSystem {
            procs: vec![
                (SELF, SHELL, "cq.exe", 30),
                (SHELL, SERVER, "pwsh.exe", 20),
                (SERVER, 4, "language_server.exe", 10),
                (4, 0, "System", 1),
            ],
            listeners: vec![(SocketAddr::from(([127, 0, 0, 1], PORT)), SERVER)],
            server_image,
        }
    }
}

impl System for FakeSystem {
    fn processes(&self) -> io::Result<Vec<Entry>> {
        Ok(self
            .procs
            .iter()
            .map(|&(pid, parent, name, _)| Entry {
                pid,
                parent,
                name: name.into(),
            })
            .collect())
    }
    fn open(&self, pid: u32) -> io::Result<Box<dyn Process>> {
        let &(_, _, name, created) = self
            .procs
            .iter()
            .find(|p| p.0 == pid)
            .ok_or_else(|| io::Error::other("no such process"))?;
        let image = if pid == SERVER {
            self.server_image.clone()
        } else {
            PathBuf::from(format!("C:/x/{name}"))
        };
        Ok(Box::new(FakeProcess { created, image }))
    }
    fn listeners(&self) -> io::Result<Vec<(SocketAddr, u32)>> {
        Ok(self.listeners.clone())
    }
}

const POLICY: &str = r#"
rules = "rules.md"

[agy-session.w]
conversation = "conv-1"
allow_from = ["lead"]
sandbox_sids = ["S-1-15-3-1024-1-2-3-4-5-6-7-8"]
reply_minutes = 30
"#;

/// A root with `config.toml`, a `cq.exe`, and a decoy root with its own
/// policy, in a temp folder that passes the checks itself.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    /// In the build folder, or else the temp folder (which often grants
    /// write to all app packages); `None` when both fail the checks, so the
    /// test cannot run here.
    fn new() -> Option<Self> {
        let exe = std::env::current_exe().unwrap();
        let bases = [exe.parent().unwrap().to_path_buf(), std::env::temp_dir()];
        let mut problems = Vec::new();
        let tmp = bases.iter().find_map(|base| {
            let tmp = tempfile::tempdir_in(base).ok()?;
            match imp::pin_trusted(tmp.path(), false, None, &[SID.into()]) {
                Ok(_) => Some(tmp),
                Err(e) => {
                    problems.push(e.to_string());
                    None
                }
            }
        });
        let Some(tmp) = tmp else {
            eprintln!("no folder passes the session-mode checks ({problems:?}); skipped");
            return None;
        };
        let f = Fixture { tmp };
        for (dir, conv) in [("cq", "conv-1"), ("decoy", "conv-evil")] {
            let root = Root::at(f.path(dir));
            root.init().unwrap();
            fs::write(
                root.path().join(config::FILE),
                POLICY.replace("conv-1", conv),
            )
            .unwrap();
        }
        fs::create_dir(f.path("bin")).unwrap();
        fs::write(f.exe(), "").unwrap();
        fs::write(f.path("bin/language_server.exe"), "").unwrap();
        Some(f)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    fn exe(&self) -> PathBuf {
        self.path("bin/cq.exe")
    }

    fn system(&self) -> FakeSystem {
        FakeSystem::new(self.path("bin/language_server.exe"))
    }

    fn build_with(
        &self,
        root: PathBuf,
        system: FakeSystem,
        env: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<SessionInputs> {
        SessionInputs::build(
            NAME,
            Sources {
                build_root: Some(root),
                exe: self.exe(),
                pid: SELF,
                env,
                system: Box::new(system),
            },
        )
    }

    fn build(&self) -> Result<SessionInputs> {
        self.build_with(self.path("cq"), self.system(), &good_env)
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut todo = vec![self.path("cq")];
        while let Some(d) = todo.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    todo.push(p.clone());
                }
                out.push(p);
            }
        }
        out.sort();
        out
    }
}

fn good_env(var: &str) -> Option<OsString> {
    (var == ADDRESS_VAR).then(|| format!("http://127.0.0.1:{PORT}/").into())
}

fn code(r: Result<SessionInputs>) -> i32 {
    match r {
        Ok(_) => 0,
        Err(e) => e.code,
    }
}

fn message(r: Result<SessionInputs>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => e.message,
    }
}

fn open_with(path: &Path, access: u32) -> OwnedHandle {
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let f = fs::OpenOptions::new()
        .access_mode(access)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    OwnedHandle::from(f)
}

// --- Section 11: hostile environment --------------------------------------

#[test]
fn a_hostile_environment_changes_nothing() {
    let Some(f) = Fixture::new() else { return };
    let decoy = f.path("decoy").into_os_string();
    let asked = RefCell::new(Vec::new());
    let env = |var: &str| -> Option<OsString> {
        asked.borrow_mut().push(var.to_owned());
        if var == ADDRESS_VAR {
            return good_env(var);
        }
        if var == AGENTAPI_VAR {
            return Some(f.path("bin/language_server.exe").into_os_string());
        }
        if var == CONVERSATION_VAR {
            // Trusted input, set by Antigravity; it must match the policy.
            return Some("conv-1".into());
        }
        // CQ_ROOT, LOCALAPPDATA, USERPROFILE, HOME, CQ_SESSION_ID, CQ_RULES,
        // CQ_TZ, PATH and the rest: all point at the decoy.
        Some(decoy.clone())
    };
    let inputs = f.build_with(f.path("cq"), f.system(), &env).unwrap();
    assert_eq!(inputs.root.path(), f.path("cq"));
    assert_eq!(inputs.conversation, "conv-1");
    assert_eq!(inputs.policy.allow_from, ["lead"]);
    assert_eq!(
        (
            inputs.policy.ack_minutes,
            inputs.policy.reply_minutes,
            inputs.policy.resume_minutes
        ),
        (15, 30, 10)
    );
    assert_eq!(inputs.agentapi, f.path("bin/language_server.exe"));
    assert_eq!(inputs.launcher, SERVER);
    assert_eq!(
        inputs.settings.rules.as_deref().map(Path::new),
        Some(f.path("cq").join("rules.md").as_path())
    );
    // Only the address, the conversation and the agentapi cross-check are
    // read from the environment.
    for var in asked.borrow().iter() {
        assert!(
            [ADDRESS_VAR, AGENTAPI_VAR, CONVERSATION_VAR].contains(&var.as_str()),
            "read {var}"
        );
    }
}

#[test]
fn a_build_without_a_default_root_is_refused() {
    let r = SessionInputs::build(
        NAME,
        Sources {
            build_root: None,
            exe: PathBuf::from("C:/x/cq.exe"),
            pid: SELF,
            env: &good_env,
            system: Box::new(FakeSystem::new("C:/x/language_server.exe".into())),
        },
    );
    assert_eq!(code(r), exit::USAGE);
}

#[test]
fn a_low_labelled_root_is_refused_before_any_write() {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    };
    let Some(f) = Fixture::new() else { return };
    let before = f.files();
    // WRITE_OWNER (to set a label) needs a grant the owner may not have.
    let h = open_with(&f.path("cq"), READ_CONTROL | WRITE_DAC);
    let d = descriptor(&h).unwrap();
    let mut aces = d.dacl.unwrap();
    aces.push(allow(FILE_ALL_ACCESS, 0, &d.owner));
    set_dacl(&h, &aces, false).unwrap();
    drop(h);
    set_low_label(&open_with(&f.path("cq"), READ_CONTROL | WRITE_OWNER)).unwrap();
    let m = message(f.build());
    assert!(m.contains("the queue root fails"), "{m}");
    assert_eq!(f.files(), before);
}

#[test]
fn a_config_writable_by_the_sandbox_is_refused() {
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    let Some(f) = Fixture::new() else { return };
    let path = f.path("cq").join(config::FILE);
    let h = open_with(&path, READ_CONTROL | WRITE_DAC);
    let mut aces = descriptor(&h).unwrap().dacl.unwrap();
    aces.push(allow(MODIFY, 0, SID));
    set_dacl(&h, &aces, true).unwrap();
    drop(h);
    let m = message(f.build());
    assert!(m.contains("config.toml fails"), "{m}");
}

/// The live S1 finding: an inheritable Modify entry on the root for a
/// capability SID that the policy does not list.
#[test]
fn a_root_writable_by_any_capability_sid_is_refused() {
    use crate::exchange::imp::INHERIT_FLAGS;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    let Some(f) = Fixture::new() else { return };
    let before = f.files();
    let h = open_with(&f.path("cq"), READ_CONTROL | WRITE_DAC);
    let mut aces = descriptor(&h).unwrap().dacl.unwrap();
    aces.push(allow(MODIFY, INHERIT_FLAGS, "S-1-15-3-1024-9-9-9"));
    set_dacl(&h, &aces, false).unwrap();
    drop(h);
    let m = message(f.build());
    assert!(m.contains("the queue root fails"), "{m}");
    assert!(m.contains("S-1-15-3-1024-9-9-9"), "{m}");
    assert_eq!(f.files(), before);
}

#[test]
fn a_root_reached_through_a_junction_is_refused() {
    let Some(f) = Fixture::new() else { return };
    let ok = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(f.path("jn"))
        .arg(f.path("cq"))
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "mklink /J failed");
    let r = f.build_with(f.path("jn"), f.system(), &good_env);
    assert_eq!(code(r), exit::USAGE);
}

#[test]
fn a_root_without_a_policy_table_is_refused() {
    let Some(f) = Fixture::new() else { return };
    fs::write(f.path("cq").join(config::FILE), "tz = \"UTC\"\n").unwrap();
    let m = message(f.build());
    assert!(m.contains("[agy-session.w]"), "{m}");
}

#[test]
fn an_agentapi_variable_naming_another_program_is_refused() {
    let Some(f) = Fixture::new() else { return };
    let env = |var: &str| match var {
        AGENTAPI_VAR => Some(f.exe().into_os_string()),
        _ => good_env(var),
    };
    let r = f.build_with(f.path("cq"), f.system(), &env);
    assert_eq!(code(r), exit::USAGE);
}

#[test]
fn no_address_is_refused_and_no_error_holds_the_address() {
    let Some(f) = Fixture::new() else { return };
    let none = |_: &str| None;
    assert_eq!(
        code(f.build_with(f.path("cq"), f.system(), &none)),
        exit::USAGE
    );
    let remote = |var: &str| (var == ADDRESS_VAR).then(|| "10.1.2.3:5000".into());
    let m = message(f.build_with(f.path("cq"), f.system(), &remote));
    assert!(m.contains("not a loopback"), "{m}");
    assert!(!m.contains("10.1.2.3"), "{m}");
}

// --- Section 11: address, parent and conversation ---------------------------

#[test]
fn an_address_owned_by_another_process_is_refused() {
    let Some(f) = Fixture::new() else { return };
    let mut system = f.system();
    system.listeners = vec![(SocketAddr::from(([127, 0, 0, 1], PORT)), 999)];
    let m = message(f.build_with(f.path("cq"), system, &good_env));
    assert!(m.contains("another process listens on it"), "{m}");
    assert!(!m.contains(&PORT.to_string()), "{m}");
}

#[test]
fn the_address_owner_rules() {
    let a = Address::parse("localhost:5000").unwrap();
    let at = |ip: [u8; 4], pid| (SocketAddr::from((ip, PORT)), pid);
    let v6 = |pid| (SocketAddr::from((Ipv6Addr::LOCALHOST, PORT)), pid);
    assert_eq!(a.problem(&[at([127, 0, 0, 1], 7)], 7), None);
    assert_eq!(a.problem(&[at([0, 0, 0, 0], 7)], 7), None);
    // Any listener a connect could reach must be the launcher's.
    assert!(a.problem(&[at([127, 0, 0, 1], 7), v6(8)], 7).is_some());
    assert!(a.problem(&[at([0, 0, 0, 0], 8)], 7).is_some());
    assert!(
        a.problem(&[at([10, 0, 0, 1], 7)], 7).is_some(),
        "not loopback"
    );
    assert!(a.problem(&[], 7).is_some());
    let a = Address::parse("127.0.0.1:5000").unwrap();
    assert_eq!(a.problem(&[at([127, 0, 0, 1], 7), v6(8)], 7), None);

    for bad in [
        "10.0.0.1:5000",
        "example.com:1",
        "127.0.0.1",
        "127.0.0.1:0",
        "::1:5000",
    ] {
        assert!(Address::parse(bad).is_none(), "{bad}");
    }
    for good in [
        "[::1]:5000",
        "https://LOCALHOST:5000/path",
        "127.0.0.1:5000",
    ] {
        assert!(Address::parse(good).is_some(), "{good}");
    }
}

#[test]
fn a_parent_that_started_after_its_child_is_refused() {
    let mut system = FakeSystem::new("C:/x/language_server.exe".into());
    // The shell's ID was reused by a process started after cq.
    system.procs[1].3 = 40;
    let e = find_launcher(&system, SELF).err().expect("refused");
    assert_eq!(e.code, exit::USAGE);
    assert!(e.message.contains("started after its child"), "{e}");
}

#[test]
fn the_nearest_launcher_is_found_and_none_is_refused() {
    let system = FakeSystem::new("C:/x/language_server.exe".into());
    let (pid, _, image) = find_launcher(&system, SELF).unwrap();
    assert_eq!(
        (pid, image),
        (SERVER, PathBuf::from("C:/x/language_server.exe"))
    );

    let mut cli = system.clone();
    cli.procs[2].2 = "agy.exe";
    cli.server_image = "C:/x/AGY.EXE".into();
    assert_eq!(find_launcher(&cli, SELF).unwrap().0, SERVER);

    let mut none = system.clone();
    none.procs[2].2 = "explorer.exe";
    assert_eq!(find_launcher(&none, SELF).err().unwrap().code, exit::USAGE);

    // A process named like a launcher whose image is another file.
    let mut fake = system;
    fake.server_image = "C:/x/other.exe".into();
    assert_eq!(find_launcher(&fake, SELF).err().unwrap().code, exit::USAGE);
}

#[test]
fn the_conversation_comes_from_the_variable_then_the_policy() {
    let policy = |c: Option<&str>| {
        let mut p = Policy::parse(POLICY, NAME).unwrap();
        p.conversation = c.map(str::to_owned);
        p
    };
    let var = |id: &'static str| move |v: &str| (v == CONVERSATION_VAR).then(|| OsString::from(id));
    let unset = |_: &str| None;
    let empty = var("");
    // The variable alone, or both the same: the variable.
    assert_eq!(
        conversation_of(&policy(None), &var("env-1")).unwrap(),
        "env-1"
    );
    assert_eq!(
        conversation_of(&policy(Some("env-1")), &var("env-1")).unwrap(),
        "env-1"
    );
    // No variable (or an empty one): the policy's value.
    assert_eq!(
        conversation_of(&policy(Some("conv-1")), &unset).unwrap(),
        "conv-1"
    );
    assert_eq!(
        conversation_of(&policy(Some("conv-1")), &empty).unwrap(),
        "conv-1"
    );
    // Both and different, neither, or a variable with a line break: exit 64.
    for (p, env) in [
        (
            policy(Some("conv-1")),
            &var("env-1") as &dyn Fn(&str) -> Option<OsString>,
        ),
        (policy(None), &unset),
        (policy(None), &var("a\nb")),
    ] {
        assert_eq!(conversation_of(&p, env).unwrap_err().code, exit::USAGE);
    }
}

#[test]
fn the_antigravity_conversation_variable_is_cross_checked() {
    assert_eq!(CONVERSATION_VAR, "ANTIGRAVITY_CONVERSATION_ID");
    let Some(f) = Fixture::new() else { return };
    let with = |id: &'static str| {
        move |var: &str| match var {
            CONVERSATION_VAR => Some(OsString::from(id)),
            _ => good_env(var),
        }
    };
    let r = f.build_with(f.path("cq"), f.system(), &with("another-conversation"));
    assert_eq!(code(r), exit::USAGE);
    assert!(
        f.build_with(f.path("cq"), f.system(), &with("conv-1"))
            .is_ok()
    );
}

// --- Policy -------------------------------------------------------------------

#[test]
fn the_policy_table_is_strict() {
    let p = Policy::parse(POLICY, NAME).unwrap();
    assert_eq!(p.sandbox_sids, [SID]);
    assert_eq!(p.conversation.as_deref(), Some("conv-1"));
    let none = Policy::parse(&POLICY.replace("conversation = \"conv-1\"", ""), NAME).unwrap();
    assert_eq!(none.conversation, None);
    let bad = [
        (
            "an unknown key",
            POLICY.replace("reply_minutes", "reply_mins"),
        ),
        (
            "an empty conversation",
            POLICY.replace("\"conv-1\"", "\"\""),
        ),
        ("empty allow_from", POLICY.replace("[\"lead\"]", "[]")),
        ("a bad sender", POLICY.replace("\"lead\"", "\"../x\"")),
        (
            "two SIDs",
            POLICY.replace(&format!("\"{SID}\""), &format!("\"{SID}\", \"{SID}\"")),
        ),
        ("a bad SID", POLICY.replace(SID, "S-1-x")),
        ("zero minutes", POLICY.replace("= 30", "= 0")),
        ("a string for minutes", POLICY.replace("= 30", "= \"30\"")),
        (
            "a two-line conversation",
            POLICY.replace("\"conv-1\"", "\"a\\nb\""),
        ),
    ];
    for (what, text) in bad {
        let e = Policy::parse(&text, NAME).expect_err(what);
        assert_eq!(e.code, exit::USAGE, "{what}");
    }
    assert_eq!(
        Policy::parse(POLICY, "other").unwrap_err().code,
        exit::USAGE
    );
}

// --- Windows itself -------------------------------------------------------------

#[test]
fn the_os_finds_this_process_and_its_listener() {
    let me = Os.open(std::process::id()).unwrap();
    assert!(me.alive());
    assert!(me.created() > 0);
    assert!(me.image().unwrap().extension().is_some_and(|e| e == "exe"));
    let entries = Os.processes().unwrap();
    assert!(entries.iter().any(|e| e.pid == std::process::id()));

    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = l.local_addr().unwrap();
    let listeners = Os.listeners().unwrap();
    assert!(listeners.contains(&(at, std::process::id())), "{at}");
    let a = Address::parse(&format!("127.0.0.1:{}", at.port())).unwrap();
    assert_eq!(a.problem(&listeners, std::process::id()), None);
    assert!(a.problem(&listeners, std::process::id() + 1).is_some());
}

#[test]
fn the_link_sees_the_listener_and_the_process() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let mut link = SessionLink {
        process: Os.open(std::process::id()).unwrap(),
        pid: std::process::id(),
        address: Address::parse(&format!("127.0.0.1:{port}")).unwrap(),
        system: Box::new(Os),
    };
    assert!(link.alive() && link.reachable() && link.owned());
    drop(l);
    assert!(!link.reachable());
    assert!(!link.owned());
}
