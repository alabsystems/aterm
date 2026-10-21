// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Does the lab's jail hold? (`probe [tag] [--replica]`; the selftests run
//! `--replica`, which never touches the owner's tree.)
//!
//! For each WORLD the same checks run twice: outside any sandbox (the control:
//! is the thing reachable at all?) and inside `sandbox-exec -f <the world's
//! profile> aterm-lab probe-inside …`, and the two are compared. A denial
//! counts only where the control got through: a thing this machine does not
//! have, or one that is unreachable outside the jail too (no privacy consent,
//! say), proves nothing about the jail and is reported as such.
//!
//! * this lab's `jail.sb`, against THIS machine: the owner's real paths and
//!   live sockets (the lab's own profile when it is up, a fresh render of the
//!   same rules otherwise);
//! * a replica home: the same rules rendered for a scratch home that holds
//!   every entry of the protected list, a socket where the owner's live aterm
//!   and Claude Code keep theirs, the socket of a second private instance, the
//!   agents in a replica of aterm's package store (a Codex build with its
//!   package file, and another build beside it), the scratch's own
//!   directories and a replica of the user temp dir's xcrun cache, so every
//!   rule is proven whatever data this machine has. Its run, state and temp
//!   dirs sit in the lab's state dir (only their own rules deny them there; a
//!   `/tmp/labprobe.*` path is denied to every jail as a whole), its sockets in
//!   `/tmp` (socket names are limited to 103 bytes). No jail writes either
//!   place, so nothing a jail planted is followed by the probe's unjailed
//!   controls or by the files it builds.
//!
//! Nothing leaves the machine and nothing of the owner's is read: the owner's
//! real tree gets `stat`, `access` and an `open` without a read, the live
//! sockets get a connect only INSIDE the jail (the control only looks at
//! them), and the "external" connect is a UDP `connect()`, which sends no
//! packet. A replica file is read for real. To see the probes still bite after
//! a change to the profile, delete a line of a rendered profile and probe
//! again: it must FAIL (do that with a scratch HOME and socket directory, so a
//! mutant that opens a socket cannot reach the owner's live aterm).

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::Stdio;

use crate::paths::paths;
use crate::protected::{CCSOCKS, Kind, MACH_DENY, MACH_DENY_OPEN, SCRATCH_DIRS, protected_paths};
use crate::run::{Run, is_run, take_tag};
use crate::sys::io_errname;
use crate::{Fail, Res};

/// What a check wants inside the jail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// The jail refuses it.
    Denied,
    /// The jail lets it through.
    Allowed,
}

/// One operation, run outside and inside the jail.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// `stat`, then a read (replica) or an open without a read (the owner's tree).
    Reach(String, bool),
    /// Create (exclusively, then remove), open for appending, or replace by a rename.
    Write(String, WriteHow),
    /// Connect to a unix socket; a LIVE one is only looked at outside the jail.
    Unix(String, bool),
    /// A UDP connect to TEST-NET-1.
    Udp,
    /// Bind, listen, connect and accept: in a directory (unix) or on loopback.
    Serve(Option<String>),
    /// `bootstrap_look_up` of a Mach service.
    Mach(String),
    /// A kernel policy query only: never looks up a service or reads preferences.
    PreferencePolicy(String, String),
    /// Open and map a parent-owned named shared-memory canary read-only.
    #[cfg(test)]
    ShmRead(String),
    /// `kill(…, 0)` of the parent, self, a child or a grandchild.
    Signal(String),
    /// Start `/usr/bin/security`, `/usr/bin/open`, `/bin/ps`, or the tripwire
    /// or the open recorder by their bare names.
    Exec(String, String),
    /// Read and run the Claude Code binary.
    Claude(String),
    /// `TIOCSTI` on a terminal of its own.
    Tiocsti,
    /// Remove (unlink, rename or rmdir) a thing the unjailed lab trusts, put back at once.
    Remove(String, RemoveHow),
    /// A directory's metadata only (`stat`), or its listing.
    Meta(String, bool),
}

/// How [`Op::Write`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteHow {
    /// Create exclusively, then remove.
    Create,
    /// Open for appending; nothing is appended.
    Append,
    /// A file of its own renamed over it, the way a writer replaces a file (replica only).
    Replace,
}

/// How [`Op::Remove`] removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveHow {
    /// A hard link first, then unlink and link back (the live socket stays bound through its inode).
    Unlink,
    /// Rename away and back.
    Rename,
    /// Remove an empty directory and make it again.
    Rmdir,
}

impl WriteHow {
    fn word(self) -> &'static str {
        match self {
            WriteHow::Create => "create",
            WriteHow::Append => "append",
            WriteHow::Replace => "replace",
        }
    }

    fn from_word(w: &str) -> Option<WriteHow> {
        Some(match w {
            "create" => WriteHow::Create,
            "append" => WriteHow::Append,
            "replace" => WriteHow::Replace,
            _ => return None,
        })
    }
}

impl RemoveHow {
    fn word(self) -> &'static str {
        match self {
            RemoveHow::Unlink => "unlink",
            RemoveHow::Rename => "rename",
            RemoveHow::Rmdir => "rmdir",
        }
    }

    fn from_word(w: &str) -> Option<RemoveHow> {
        Some(match w {
            "unlink" => RemoveHow::Unlink,
            "rename" => RemoveHow::Rename,
            "rmdir" => RemoveHow::Rmdir,
            _ => return None,
        })
    }
}

impl Op {
    pub(crate) fn encode(&self) -> String {
        let b = |v: bool| if v { "1" } else { "0" };
        match self {
            Op::Reach(p, r) => format!("reach\t{p}\t{}", b(*r)),
            Op::Write(p, h) => format!("write\t{p}\t{}", h.word()),
            Op::Unix(p, l) => format!("unix\t{p}\t{}", b(*l)),
            Op::Udp => "udp".into(),
            Op::Serve(d) => format!("serve\t{}", d.as_deref().unwrap_or("")),
            Op::Mach(n) => format!("mach\t{n}"),
            Op::PreferencePolicy(scope, n) => format!("preference-policy\t{scope}\t{n}"),
            #[cfg(test)]
            Op::ShmRead(n) => format!("shm-read\t{n}"),
            Op::Signal(w) => format!("signal\t{w}"),
            Op::Exec(w, s) => format!("exec\t{w}\t{s}"),
            Op::Claude(p) => format!("claude\t{p}"),
            Op::Tiocsti => "tiocsti".into(),
            Op::Remove(p, h) => format!("remove\t{p}\t{}", h.word()),
            Op::Meta(p, l) => format!("meta\t{p}\t{}", b(*l)),
        }
    }

    fn decode(line: &str) -> Option<Op> {
        let f: Vec<&str> = line.split('\t').collect();
        let s = |i: usize| f.get(i).map(|v| (*v).to_string());
        let b = |i: usize| f.get(i) == Some(&"1");
        Some(match *f.first()? {
            "reach" => Op::Reach(s(1)?, b(2)),
            "write" => Op::Write(s(1)?, WriteHow::from_word(f.get(2)?)?),
            "unix" => Op::Unix(s(1)?, b(2)),
            "udp" => Op::Udp,
            "serve" => Op::Serve(s(1).filter(|d| !d.is_empty())),
            "mach" => Op::Mach(s(1)?),
            "preference-policy" => Op::PreferencePolicy(s(1)?, s(2)?),
            #[cfg(test)]
            "shm-read" => Op::ShmRead(s(1)?),
            "signal" => Op::Signal(s(1)?),
            "exec" => Op::Exec(s(1)?, s(2)?),
            "claude" => Op::Claude(s(1)?),
            "tiocsti" => Op::Tiocsti,
            "remove" => Op::Remove(s(1)?, RemoveHow::from_word(f.get(2)?)?),
            "meta" => Op::Meta(s(1)?, b(2)),
            _ => return None,
        })
    }

    /// The outcome: `yes`, or the errno of the refusal.
    fn perform(&self, jailed: bool) -> String {
        let name = |e: std::io::Error| io_errname(&e);
        match self {
            Op::Reach(p, read) => {
                let r = fs::metadata(p).and_then(|m| {
                    if !read {
                        return fs::File::open(p).map(drop);
                    }
                    if m.is_dir() {
                        fs::read_dir(p).map(drop)
                    } else {
                        fs::File::open(p).and_then(|mut f| f.read(&mut [0u8; 1]).map(drop))
                    }
                });
                r.map_or_else(name, |()| "yes".into())
            }
            Op::Write(p, how) => {
                use std::os::unix::fs::OpenOptionsExt as _;
                let create = |path: &str| {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)
                        .map(drop)
                };
                let r = match how {
                    WriteHow::Create => create(p).and_then(|()| fs::remove_file(p)),
                    WriteHow::Append => fs::OpenOptions::new().append(true).open(p).map(drop),
                    WriteHow::Replace => {
                        let tmp = format!("{p}.probe-{}", std::process::id());
                        create(&tmp).and_then(|()| {
                            fs::rename(&tmp, p).inspect_err(|_| {
                                let _ = fs::remove_file(&tmp);
                            })
                        })
                    }
                };
                r.map_or_else(name, |()| "yes".into())
            }
            Op::Unix(p, live) => {
                if *live && !jailed {
                    use std::os::unix::fs::FileTypeExt as _;
                    return match fs::metadata(p) {
                        Ok(m) if m.file_type().is_socket() => "yes".into(),
                        Ok(_) => "ENOTSOCK".into(),
                        Err(e) => name(e),
                    };
                }
                UnixStream::connect(p).map_or_else(name, |_| "yes".into())
            }
            Op::Udp => {
                crate::sys::udp_connect([192, 0, 2, 1], 9).map_or_else(name, |()| "yes".into())
            }
            Op::Serve(dir) => serve(dir.as_deref()).map_or_else(name, |()| "yes".into()),
            Op::Mach(n) => match crate::sys::mach_look_up(n) {
                0 => "yes".into(),
                1100 => "EPERM".into(),
                1102 => "ENOENT".into(),
                kr => format!("kr={kr}"),
            },
            Op::PreferencePolicy(scope, n) => crate::sys::preference_policy(scope, n)
                .map_or_else(name, |allowed| if allowed { "yes" } else { "EPERM" }.into()),
            #[cfg(test)]
            Op::ShmRead(n) => crate::sys::read_named_shm(n).map_or_else(name, |()| "yes".into()),
            Op::Signal(who) => signal(who).map_or_else(name, |()| "yes".into()),
            Op::Exec(what, shim) => exec(what, shim, jailed),
            Op::Claude(p) => match fs::File::open(p).and_then(|mut f| f.read(&mut [0u8; 4])) {
                Ok(_) if crate::sys::executable(p) => "yes".into(),
                Ok(_) => "EACCES".into(),
                Err(e) => name(e),
            },
            Op::Tiocsti => tiocsti(),
            Op::Remove(p, how) => remove(p, *how),
            Op::Meta(p, list) => {
                let r = if *list {
                    fs::read_dir(p).map(drop)
                } else {
                    fs::metadata(p).map(drop)
                };
                r.map_or_else(name, |()| "yes".into())
            }
        }
    }
}

fn serve(dir: Option<&str>) -> std::io::Result<()> {
    if let Some(d) = dir {
        let path = Path::new(d).join(format!("probe-{}.sock", std::process::id()));
        let r = (|| {
            let l = UnixListener::bind(&path)?;
            let _c = UnixStream::connect(&path)?;
            l.accept().map(drop)
        })();
        let _ = fs::remove_file(&path);
        r
    } else {
        let l = std::net::TcpListener::bind("127.0.0.1:0")?;
        let _c = std::net::TcpStream::connect(l.local_addr()?)?;
        l.accept().map(drop)
    }
}

fn signal(who: &str) -> std::io::Result<()> {
    match who {
        "parent" => crate::sys::signal_probe(crate::sys::getppid(), 0),
        "self" => crate::sys::signal_probe(i32::try_from(std::process::id()).unwrap_or(0), 0),
        _ => {
            let mut cmd = if who == "child" {
                aterm_uds::spawnfd::command("/bin/sleep")
            } else {
                aterm_uds::spawnfd::command("/bin/sh")
            };
            if who == "child" {
                cmd.arg("20");
            } else {
                cmd.args(["-c", "/bin/sleep 20 & echo $!; wait"])
                    .stdout(Stdio::piped());
            }
            let mut child = cmd.spawn()?;
            let pid = if who == "child" {
                i32::try_from(child.id()).unwrap_or(0)
            } else {
                let mut line = String::new();
                let mut b = [0u8; 1];
                let out = child
                    .stdout
                    .as_mut()
                    .ok_or_else(|| std::io::Error::other("no stdout"))?;
                while out.read(&mut b)? == 1 && b[0] != b'\n' {
                    line.push(char::from(b[0]));
                }
                line.trim()
                    .parse()
                    .map_err(|_| std::io::Error::other("no pid"))?
            };
            let r = crate::sys::signal_probe(pid, 0)
                .and_then(|()| crate::sys::signal_probe(pid, libc::SIGTERM));
            let _ = child.kill();
            let _ = child.wait();
            r
        }
    }
}

fn exec(what: &str, shim: &str, jailed: bool) -> String {
    let rc = |r: std::io::Result<std::process::Output>,
              ok: &dyn Fn(&std::process::Output) -> bool| match r {
        Ok(o) if ok(&o) => "yes".to_string(),
        Ok(o) => format!("rc={}", o.status.code().unwrap_or(-1)),
        Err(e) => io_errname(&e),
    };
    match what {
        // the keychain CLI: inside the jail it cannot start
        "security" if !jailed => {
            if crate::sys::executable("/usr/bin/security") {
                "yes".into()
            } else {
                "ENOENT".into()
            }
        }
        "security" => rc(
            aterm_uds::spawnfd::command("/usr/bin/security")
                .arg("-h")
                .output(),
            &|_| true,
        ),
        // the desktop's opener: inside the jail it cannot start (`-h` opens nothing)
        "open" if !jailed => {
            if crate::sys::executable("/usr/bin/open") {
                "yes".into()
            } else {
                "ENOENT".into()
            }
        }
        "open" => rc(
            aterm_uds::spawnfd::command("/usr/bin/open")
                .arg("-h")
                .output(),
            &|_| true,
        ),
        // the lab's stand-in for open and BROWSER: answers 0, opens nothing
        "recorder" => rc(
            aterm_uds::spawnfd::command(format!("{shim}/open"))
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LAB_OPEN_LOG", "/dev/null")
                .arg("x-lab-probe://none")
                .output(),
            &|o| o.status.success(),
        ),
        // the setuid ps the harness reads process facts through
        "ps" => {
            let me = std::process::id().to_string();
            rc(
                aterm_uds::spawnfd::command("/bin/ps")
                    .args(["-p", &me, "-o", "pid="])
                    .output(),
                &|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&me),
            )
        }
        // a bare-name call lands on the tripwire, which answers 44
        _ => rc(
            aterm_uds::spawnfd::command("security")
                .env_clear()
                .env("PATH", format!("{shim}:/usr/bin:/bin"))
                .env("LAB_SECURITY_LOG", "/dev/null")
                .args(["find-generic-password", "-a", "x", "-w", "-s", "lab-probe"])
                .output(),
            &|o| o.status.code() == Some(44),
        ),
    }
}

/// `TIOCSTI` puts a byte into a terminal's input queue as if typed. A child in
/// a session of its own takes a PTY it owns as its controlling terminal (the
/// one terminal `TIOCSTI` is allowed on) and tries; inside the jail it must be refused.
fn tiocsti() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "no exe".into();
    };
    match aterm_uds::spawnfd::command(exe)
        .arg("probe-tiocsti")
        .setsid(true)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { "no answer".into() } else { s }
        }
        Err(e) => io_errname(&e),
    }
}

/// `aterm-lab probe-tiocsti`: the child half of [`tiocsti`].
#[must_use]
pub fn tiocsti_child() -> i32 {
    let res = match aterm_pty::open_pty_pair() {
        Ok((_master, slave)) => crate::sys::take_controlling_tty(slave)
            .and_then(|()| crate::sys::tiocsti(slave, b'x'))
            .map_or_else(|e| io_errname(&e), |()| "yes".into()),
        Err(e) => io_errname(&e),
    };
    println!("{res}");
    0
}

/// Remove or move something the unjailed lab trusts (the control socket, its
/// token, their directory, the scratch's directories), harmlessly: what is
/// removed is put back at once. A hard link gives it a second name first, so
/// even the live control socket (bound through its inode) is back under its
/// own name; a removed directory is made again.
fn remove(path: &str, how: RemoveHow) -> String {
    let keep = format!("{path}.probe-keep");
    let r = (|| -> std::io::Result<Option<String>> {
        if how == RemoveHow::Rmdir {
            if let Err(e) = fs::remove_dir(path) {
                return Ok(Some(io_errname(&e)));
            }
            fs::create_dir(path)?;
        } else if how == RemoveHow::Unlink {
            fs::hard_link(path, &keep)?;
            if let Err(e) = fs::remove_file(path) {
                fs::remove_file(&keep)?;
                return Ok(Some(io_errname(&e)));
            }
            fs::hard_link(&keep, path)?;
            fs::remove_file(&keep)?;
        } else {
            if let Err(e) = fs::rename(path, &keep) {
                return Ok(Some(io_errname(&e)));
            }
            fs::rename(&keep, path)?;
        }
        Ok(None)
    })();
    match r {
        Ok(None) => "yes".into(),
        Ok(Some(e)) => e,
        Err(e) => io_errname(&e),
    }
}

/// One check of a world.
#[derive(Clone, Debug)]
pub struct Check {
    /// What it tries, for the report.
    pub label: String,
    /// What it wants inside the jail.
    pub want: Want,
    /// The operation.
    pub op: Op,
    /// Whether the control run outside the jail is meaningful and harmless.
    pub control: bool,
    /// Whether a thing that is not there is no failure.
    pub absent_ok: bool,
}

/// A world the checks run against.
#[derive(Clone, Debug)]
pub struct World {
    /// Its name in the report.
    pub name: String,
    /// The replica home (every protected path exists).
    pub replica: bool,
    /// Its home.
    pub home: String,
    /// The caller's separate HOME (a second owned home in the replica).
    pub caller_home: String,
    /// Its Claude socket directory.
    pub cc: String,
    /// Its run directory.
    pub run: String,
    /// Its socket directory.
    pub sockdir: String,
    /// Its profile.
    pub jail: PathBuf,
    /// The Claude Code binary it allows.
    pub claude: String,
    /// The Codex binary it allows (empty: none).
    pub codex: String,
    /// The tripwire's and the open recorder's directory.
    pub shim: String,
    /// The lab's state dir (its `runs/` no jailed write may reach).
    pub state: String,
    /// The lab's own program and source directories.
    pub lab_dirs: Vec<String>,
    /// The aterm the lab's door runs (empty: none).
    pub aterm: String,
    /// The user temp dir whose xcrun cache no jailed process may write.
    pub usertmp: String,
    /// The checkout's `target/debug` and `target/release` (physical), where a later `up` may pick an aterm.
    pub build_dirs: Vec<String>,
}

/// Another build of the Codex's in the replica's store: denied.
fn sibling_build(w: &World) -> String {
    let store = Path::new(&w.codex)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    store.join("0/bin/codex").to_string_lossy().into_owned()
}

/// Owned files outside the old protected list, including preferences and
/// containers that resolve through account identity rather than HOME.
fn home_canaries(w: &World) -> Vec<String> {
    [&w.home, &w.caller_home]
        .into_iter()
        .filter(|h| !h.is_empty())
        .flat_map(|h| {
            [
                "plain-canary",
                "Library/Preferences/lab-canary.plist",
                "Library/Containers/lab-canary/data",
                "Library/Group Containers/lab-canary/data",
                "Developer/unselected/canary",
            ]
            .map(|n| format!("{h}/{n}"))
        })
        .collect()
}

/// The checks of one world; `decoy` is the socket of another private
/// instance, `other_dirs` another lab's socket dir and another probe's scratch in `/tmp`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn plan(w: &World, decoy: &str, other_dirs: &[String]) -> Vec<Check> {
    let mut p: Vec<Check> = Vec::new();
    let short = |path: &str| match path.strip_prefix(&w.home) {
        Some(rest) => format!("{}{rest}", if w.replica { "<replica>" } else { "~" }),
        None => path.to_string(),
    };
    // the group names the rule a check proves (data, sockets, terminal, network, signals, keychain, writes)
    let mut add = |_group: &'static str,
                   label: String,
                   want: Want,
                   op: Op,
                   control: bool,
                   absent_ok: bool| {
        p.push(Check {
            label,
            want,
            op,
            control,
            absent_ok,
        });
    };
    use Want::{Allowed, Denied};
    // -- the owner's data: neither readable nor writable
    for (kind, path) in protected_paths(&w.home, &w.cc) {
        if w.replica {
            let here: Vec<String> = match kind {
                Kind::Subpath => vec![
                    path.clone(),
                    format!("{path}/sub"),
                    format!("{path}/secret"),
                ],
                Kind::Prefix => vec![path.clone(), format!("{path}.bak")],
                Kind::Literal => vec![path.clone()],
            };
            for h in here {
                add(
                    "data",
                    short(&h),
                    Denied,
                    Op::Reach(h.clone(), true),
                    true,
                    false,
                );
            }
            if kind == Kind::Subpath {
                add(
                    "data",
                    format!("{}/ (create)", short(&path)),
                    Denied,
                    Op::Write(format!("{path}/lab-probe-new"), WriteHow::Create),
                    false,
                    false,
                );
            } else {
                add(
                    "data",
                    format!("{} (append)", short(&path)),
                    Denied,
                    Op::Write(path.clone(), WriteHow::Append),
                    false,
                    false,
                );
                if kind == Kind::Prefix {
                    add(
                        "data",
                        format!("{}.new (create)", short(&path)),
                        Denied,
                        Op::Write(format!("{path}.new"), WriteHow::Create),
                        false,
                        false,
                    );
                }
            }
        } else {
            let mut here = vec![path.clone()];
            if kind == Kind::Prefix {
                here.extend(prefixed_siblings(&path).into_iter().take(3));
            }
            for h in here {
                add(
                    "data",
                    short(&h),
                    Denied,
                    Op::Reach(h.clone(), false),
                    true,
                    true,
                );
            }
        }
    }
    if w.replica {
        for path in home_canaries(w) {
            add(
                "data",
                short(&path),
                Denied,
                Op::Reach(path, true),
                true,
                false,
            );
        }
        for path in [
            format!("{}/plain-canary", w.caller_home),
            format!("{}/plain-canary", w.home),
        ] {
            add(
                "writes",
                short(&path),
                Denied,
                Op::Write(path, WriteHow::Append),
                true,
                false,
            );
        }
        add(
            "allowed",
            "read the selected source inside the account home".into(),
            Allowed,
            Op::Reach(format!("{}/Developer/selected-source/canary", w.home), true),
            true,
            false,
        );
    }
    // Query the sandbox instead of touching the owner's actual preferences
    // daemons/domains. The allow-default control must admit each operation.
    for (scope, name) in [
        ("global", "com.apple.cfprefsd.daemon"),
        ("global", "com.apple.cfprefsd.agent"),
        ("local", "com.apple.cfprefsd.agent"),
        ("read", ""),
    ] {
        add(
            "preferences",
            format!("preferences policy {scope} {name}"),
            Denied,
            Op::PreferencePolicy(scope.into(), name.into()),
            true,
            false,
        );
    }
    add(
        "allowed",
        "an unrelated Mach policy remains available (query only)".into(),
        Allowed,
        Op::PreferencePolicy("global".into(), "com.apple.TrustEvaluationAgent".into()),
        true,
        false,
    );
    // -- unix sockets: only the lab's own socket dir
    add(
        "sockets",
        format!("connect to the socket of another private instance ({decoy})"),
        Denied,
        Op::Unix(decoy.into(), false),
        true,
        false,
    );
    add(
        "sockets",
        "connect to mDNSResponder (no name resolution)".into(),
        Denied,
        Op::Unix("/var/run/mDNSResponder".into(), false),
        true,
        true,
    );
    let mut live = vec![(
        format!("{}/Library/Application Support/aterm/aterm.sock", w.home),
        "the owner's live aterm socket",
    )];
    if w.replica {
        live.push((
            format!("{}/probe.sock", w.cc),
            "a Claude Code messaging socket",
        ));
    } else if let Some(s) = sorted_glob(&w.cc, ".sock").into_iter().next() {
        live.push((s, "a Claude Code messaging socket"));
    }
    for (path, what) in live {
        add(
            "sockets",
            format!("connect to {what} ({})", short(&path)),
            Denied,
            Op::Unix(path.clone(), true),
            true,
            !w.replica,
        );
    }
    add(
        "sockets",
        "bind, listen, connect and accept in the lab's own socket dir".into(),
        Allowed,
        Op::Serve(Some(w.sockdir.clone())),
        true,
        false,
    );
    // -- the control socket the unjailed client trusts: no removing, no moving, no planting a link in its place
    // (in the lab's own world these touch the LIVE socket, put back at once: the control run is skipped there,
    // so the only attempt is the one inside the jail, and the replica's decoys get both)
    for n in ["a.sock", "a.sock.token"] {
        for how in [RemoveHow::Unlink, RemoveHow::Rename] {
            add(
                "sockets",
                format!(
                    "{} {n} in the lab's socket dir (the client dereferences it)",
                    how.word()
                ),
                Denied,
                Op::Remove(format!("{}/{n}", w.sockdir), how),
                w.replica,
                false,
            );
        }
    }
    add(
        "sockets",
        "move the lab's socket dir".into(),
        Denied,
        Op::Remove(w.sockdir.clone(), RemoveHow::Rename),
        w.replica,
        false,
    );
    // -- the scratch's own directories and the project dir, which the lab writes and reads through from outside the
    // jail: no removing or moving one (a link planted in its place would lead the lab into the owner's data); in the
    // lab's own world these are the live ones, put back at once, and the control run is skipped, as for the socket
    let scratch = format!("{}/scratch", w.run);
    for n in std::iter::once("").chain(SCRATCH_DIRS.iter().copied()) {
        let (what, path) = if n.is_empty() {
            ("the scratch".to_string(), scratch.clone())
        } else {
            (format!("scratch/{n}"), format!("{scratch}/{n}"))
        };
        add(
            "scratch",
            format!("move {what} (the lab writes and reads through it)"),
            Denied,
            Op::Remove(path, RemoveHow::Rename),
            w.replica,
            false,
        );
    }
    add(
        "scratch",
        "move the project dir".into(),
        Denied,
        Op::Remove(format!("{}/proj", w.sockdir), RemoveHow::Rename),
        w.replica,
        false,
    );
    add(
        "scratch",
        "remove scratch/codex-home".into(),
        Denied,
        Op::Remove(format!("{scratch}/codex-home"), RemoveHow::Rmdir),
        w.replica,
        false,
    );
    // -- the terminal: no keystroke into any terminal's input queue
    add(
        "terminal",
        "TIOCSTI on a terminal of its own (puts a byte into the input queue as if typed)".into(),
        Denied,
        Op::Tiocsti,
        true,
        false,
    );
    // -- network: loopback only
    add(
        "network",
        "UDP connect to 192.0.2.1:9 (TEST-NET-1, not loopback; a connect sends no packet)".into(),
        Denied,
        Op::Udp,
        true,
        false,
    );
    add(
        "network",
        "bind, listen, connect and accept on loopback".into(),
        Allowed,
        Op::Serve(None),
        true,
        false,
    );
    // -- signals: only the lab's own
    add(
        "signals",
        "signal the process that started the jail (an unjailed process of the same uid)".into(),
        Denied,
        Op::Signal("parent".into()),
        true,
        false,
    );
    for who in ["self", "child", "grandchild"] {
        add(
            "signals",
            format!("signal its own {who}"),
            Allowed,
            Op::Signal(who.into()),
            true,
            false,
        );
    }
    // -- keychain: the sandbox answers before it looks the name up, so a denial is exact either way
    for n in MACH_DENY {
        add(
            "keychain",
            format!("look up the Mach service {n}"),
            Denied,
            Op::Mach((*n).into()),
            false,
            false,
        );
    }
    add(
        "keychain",
        "start /usr/bin/security".into(),
        Denied,
        Op::Exec("security".into(), w.shim.clone()),
        true,
        false,
    );
    add(
        "keychain",
        "a bare-name security call lands on the tripwire (answers 44)".into(),
        Allowed,
        Op::Exec("tripwire".into(), w.shim.clone()),
        true,
        false,
    );
    // -- the desktop: nothing opens a URL or an app there (a sign-in's browser): Launch Services' open doors, /usr/bin/open
    for n in MACH_DENY_OPEN {
        add(
            "desktop",
            format!("look up the Mach service {n}"),
            Denied,
            Op::Mach((*n).into()),
            false,
            false,
        );
    }
    add(
        "desktop",
        "start /usr/bin/open".into(),
        Denied,
        Op::Exec("open".into(), w.shim.clone()),
        true,
        false,
    );
    add(
        "desktop",
        "the open recorder answers 0 and opens nothing".into(),
        Allowed,
        Op::Exec("recorder".into(), w.shim.clone()),
        true,
        false,
    );
    // -- writes: nowhere in the owner's home or the run directory, but the scratch
    add(
        "writes",
        "create a file in the owner's home".into(),
        Denied,
        Op::Write(format!("{}/lab-write-probe", w.home), WriteHow::Create),
        false,
        false,
    );
    add(
        "writes",
        "create a file in the lab's run directory".into(),
        Denied,
        Op::Write(format!("{}/lab-write-probe", w.run), WriteHow::Create),
        false,
        false,
    );
    for f in ["env.sh", "jail.sb"] {
        add(
            "writes",
            format!("open {f} for appending (aterm-lab trusts it outside the jail)"),
            Denied,
            Op::Write(format!("{}/{f}", w.run), WriteHow::Append),
            false,
            false,
        );
    }
    // ... nor in another lab's socket dir or another probe's scratch in /tmp (the lab reads and writes them outside the jail)
    for d in other_dirs {
        let shape = Path::new(d)
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        add(
            "writes",
            format!("create a file in /tmp/{shape}.* (another lab's directory there)"),
            Denied,
            Op::Write(format!("{d}/lab-write-probe"), WriteHow::Create),
            true,
            false,
        );
    }
    // ... nor the xcrun shims' cache, which /usr/bin/python3, cc, git and the owner's shells trust (the owner's real one:
    // an open for appending, nothing appended, inside the jail only; the replica's is also replaced and moved, and its
    // temp dir moved)
    let db = format!("{}/xcrun_db", w.usertmp);
    add(
        "writes",
        format!(
            "open the xcrun shims' cache {} for appending (what python3, cc, git run is named there)",
            if w.replica {
                "<replica>/T/xcrun_db"
            } else {
                "<user temp dir>/xcrun_db"
            }
        ),
        Denied,
        Op::Write(db.clone(), WriteHow::Append),
        w.replica,
        !w.replica,
    );
    if w.replica {
        add(
            "writes",
            "replace the xcrun shims' cache by a rename".into(),
            Denied,
            Op::Write(db.clone(), WriteHow::Replace),
            true,
            false,
        );
        add(
            "writes",
            "move the xcrun shims' cache".into(),
            Denied,
            Op::Remove(db, RemoveHow::Rename),
            true,
            false,
        );
        add(
            "writes",
            "move the user temp dir that holds it".into(),
            Denied,
            Op::Remove(w.usertmp.clone(), RemoveHow::Rename),
            true,
            false,
        );
    }
    // ... nor in what the lab trusts from outside the jail, wherever it sits: the state dir (it reads runs/*/env.sh),
    // its own program and source, the aterm its door runs; and the state dir's parent stays put (the replica's:
    // moving the real one is no probe)
    add(
        "writes",
        "create a file in the lab's state dir, runs/ (aterm-lab reads each runs/*/env.sh)".into(),
        Denied,
        Op::Write(
            format!("{}/runs/lab-write-probe", w.state),
            WriteHow::Create,
        ),
        false,
        false,
    );
    for d in &w.lab_dirs {
        add(
            "writes",
            format!(
                "create a file in the lab's own program or source directory {} (it runs outside the jail)",
                short(d)
            ),
            Denied,
            Op::Write(format!("{d}/lab-write-probe"), WriteHow::Create),
            false,
            false,
        );
    }
    if !w.aterm.is_empty() {
        add(
            "writes",
            "open the aterm under test for appending (the lab's door runs it outside the jail)"
                .into(),
            Denied,
            Op::Write(w.aterm.clone(), WriteHow::Append),
            false,
            false,
        );
    }
    // ... nor the builds a later call may run in its place (this checkout's target/debug and target/release; on the
    // replica home, a checkout outside that home, where only their own rule denies it)
    for d in w.build_dirs.iter().filter(|d| Path::new(d).is_dir()) {
        add(
            "writes",
            format!(
                "create a file in the checkout's target/{} (a later aterm-lab call may run an aterm there, outside the jail)",
                Path::new(d)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            Denied,
            Op::Write(format!("{d}/lab-write-probe"), WriteHow::Create),
            false,
            false,
        );
    }
    if w.replica {
        add(
            "writes",
            "move the parent of the lab's state dir (a state dir put in its place would hand aterm-lab other env.sh files)".into(),
            Denied,
            Op::Remove(
                Path::new(&w.state)
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                RemoveHow::Rename,
            ),
            true,
            false,
        );
    }
    add(
        "writes",
        "create a file in the lab's scratch".into(),
        Allowed,
        Op::Write(format!("{}/scratch/probe-write", w.run), WriteHow::Create),
        true,
        false,
    );
    // -- what the lab keeps working
    add(
        "allowed",
        "run /bin/ps (the setuid ps, outside the sandbox)".into(),
        Allowed,
        Op::Exec("ps".into(), w.shim.clone()),
        true,
        false,
    );
    add(
        "allowed",
        "read and run the Claude Code binary".into(),
        Allowed,
        Op::Claude(w.claude.clone()),
        true,
        false,
    );
    if !w.codex.is_empty() {
        add(
            "allowed",
            "read and run the Codex binary".into(),
            Allowed,
            Op::Claude(w.codex.clone()),
            true,
            false,
        );
    }
    // -- the store above the Codex: its build is readable, the directories above it up to aterm's own give their
    // metadata only (Codex resolves its own path), no listing, and no other build is readable
    if w.replica && !w.codex.is_empty() {
        let build = Path::new(&w.codex)
            .parent()
            .and_then(Path::parent)
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_default();
        let top = format!("{}/Library/Application Support/aterm", w.home);
        add(
            "store",
            "read the Codex package beside its bin/ (codex-package.json)".into(),
            Allowed,
            Op::Reach(format!("{build}/codex-package.json"), true),
            true,
            false,
        );
        let mut d = Path::new(&build)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        while d == top || d.starts_with(&format!("{top}/")) {
            add(
                "store",
                format!("stat {} (metadata only)", short(&d)),
                Allowed,
                Op::Meta(d.clone(), false),
                true,
                false,
            );
            add(
                "store",
                format!("list {}", short(&d)),
                Denied,
                Op::Meta(d.clone(), true),
                true,
                false,
            );
            if d == top {
                break;
            }
            d = Path::new(&d)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        let other = sibling_build(w);
        add(
            "store",
            format!("read another build in the store ({})", short(&other)),
            Denied,
            Op::Reach(other, true),
            true,
            false,
        );
    }
    p
}

/// Files beside `path` whose names begin with its name and go on (`path?*`), sorted.
fn prefixed_siblings(path: &str) -> Vec<String> {
    let p = Path::new(path);
    let (Some(dir), Some(base)) = (
        p.parent(),
        p.file_name().map(|b| b.to_string_lossy().into_owned()),
    ) else {
        return Vec::new();
    };
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.len() > base.len() && n.starts_with(&base))
                .map(|n| dir.join(n).to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Files in `dir` ending in `suffix`, sorted.
fn sorted_glob(dir: &str, suffix: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(suffix))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// How one check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// It held.
    Ok,
    /// It did not.
    Fail,
    /// The thing is not on this machine (only the replica proves its rule).
    Absent,
    /// Refused inside, but unreachable outside too: proves nothing.
    Unproven,
}

/// Judge one check from its two outcomes.
#[must_use]
pub fn judge(
    replica: bool,
    c: &Check,
    inside: Option<&str>,
    control: Option<&str>,
) -> (Status, String) {
    let inside = inside.unwrap_or("none");
    if c.want == Want::Allowed {
        return if inside == "yes" {
            (Status::Ok, String::new())
        } else {
            (Status::Fail, format!("refused inside the jail: {inside}"))
        };
    }
    if inside == "yes" {
        return (Status::Fail, "reachable: the jail does not deny it".into());
    }
    if inside == "EPERM" {
        return match control {
            None | Some("yes") => (Status::Ok, String::new()),
            Some(c) if replica => (
                Status::Fail,
                format!("unreachable outside the jail too ({c}): the replica is broken"),
            ),
            Some(c) => (
                Status::Unproven,
                format!(
                    "refused, but outside the jail the same operation gives {c} too: no privacy consent for this shell?"
                ),
            ),
        };
    }
    if inside == "ENOENT" && c.absent_ok && !replica {
        return (Status::Absent, String::new());
    }
    (Status::Fail, format!("not a denial: {inside}"))
}

/// Every protected path of the replica as files this process owns, and what the other checks need.
fn build_replica(w: &World) -> std::io::Result<()> {
    for path in home_canaries(w).into_iter().chain(std::iter::once(format!(
        "{}/Developer/selected-source/canary",
        w.home
    ))) {
        fs::create_dir_all(Path::new(&path).parent().unwrap())?;
        fs::write(path, "owned replica\n")?;
    }
    for (kind, path) in protected_paths(&w.home, &w.cc) {
        let names = match kind {
            Kind::Subpath => {
                fs::create_dir_all(format!("{path}/sub"))?;
                vec![format!("{path}/secret")]
            }
            Kind::Prefix => vec![path.clone(), format!("{path}.bak")],
            Kind::Literal => vec![path.clone()],
        };
        for n in names {
            if let Some(d) = Path::new(&n).parent() {
                fs::create_dir_all(d)?;
            }
            fs::write(&n, "replica\n")?;
        }
    }
    // the agents under test in the store, a package file beside the Codex's bin/, and another build beside it
    use std::os::unix::fs::PermissionsExt as _;
    for b in [w.claude.clone(), w.codex.clone(), sibling_build(w)] {
        if let Some(d) = Path::new(&b).parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(&b, "#!/bin/sh\necho replica\n")?;
        fs::set_permissions(&b, fs::Permissions::from_mode(0o755))?;
    }
    if let Some(build) = Path::new(&w.codex).parent().and_then(Path::parent) {
        fs::write(
            build.join("codex-package.json"),
            "{\"version\": \"replica\"}\n",
        )?;
    }
    fs::write(format!("{}/env.sh", w.run), "# replica\n")?;
    // the xcrun shims' cache, as the system keeps it there
    fs::write(format!("{}/xcrun_db", w.usertmp), "replica\n")?;
    // empty, so that the control can remove each
    fs::create_dir_all(format!("{}/scratch", w.run))?;
    for n in SCRATCH_DIRS {
        fs::create_dir_all(format!("{}/scratch/{n}", w.run))?;
    }
    fs::create_dir_all(format!("{}/proj", w.sockdir))?;
    // what the lab's client trusts in a lab's socket directory
    for n in ["a.sock", "a.sock.token"] {
        fs::write(format!("{}/{n}", w.sockdir), "replica\n")?;
    }
    Ok(())
}

/// A listening unix socket that accepts and hangs up, until dropped.
struct Listener {
    path: PathBuf,
}

impl Listener {
    fn serve(path: &Path) -> std::io::Result<Listener> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        let l = UnixListener::bind(path)?;
        std::thread::spawn(move || {
            for c in l.incoming() {
                drop(c);
            }
        });
        Ok(Listener {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The checks of `w` inside its profile: one outcome per check.
fn run_inside(w: &World, checks: &[Check], spec_dir: &Path) -> Result<Vec<String>, String> {
    let spec = spec_dir.join("probe-spec.txt");
    let body: String = checks
        .iter()
        .map(|c| format!("{}\n", c.op.encode()))
        .collect();
    fs::write(&spec, body).map_err(|e| e.to_string())?;
    let out = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(&w.jail)
        .arg(&paths().exe)
        .arg("probe-inside")
        .arg(&spec)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&spec);
    let text = String::from_utf8_lossy(&out.stdout);
    let got: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("R\t").map(str::to_string))
        .collect();
    if got.len() != checks.len() {
        return Err(format!(
            "nothing came back from inside {} (rc {:?}): {}",
            w.jail.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(got)
}

/// `aterm-lab probe-inside <spec>`: the checks of a spec, jailed; one `R\t<outcome>` line each.
#[must_use]
pub fn inside_main(args: &[String]) -> i32 {
    let Some(text) = args.first().and_then(|p| fs::read_to_string(p).ok()) else {
        return 2;
    };
    let mut out = std::io::stdout().lock();
    for line in text.lines() {
        let res = Op::decode(line).map_or_else(|| "bad spec".into(), |op| op.perform(true));
        let _ = writeln!(out, "R\t{res}");
    }
    0
}

/// Judge every world and print the report; 0 PASS, 1 FAIL.
fn judge_worlds(worlds: &[World], base: &Path, o: &mut dyn std::io::Write) -> i32 {
    // the shape of another agent's private instance, and of another probe's scratch
    let decoy_dir = crate::sys::make_temp_dir("/tmp/at.XXXXXX")
        .unwrap_or_else(|_| base.join("decoy").to_string_lossy().into_owned());
    let other_probe = crate::sys::make_temp_dir("/tmp/labprobe.XXXXXX")
        .unwrap_or_else(|_| base.join("other").to_string_lossy().into_owned());
    let decoy = format!("{decoy_dir}/a.sock");
    let mut keep: Vec<Listener> = Listener::serve(Path::new(&decoy)).into_iter().collect();
    let (mut ok, mut fail, mut absent_n, mut unproven) = (0, 0, 0, 0);
    for w in worlds {
        if w.replica {
            if let Err(e) = build_replica(w) {
                let _ = writeln!(o, "  {}: the replica could not be built: {e}", w.name);
                fail += 1;
                continue;
            }
            for s in [
                format!("{}/Library/Application Support/aterm/aterm.sock", w.home),
                format!("{}/probe.sock", w.cc),
            ] {
                keep.extend(Listener::serve(Path::new(&s)));
            }
        }
        // a lab that is not up has no socket or token: stand-ins take their places
        let mut made = Vec::new();
        for n in ["a.sock", "a.sock.token"] {
            let path = format!("{}/{n}", w.sockdir);
            if !w.replica && fs::symlink_metadata(&path).is_err() && fs::write(&path, "").is_ok() {
                made.push(path);
            }
        }
        let checks = plan(w, &decoy, &[decoy_dir.clone(), other_probe.clone()]);
        let control: Vec<Option<String>> = checks
            .iter()
            .map(|c| c.control.then(|| c.op.perform(false)))
            .collect();
        let inside = run_inside(w, &checks, base);
        for p in made {
            let _ = fs::remove_file(p);
        }
        let _ = writeln!(o, "  {}:", w.name);
        let inside = match inside {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(o, "    FAIL  {e}");
                fail += 1;
                continue;
            }
        };
        let (mut absent, mut held) = (Vec::new(), 0);
        for (i, c) in checks.iter().enumerate() {
            let (st, why) = judge(
                w.replica,
                c,
                inside.get(i).map(String::as_str),
                control[i].as_deref(),
            );
            match st {
                Status::Ok => ok += 1,
                Status::Fail => fail += 1,
                Status::Absent => absent_n += 1,
                Status::Unproven => unproven += 1,
            }
            match st {
                Status::Absent => absent.push(c.label.clone()),
                Status::Ok if w.replica => held += 1,
                Status::Ok => {
                    let _ = writeln!(
                        o,
                        "    ok    {:<8} {}",
                        if c.want == Want::Denied {
                            "denied"
                        } else {
                            "allowed"
                        },
                        c.label
                    );
                }
                Status::Fail => {
                    let _ = writeln!(o, "    FAIL  {}  ({why})", c.label);
                }
                Status::Unproven => {
                    let _ = writeln!(o, "    ??    {}  ({why})", c.label);
                }
            }
        }
        if !absent.is_empty() {
            let _ = writeln!(
                o,
                "    --    not on this machine, so only the replica proves their rules: {}",
                absent.join(" ")
            );
        }
        if w.replica {
            let _ = writeln!(
                o,
                "    ok    {held} checks held: each protected path denied to stat, read and write, the sockets, the signals, \
                 the keychain, the desktop, the scratch's directories, the lab's state dir, the store"
            );
        }
    }
    drop(keep);
    let _ = fs::remove_dir_all(&decoy_dir);
    let _ = fs::remove_dir_all(&other_probe);
    let _ = writeln!(
        o,
        "isolation: {}  ({ok} held, {absent_n} absent on this machine, {unproven} unproven, {fail} failed)",
        if fail == 0 { "PASS" } else { "FAIL" }
    );
    i32::from(fail != 0)
}

/// `probe [tag] [--replica]`: the lab's own jail when it is up, else a fresh
/// render of the same profile, and the replica home; `--replica`: the replica
/// home alone (the same rules, Codex's among them), which never touches the
/// owner's tree.
pub fn cmd_probe(args: &[String]) -> Res {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    // an argument that is neither --replica nor a lab that is up is refused: read as a tag, it would leave a
    // --replica after it unseen and run the probe of this machine, which stats and opens the owner's protected paths
    let mut replica_only = false;
    for x in &a {
        if x == "--replica" {
            replica_only = true;
        } else {
            return Err(Fail::die(format!(
                "probe: '{x}' is neither --replica nor a lab that is up ({} ps)",
                crate::LAB
            )));
        }
    }
    probe(&tag, replica_only, &mut std::io::stdout())
}

/// The probe of lab `tag` with its report captured: (0 PASS / 1 FAIL, the report).
///
/// # Errors
/// The probe could not run.
pub fn run_captured(tag: &str, replica_only: bool) -> Result<(i32, String), Fail> {
    let mut buf: Vec<u8> = Vec::new();
    let rc = probe(tag, replica_only, &mut buf)?;
    Ok((rc, String::from_utf8_lossy(&buf).into_owned()))
}

fn probe(tag: &str, replica_only: bool, o: &mut dyn std::io::Write) -> Res {
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(Fail::die(
            "sandbox-exec is required (macOS): it is the lab's network/keychain jail",
        ));
    }
    let p = paths();
    // the replica's sockets (its home, its Claude sockets, its socket dir) need short paths, in /tmp: unix socket
    // names are limited to 103 bytes; its run, state and temp dirs go into the lab's state dir
    let base = PathBuf::from(
        crate::sys::make_temp_dir("/tmp/labprobe.XXXX")
            .map_err(|e| Fail::die(format!("cannot make a scratch dir in /tmp: {e}")))?,
    );
    let base = fs::canonicalize(&base).unwrap_or(base);
    let pst = fs::create_dir_all(p.state.join("runs"))
        .and_then(|()| {
            crate::sys::make_temp_dir(&format!("{}/probe.XXXX", p.state.display()))
                .map(PathBuf::from)
        })
        .and_then(fs::canonicalize);
    let pst = match pst {
        Ok(d) => d,
        Err(e) => {
            let _ = fs::remove_dir_all(&base);
            return Err(Fail::die(format!(
                "cannot make a scratch dir in {}: {e}",
                p.state.display()
            )));
        }
    };
    let result = probe_in(tag, replica_only, &base, &pst, p, o);
    let _ = fs::remove_dir_all(&base);
    let _ = fs::remove_dir_all(&pst);
    result
}

#[allow(clippy::too_many_lines)]
fn probe_in(
    tag: &str,
    replica_only: bool,
    base: &Path,
    pst: &Path,
    p: &crate::paths::Paths,
    o: &mut dyn std::io::Write,
) -> Res {
    let shim = base.join("shim");
    fs::create_dir_all(&shim).map_err(|e| Fail::die(e.to_string()))?;
    let _ = std::os::unix::fs::symlink(&p.exe, shim.join("security"));
    let _ = std::os::unix::fs::symlink(&p.exe, shim.join("open"));
    let shim = shim.to_string_lossy().into_owned();
    let lab_dirs = vec![
        p.exe_dir().to_string_lossy().into_owned(),
        p.crate_dir().to_string_lossy().into_owned(),
    ];
    let tg = crate::paths::physical(&p.repo.join("target"));
    let build_dirs: Vec<String> = ["debug", "release"]
        .iter()
        .map(|d| {
            crate::paths::physical(&tg.join(d))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let real_tmp = crate::sys::user_temp_dir()
        .map(|t| {
            crate::paths::physical(Path::new(t.trim_end_matches('/')))
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    let state = p.state.to_string_lossy().into_owned();
    let mut worlds: Vec<World> = Vec::new();
    if !replica_only {
        let lab_world = if is_run(tag) {
            let r = Run::load(tag)?;
            World {
                name: "this lab's jail.sb, against this machine".into(),
                replica: false,
                home: p.real_home.clone(),
                caller_home: p.caller_home.clone(),
                cc: CCSOCKS.into(),
                run: r.run.to_string_lossy().into_owned(),
                sockdir: r.sockdir.clone(),
                jail: r.run.join("jail.sb"),
                claude: r.claude_bin.clone(),
                codex: r.codex_bin.clone(),
                // the shim the lab's own agents run under, so its tripwire and recorder links are what is probed
                shim: r.run.join("shim").to_string_lossy().into_owned(),
                state: state.clone(),
                lab_dirs: lab_dirs.clone(),
                aterm: r.aterm.clone(),
                usertmp: real_tmp.clone(),
                build_dirs: build_dirs.clone(),
            }
        } else {
            p.need_aterm()?;
            let claude = crate::lab::find_claude()?;
            let codex = crate::lab::find_codex().unwrap_or_default();
            let run = base.join("run");
            let sock = base.join("sock");
            fs::create_dir_all(sock.join("proj")).map_err(|e| Fail::die(e.to_string()))?;
            for n in std::iter::once("").chain(SCRATCH_DIRS.iter().copied()) {
                fs::create_dir_all(run.join("scratch").join(n))
                    .map_err(|e| Fail::die(e.to_string()))?;
            }
            // the file the probe tries to append to
            let _ = fs::write(run.join("env.sh"), "");
            let mut spec = crate::jail::Spec::for_lab(
                &run.to_string_lossy(),
                &claude,
                &codex,
                &sock.to_string_lossy(),
                "",
            );
            spec.home.clone_from(&p.real_home);
            fs::write(run.join("jail.sb"), crate::jail::render(&spec)?)
                .map_err(|e| Fail::die(e.to_string()))?;
            World {
                name: "this lab's jail.sb, against this machine".into(),
                replica: false,
                home: p.real_home.clone(),
                caller_home: p.caller_home.clone(),
                cc: CCSOCKS.into(),
                run: run.to_string_lossy().into_owned(),
                sockdir: sock.to_string_lossy().into_owned(),
                jail: run.join("jail.sb"),
                claude,
                codex,
                shim: shim.clone(),
                state: state.clone(),
                lab_dirs: lab_dirs.clone(),
                aterm: spec.aterm.clone(),
                usertmp: real_tmp.clone(),
                build_dirs: build_dirs.clone(),
            }
        };
        worlds.push(lab_world);
    }
    let rep = base.join("replica");
    let rrun = pst.join("run");
    let store = rep.join("home/Library/Application Support/aterm/pkg/store");
    let rclaude = store.join("claude/1/bin/claude");
    let rcodex = store.join("codex/1000000000162000001/bin/codex");
    for d in [
        rrun.join("scratch"),
        rep.join("sock"),
        rep.join("home"),
        rep.join("caller-home"),
        rep.join("cc"),
        pst.join("lab/state/runs"),
        pst.join("T"),
    ] {
        fs::create_dir_all(&d)
            .map_err(|e| Fail::die(format!("cannot make {}: {e}", d.display())))?;
    }
    let st = |x: &Path| x.to_string_lossy().into_owned();
    let rs = |s: &str| st(&rep.join(s));
    let mut rspec =
        crate::jail::Spec::for_lab(&st(&rrun), &st(&rclaude), &st(&rcodex), &rs("sock"), "");
    rspec.home = rs("home");
    rspec.caller_home = rs("caller-home");
    rspec.lab_dirs.push(rs("home/Developer/selected-source"));
    rspec.ccsocks = rs("cc");
    rspec.state = st(&pst.join("lab/state"));
    rspec.user_tmp = st(&pst.join("T"));
    fs::write(rrun.join("jail.sb"), crate::jail::render(&rspec)?)
        .map_err(|e| Fail::die(e.to_string()))?;
    worlds.push(World {
        name: "the same rules on a replica home (every protected path, a socket for each, the store, the scratch)"
            .into(),
        replica: true,
        home: rs("home"),
        caller_home: rs("caller-home"),
        cc: rs("cc"),
        run: st(&rrun),
        sockdir: rs("sock"),
        jail: rrun.join("jail.sb"),
        claude: st(&rclaude),
        codex: st(&rcodex),
        shim,
        state: st(&pst.join("lab/state")),
        lab_dirs: rspec.lab_dirs.clone(),
        aterm: rspec.aterm.clone(),
        usertmp: st(&pst.join("T")),
        build_dirs,
    });
    for w in &worlds {
        if let Err(e) = crate::jail::check(&w.jail) {
            let _ = writeln!(o, "aterm-lab: {e}");
            return Ok(1);
        }
    }
    Ok(judge_worlds(&worlds, base, o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(replica: bool) -> World {
        World {
            name: "w".into(),
            replica,
            home: "/H".into(),
            caller_home: "/caller-H".into(),
            cc: "/C".into(),
            run: "/R".into(),
            sockdir: "/S".into(),
            jail: "/R/jail.sb".into(),
            claude: "/claude".into(),
            codex: "/H/Library/Application Support/aterm/pkg/store/codex/7/bin/codex".into(),
            shim: "/shim".into(),
            state: "/S/state".into(),
            lab_dirs: vec!["/L".into()],
            aterm: "/A/aterm".into(),
            usertmp: "/T".into(),
            build_dirs: Vec::new(),
        }
    }

    #[test]
    fn every_operation_crosses_into_the_jail_unchanged() {
        for c in plan(&world(true), "/tmp/at.x/a.sock", &["/tmp/at.y".into()]) {
            assert_eq!(
                Op::decode(&c.op.encode()).as_ref(),
                Some(&c.op),
                "{}",
                c.label
            );
        }
        let owned_shm = Op::ShmRead("/apple.cfprefs.owned-fixture".into());
        assert_eq!(Op::decode(&owned_shm.encode()), Some(owned_shm));
    }

    #[test]
    fn the_replica_plan_covers_every_protected_path_and_every_mach_service() {
        let p = plan(&world(true), "/d", &[]);
        let data = p
            .iter()
            .filter(|c| {
                matches!(c.op, Op::Reach(..) | Op::Write(..)) && c.label.starts_with("<replica>")
            })
            .count();
        assert!(data >= protected_paths("/H", "/C").len() * 2);
        assert_eq!(
            p.iter().filter(|c| matches!(c.op, Op::Mach(_))).count(),
            MACH_DENY.len() + MACH_DENY_OPEN.len()
        );
        assert!(p.iter().any(|c| c.label.starts_with("<replica>/.ssh")));
        // every scratch directory stays put, the store gives metadata only above the Codex's build
        for n in SCRATCH_DIRS {
            assert!(
                p.iter().any(|c| c.label
                    == format!("move scratch/{n} (the lab writes and reads through it)")),
                "{n}"
            );
        }
        assert!(p.iter().any(
            |c| c.label == "list <replica>/Library/Application Support/aterm"
                && c.want == Want::Denied
        ));
        assert!(p.iter().any(|c| c.label
            == "stat <replica>/Library/Application Support/aterm/pkg/store/codex (metadata only)"));
    }

    #[test]
    fn a_denial_counts_only_where_the_control_got_through() {
        let denied = Check {
            label: "l".into(),
            want: Want::Denied,
            op: Op::Udp,
            control: true,
            absent_ok: true,
        };
        assert_eq!(
            judge(false, &denied, Some("EPERM"), Some("yes")).0,
            Status::Ok
        );
        assert_eq!(judge(false, &denied, Some("EPERM"), None).0, Status::Ok);
        assert_eq!(
            judge(false, &denied, Some("EPERM"), Some("EPERM")).0,
            Status::Unproven
        );
        assert_eq!(
            judge(true, &denied, Some("EPERM"), Some("EACCES")).0,
            Status::Fail
        );
        assert_eq!(
            judge(false, &denied, Some("ENOENT"), Some("ENOENT")).0,
            Status::Absent
        );
        assert_eq!(
            judge(true, &denied, Some("ENOENT"), Some("ENOENT")).0,
            Status::Fail
        );
        assert_eq!(
            judge(false, &denied, Some("yes"), Some("yes")).0,
            Status::Fail
        );
        let allowed = Check {
            want: Want::Allowed,
            ..denied
        };
        assert_eq!(judge(false, &allowed, Some("yes"), None).0, Status::Ok);
        assert_eq!(judge(false, &allowed, Some("EPERM"), None).0, Status::Fail);
    }
}
