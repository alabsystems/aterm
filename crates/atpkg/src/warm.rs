// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The warm of a landed agent build.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::store::{Layout, ToolName};
use crate::vendor_direct::{VendorSpec, Warm};

/// `atpkg __warm <program> <prefix> <build>`: unlisted, answered on the raw argv before the verb match and the store lock, like the lane helpers — it never mutates the store.
pub const HIDDEN_VERB: &str = "__warm";

/// How long the warm leaves the program's stdin open before closing it (claude's fetches run while it waits for input; a closed stdin is what ends it).
pub const HOLD: Duration = Duration::from_secs(10);

/// When the warm's process group is killed, counted from its start.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// How often the warm looks at the program.
const POLL: Duration = Duration::from_millis(50);

/// Warm each of `programs` — the ones a vendor lane of this process just landed a build of — once, detached and silent ([`launches`] picks which).
pub(crate) fn spawn_landed(layout: &Layout, programs: &BTreeSet<String>) {
    if programs.is_empty() || !cfg!(unix) {
        return;
    }
    let admitted = admitted(
        crate::manager_enabled(),
        crate::cli::registry_seam().as_deref(),
        crate::packages_log::records(layout),
        running_as_root(),
    );
    let exe = if admitted { warm_executable() } else { None };
    for (program, build, argv) in launches(layout, programs, admitted, exe.as_deref()) {
        let Some((atpkg, args)) = argv.split_first() else {
            continue;
        };
        if crate::platform::spawn_detached(atpkg, args, None).is_err() {
            log(layout, program, build, "spawn-failed", 0);
        }
    }
}

/// The detached warms to start for `programs`: each a program, its build and the full
/// argv `<exe> __warm <program> <prefix> <build>` — none when the warm is not `admitted`
/// or there is no executable to run it, and never a (program, build) this process already
/// started ([`first_in_this_process`]).
fn launches(
    layout: &Layout,
    programs: &BTreeSet<String>,
    admitted: bool,
    exe: Option<&Path>,
) -> Vec<(&'static str, u64, Vec<OsString>)> {
    let Some(exe) = exe.filter(|_| admitted) else {
        return Vec::new();
    };
    plan(layout, programs)
        .into_iter()
        .filter(|(spec, build)| first_in_this_process(spec.program, *build))
        .map(|(spec, build)| {
            let mut argv = vec![exe.as_os_str().to_os_string()];
            argv.extend(verb_args(spec.program, layout, build));
            (spec.program, build, argv)
        })
        .collect()
}

/// Whether a warm may start at all: the manager is on (a pinned root), the pass fetches
/// from the network — a development build's `dir:` registry lands nothing a vendor served
/// ([`crate::cli::dir_registry`]) — the store is the configured one (`records`,
/// [`crate::packages_log::records`]), never a test's or another prefix's, and this is not
/// root (`sudo` keeps the user's HOME: a root warm would leave root-owned files in it).
fn admitted(
    manager_enabled: bool,
    registry: Option<&std::ffi::OsStr>,
    records: bool,
    root: bool,
) -> bool {
    manager_enabled && crate::cli::dir_registry(registry).is_none() && records && !root
}

/// Whether this process runs as root.
fn running_as_root() -> bool {
    cfg!(unix) && crate::platform::our_uid() == 0
}

/// The programs of `landed` to warm, each with its active build: a vendor row with a warm
/// ([`VendorSpec::warm`]), not removed, held by a local pin or dev-linked, and active now.
fn plan(layout: &Layout, landed: &BTreeSet<String>) -> Vec<(&'static VendorSpec, u64)> {
    let removed = layout.removed_programs();
    let active = crate::active_builds(layout);
    landed
        .iter()
        .filter_map(|program| {
            let spec = crate::vendor_direct::spec(program)?;
            spec.warm?;
            if removed.contains(program)
                || crate::pin::is_pinned(layout, program)
                || crate::linkmode::is_linked(layout, program)
            {
                return None;
            }
            active.get(program).map(|build| (spec, *build))
        })
        .collect()
}

/// Whether this process has not yet started a warm of `program` at `build` — so a verb
/// whose lanes land the same build twice warms it once.
fn first_in_this_process(program: &str, build: u64) -> bool {
    static STARTED: std::sync::Mutex<BTreeSet<(String, u64)>> =
        std::sync::Mutex::new(BTreeSet::new());
    STARTED
        .lock()
        .is_ok_and(|mut started| started.insert((program.to_string(), build)))
}

/// The executable the detached warm runs: the `atpkg` alias beside this binary
/// ([`crate::stub::co_located_atpkg_path`], when it is a live one,
/// [`crate::stub::is_live_atpkg`] — never `current_exe`, which is `aterm` under `aterm
/// pkg update`, whose front door routes only some hidden verbs by name), else this binary
/// when it is `atpkg` (a development build), else none ([`pick_executable`]).
fn warm_executable() -> Option<PathBuf> {
    let embedded = crate::stub::co_located_atpkg_path();
    let is_file = crate::stub::is_live_atpkg(&embedded);
    pick_executable(embedded, is_file, std::env::current_exe().ok())
}

/// [`warm_executable`]'s choice, given what exists: only ever a path named `atpkg`.
fn pick_executable(
    embedded: PathBuf,
    embedded_is_file: bool,
    current: Option<PathBuf>,
) -> Option<PathBuf> {
    let named_atpkg = |p: &Path| p.file_stem().and_then(|n| n.to_str()) == Some("atpkg");
    if embedded.is_absolute() && embedded_is_file && named_atpkg(&embedded) {
        return Some(embedded);
    }
    current.filter(|exe| named_atpkg(exe))
}

/// The detached warm's argv after the executable: `__warm <program> <prefix> <build>`.
fn verb_args(program: &str, layout: &Layout, build: u64) -> Vec<OsString> {
    vec![
        OsString::from(HIDDEN_VERB),
        OsString::from(program),
        layout.prefix.clone().into_os_string(),
        OsString::from(build.to_string()),
    ]
}

/// The verb's operands, vetted: a vendor row with a warm, an absolute prefix, a build.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    spec: &'static VendorSpec,
    prefix: PathBuf,
    build: u64,
}

impl Request {
    /// `None` for anything but exactly `<program> <absolute prefix> <build>` naming a
    /// program that is warmed.
    fn parse(rest: &[OsString]) -> Option<Self> {
        let [program, prefix, build] = rest else {
            return None;
        };
        let spec = crate::vendor_direct::spec(program.to_str()?)?;
        spec.warm?;
        let prefix = PathBuf::from(prefix);
        if !prefix.is_absolute() {
            return None;
        }
        let build = build.to_str()?.parse().ok()?;
        Some(Self {
            spec,
            prefix,
            build,
        })
    }
}

/// `atpkg __warm <program> <prefix> <build>`: warm that build if it is still the active one.
pub fn run_hidden(rest: &[OsString]) -> ExitCode {
    crate::notice::silence();
    if let Some(request) = Request::parse(rest).filter(|_| !running_as_root()) {
        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let home = aterm_types::dirs::home_dir();
        let _ = warm(
            &request,
            &inherited,
            login_shell_env,
            home.as_deref(),
            HOLD,
            DEADLINE,
        );
    }
    ExitCode::SUCCESS
}

/// The variable the rc hook exports in every shell that sources it ([`crate::hooks`]).
const SHELL_MARK: &str = "ATPKG_BIN";

/// What the probe shell exports about itself rather than the person — where it stood and
/// how deep it ran — and so is never taken from it ([`persons_setup`]): the warm runs from
/// home ([`warm_command`]), and a `PWD` the rc `cd`'d to would name another directory.
const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// The person's own setup as the program's next launch will see it: `inherited`, and —
/// where no shell of theirs made it ([`SHELL_MARK`] absent) — every variable their login
/// shell exports (`login`, asked at most once) that `inherited` leaves unset or empty, but
/// the shell's own ([`SHELL_OWN`]).
fn persons_setup(
    inherited: &[(OsString, OsString)],
    login: impl FnOnce() -> Option<Vec<(OsString, OsString)>>,
) -> Option<Vec<(OsString, OsString)>> {
    let set_in = |name: &OsString| {
        inherited
            .iter()
            .any(|(key, value)| key == name && !value.is_empty())
    };
    let mut setup = inherited.to_vec();
    if set_in(&OsString::from(SHELL_MARK)) {
        return Some(setup);
    }
    for (name, value) in login()? {
        if value.is_empty() || SHELL_OWN.iter().any(|own| name == *own) || set_in(&name) {
            continue;
        }
        setup.retain(|(key, _)| *key != name);
        setup.push((name, value));
    }
    Some(setup)
}

/// How long the person's login shell has to print its environment ([`login_shell_env`]):
/// the window's own bound for the `PATH` it reads the same way (aterm-gui
/// `spawn::LOGIN_PATH_BUDGET`) — an rc file that blocks (a `read`, an `ssh-add` prompt)
/// costs a warm no more than this.
#[cfg(unix)]
const LOGIN_BUDGET: Duration = Duration::from_secs(5);

/// How long the read of an answered login shell may outlast [`LOGIN_BUDGET`]: what it
/// printed is in the pipe by then, but a background job its rc started can hold the pipe
/// open for as long as it lives.
#[cfg(unix)]
const LOGIN_GRACE: Duration = Duration::from_secs(1);

/// The variable the probe's own `env -0` sets ([`login_shell_env`]), and nothing else
/// does.
#[cfg(unix)]
const ASKED_MARK: &str = "ATPKG_WARM_ASKED";

/// The person's login shell's environment ([`persons_setup`]): `<shell> -l -i -c
/// '/usr/bin/env -0 ATPKG_WARM_ASKED=1'` — the shell [`login_shell`] picks — in a session
/// of its own (no controlling terminal for `-i` to take; its id is the group the budget
/// kills), its stdin and stderr `/dev/null`.
#[cfg(unix)]
fn login_shell_env() -> Option<Vec<(OsString, OsString)>> {
    use std::io::Read as _;
    let shell = login_shell(std::env::var_os("SHELL"))?;
    let mut child = aterm_uds::spawnfd::command(&shell)
        .args(["-l", "-i", "-c", &format!("/usr/bin/env -0 {ASKED_MARK}=1")])
        .env_remove(ASKED_MARK)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .setsid(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sent, chunks) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if sent.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    let deadline = std::time::Instant::now() + LOGIN_BUDGET;
    let answered = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                crate::platform::kill_process_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !answered {
        return None;
    }
    let stop = deadline.max(std::time::Instant::now()) + LOGIN_GRACE;
    let mut out = Vec::new();
    while let Ok(chunk) =
        chunks.recv_timeout(stop.saturating_duration_since(std::time::Instant::now()))
    {
        out.extend_from_slice(&chunk);
    }
    let mut records = parse_env0(&out);
    let asked = records
        .iter()
        .position(|(name, value)| name == ASKED_MARK && value == "1")?;
    records.remove(asked);
    Some(records)
}

/// No login shell to ask off Unix, where no warm is started ([`spawn_landed`]).
#[cfg(not(unix))]
fn login_shell_env() -> Option<Vec<(OsString, OsString)>> {
    None
}

/// The shell [`login_shell_env`] runs: `$SHELL` when it is zsh, bash or fish (whose `-l -i
/// -c` reads the person's login and interactive rc files, then runs the probe), else none.
#[cfg(unix)]
fn login_shell(shell: Option<OsString>) -> Option<PathBuf> {
    shell.map(PathBuf::from).filter(|s| {
        matches!(
            s.file_name().and_then(|n| n.to_str()),
            Some("zsh" | "bash" | "fish")
        )
    })
}

/// `env -0`'s records as (name, value) pairs: each `NAME=VALUE` ends at a NUL and the value
/// is taken whole, so a multi-line value elsewhere (an exported function, a prompt) cannot
/// forge a record as a line-split read lets it.
#[cfg(unix)]
fn parse_env0(out: &[u8]) -> Vec<(OsString, OsString)> {
    use std::os::unix::ffi::OsStringExt as _;
    out.split(|b| *b == 0)
        .filter_map(|record| {
            let eq = record.iter().position(|b| *b == b'=')?;
            let name = &record[..eq];
            let name = name
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(name, |nl| &name[nl + 1..]);
            (!name.is_empty()).then(|| {
                (
                    OsString::from_vec(name.to_vec()),
                    OsString::from_vec(record[eq + 1..].to_vec()),
                )
            })
        })
        .collect()
}

/// How a warm ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The program exited with this code.
    Exit(i32),
    /// A signal ended it (not this warm's kill).
    #[cfg(unix)]
    Signal(i32),
    /// It outlived the deadline and its group was killed.
    Killed,
    /// It could not be started.
    SpawnFailed,
    /// Not run: the user opted out of the traffic it makes ([`Warm::opt_out`]).
    OptedOut,
    /// Not run: the program has never run on this account ([`Warm::state`]).
    NeverRun,
    /// Not run: no shell of the person's made this environment and their login shell gave
    /// no answer ([`persons_setup`]), so an opt-out they keep there could not be read.
    SetupUnread,
}

impl Ended {
    /// The log's `outcome` value.
    fn words(&self) -> String {
        match self {
            Self::Exit(code) => format!("exit={code}"),
            #[cfg(unix)]
            Self::Signal(n) => format!("signal={n}"),
            Self::Killed => String::from("killed"),
            Self::SpawnFailed => String::from("spawn-failed"),
            Self::OptedOut => String::from("opted-out"),
            Self::NeverRun => String::from("never-run"),
            Self::SetupUnread => String::from("setup-unread"),
        }
    }
}

/// Why the program `warm` warms is not run under the `inherited` environment and `home`:
/// the user's opt-out ([`Warm::opt_out`]), or no state of it on this account
/// ([`Warm::state`]) — else `None`.
fn declined(warm: &Warm, inherited: &[(OsString, OsString)], home: Option<&Path>) -> Option<Ended> {
    let set = |name: &str| {
        inherited
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .filter(|value| !value.is_empty())
    };
    if warm.opt_out.is_some_and(|name| set(name).is_some()) {
        return Some(Ended::OptedOut);
    }
    let state = warm.state?;
    let has_run = match set(state.dir_var) {
        Some(dir) => Path::new(dir).exists(),
        None => home.is_some_and(|home| state.under_home.iter().any(|d| home.join(d).exists())),
    };
    (!has_run).then_some(Ended::NeverRun)
}

/// Run `request`'s warm with its stdin held `hold` and the whole of it bounded by
/// `deadline`, under the `inherited` environment completed with the person's own setup
/// ([`persons_setup`], `login` its login shell's environment) from `home`, and log it — or
/// nothing at all when the build is no longer the active one (a later landing, a rollback:
/// that build's own warm, or none, is what counts; no login shell is asked then).
fn warm(
    request: &Request,
    inherited: &[(OsString, OsString)],
    login: impl FnOnce() -> Option<Vec<(OsString, OsString)>>,
    home: Option<&Path>,
    hold: Duration,
    deadline: Duration,
) -> Option<Ended> {
    let layout = Layout {
        prefix: request.prefix.clone(),
    };
    let program = request.spec.program;
    if crate::active_builds(&layout).get(program) != Some(&request.build) {
        return None;
    }
    let tool = ToolName::new(program)?;
    let warm = request.spec.warm.as_ref()?;
    let Some(setup) = persons_setup(inherited, login) else {
        let ended = declined(warm, inherited, home)
            .filter(|why| *why == Ended::OptedOut)
            .unwrap_or(Ended::SetupUnread);
        log(&layout, program, request.build, &ended.words(), 0);
        return Some(ended);
    };
    let started = std::time::Instant::now();
    let ended = if let Some(declined) = declined(warm, &setup, home) {
        declined
    } else {
        let keys = inherited.iter().map(|(key, _)| key.clone());
        let mut command = warm_command(&layout, &tool, warm, keys, home);
        for (name, value) in setup.iter().filter(|pair| !inherited.contains(pair)) {
            if !name.to_str().is_some_and(|name| stripped(warm, name))
                && !warm.env.iter().any(|(child, _)| name == *child)
            {
                command.env(name, value);
            }
        }
        match aterm_uds::spawnfd::spawn_past_text_busy(|| command.spawn()) {
            Ok(child) => drive(child, hold, deadline),
            Err(_) => Ended::SpawnFailed,
        }
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    log(&layout, program, request.build, &ended.words(), ms);
    Some(ended)
}

/// Whether the variable `name` is kept from the program `warm` warms: an AI variable
/// ([`aterm_types::domain::is_ai_env_var`]) that is not the person's own setup of that
/// program ([`Warm::keep`]) — another vendor's setup is kept from it like a session's.
fn stripped(warm: &Warm, name: &str) -> bool {
    aterm_types::domain::is_ai_env_var(name) && !warm.keep.contains(&name)
}

/// The warm's command: `<prefix>/bin/<tool> <argv…>` ([`crate::landing::shim_command`],
/// [`Warm::argv`]), every inherited AI variable among `inherited` removed but the user's
/// own setup of the program ([`Warm::keep`]), [`Warm::env`] set, run from `home` (else the
/// prefix), stdin a pipe the warm never writes, stdout and stderr `/dev/null`, in a process
/// group of its own so the deadline can kill all of it, and holding nothing else of this
/// process ([`aterm_uds::spawnfd::InheritNothing`]): the shim command is raw because the
/// pass-throughs that exec it hand their caller's descriptors on, and the warm is no
/// pass-through.
fn warm_command(
    layout: &Layout,
    tool: &ToolName,
    warm: &Warm,
    inherited: impl IntoIterator<Item = OsString>,
    home: Option<&Path>,
) -> aterm_uds::spawnfd::StdCommand {
    use aterm_uds::spawnfd::InheritNothing as _;
    let args: Vec<String> = warm.argv.iter().map(|a| (*a).to_string()).collect();
    let mut command = crate::landing::shim_command(layout, tool, &args);
    command.inherit_nothing();
    for key in inherited {
        if key.to_str().is_some_and(|name| stripped(warm, name)) {
            command.env_remove(key);
        }
    }
    for (name, value) in warm.env {
        command.env(name, value);
    }
    command
        .current_dir(home.unwrap_or(layout.prefix.as_path()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
}

/// Wait for `child`: its stdin closed once `hold` has passed, its whole process group
/// killed (and it reaped) once `deadline` has.
fn drive(mut child: std::process::Child, hold: Duration, deadline: Duration) -> Ended {
    let started = std::time::Instant::now();
    let mut stdin = child.stdin.take();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return exit_of(status),
            Ok(None) => {}
            Err(_) => break,
        }
        let spent = started.elapsed();
        if spent >= deadline {
            break;
        }
        if spent >= hold {
            drop(stdin.take());
        }
        std::thread::sleep(POLL);
    }
    kill_group(&mut child);
    let _ = child.wait();
    Ended::Killed
}

/// An exit status as the log says it.
fn exit_of(status: std::process::ExitStatus) -> Ended {
    if let Some(code) = status.code() {
        return Ended::Exit(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return Ended::Signal(signal);
        }
    }
    Ended::Exit(-1)
}

/// SIGKILL `child`'s process group — it leads one ([`warm_command`]), so whatever it
/// started goes with it — and `child` itself, should the group be gone.
fn kill_group(child: &mut std::process::Child) {
    crate::platform::kill_process_group(child.id());
    let _ = child.kill();
}

/// The warm's one package-log line.
fn log(layout: &Layout, program: &str, build: u64, outcome: &str, ms: u64) {
    crate::packages_log::append(
        layout,
        &crate::packages_log::Event::Warm {
            program,
            build,
            outcome,
            ms,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch store (a fresh 0700 prefix), removed when the test ends however it
    /// ends.
    struct Scratch {
        layout: Layout,
        _dir: aterm_tempfile::TempDir,
    }

    fn scratch(label: &str) -> Scratch {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-vlane-warm-{label}-"))
            .tempdir()
            .unwrap();
        Scratch {
            layout: Layout {
                prefix: dir.path().to_path_buf(),
            },
            _dir: dir,
        }
    }

    /// `program` active at `build` in `layout`, its store executable the shell `script`.
    fn install(layout: &Layout, program: &str, build: u64, script: &str) {
        let dir = layout.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let tool = ToolName::new(program).unwrap();
        let exe = dir.join("bin").join(tool.exe_file());
        std::fs::write(&exe, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        crate::activate::install_shims(
            layout,
            &dir,
            &[program.to_string()],
            crate::activate::Aliases::Off,
        )
        .unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(crate::active_builds(layout).get(program), Some(&build));
    }

    fn landed(programs: &[&str]) -> BTreeSet<String> {
        programs.iter().map(|p| (*p).to_string()).collect()
    }

    fn planned(layout: &Layout, programs: &[&str]) -> Vec<(&'static str, u64)> {
        plan(layout, &landed(programs))
            .into_iter()
            .map(|(spec, build)| (spec.program, build))
            .collect()
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// An absolute prefix on this platform.
    fn abs_prefix() -> &'static str {
        if cfg!(windows) {
            "C:\\aterm\\pkg"
        } else {
            "/opt/pkg"
        }
    }

    /// Poll for `path` for up to a minute — a freshly written script's first exec can wait
    /// on the system's code-policy check for seconds on a busy machine.
    #[cfg(unix)]
    fn appears(path: &Path) -> bool {
        (0..1200).any(|_| {
            let there = path.exists();
            if !there {
                std::thread::sleep(Duration::from_millis(50));
            }
            there
        })
    }

    /// No warm under an unpinned manager, from a development `dir:` registry, over a store
    /// that is not the configured one, or as root; the negative control warms.
    #[test]
    fn a_warm_starts_only_for_the_configured_store_on_the_network_lane() {
        let dir = std::ffi::OsStr::new("dir:/tmp/registry");
        let network = std::ffi::OsStr::new("https://example.invalid");
        assert!(admitted(true, None, true, false), "the negative control");
        assert!(admitted(true, Some(network), true, false));
        assert!(!admitted(false, None, true, false), "an unpinned manager");
        assert!(!admitted(true, Some(dir), true, false), "a dir: registry");
        assert!(
            !admitted(true, None, false, false),
            "not the configured store"
        );
        assert!(!admitted(true, None, true, true), "root");
    }

    /// [`spawn_landed`] starts `<atpkg> __warm <program> <prefix> <build>` once per landed,
    /// planned build — none when not admitted or with no executable, and none again for a
    /// build this process already started.
    #[test]
    fn a_landed_build_is_launched_once_through_atpkg_only_when_admitted() {
        let scratch = scratch("launches");
        let layout = &scratch.layout;
        install(layout, "claude", 2_009_991, "#!/bin/sh\nexit 0\n");
        let programs = landed(&["claude", "ay"]);
        let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
        assert!(
            launches(layout, &programs, false, Some(atpkg)).is_empty(),
            "refused"
        );
        assert!(
            launches(layout, &programs, true, None).is_empty(),
            "no executable"
        );
        let mut expected = vec![atpkg.as_os_str().to_os_string()];
        expected.extend(os(&["__warm", "claude"]));
        expected.push(layout.prefix.clone().into_os_string());
        expected.push(OsString::from("2009991"));
        assert_eq!(
            launches(layout, &programs, true, Some(atpkg)),
            [("claude", 2_009_991, expected)]
        );
        assert!(
            launches(layout, &programs, true, Some(atpkg)).is_empty(),
            "started once per process"
        );
    }

    /// The executable is only ever one named `atpkg`: the co-located alias when it exists,
    /// else the running binary when it is `atpkg` — never `aterm`, whose front door does
    /// not route `__warm`.
    #[test]
    fn the_warm_runs_only_an_executable_named_atpkg() {
        let bundle = PathBuf::from(abs_prefix()).join("bundle");
        let embedded = bundle.join("atpkg");
        let aterm = Some(bundle.join("aterm"));
        let dev = Some(PathBuf::from("/src/target/debug/atpkg"));
        assert_eq!(
            pick_executable(embedded.clone(), true, aterm.clone()),
            Some(embedded.clone())
        );
        assert_eq!(pick_executable(embedded.clone(), false, aterm), None);
        assert_eq!(pick_executable(embedded.clone(), false, dev.clone()), dev);
        assert_eq!(
            pick_executable(PathBuf::from("atpkg"), true, None),
            None,
            "a relative alias"
        );
        assert_eq!(
            pick_executable(bundle.join("aterm"), true, None),
            None,
            "an alias not named atpkg"
        );
        let picked = warm_executable();
        assert!(
            picked
                .as_deref()
                .is_none_or(|p| p.file_stem() == Some(std::ffi::OsStr::new("atpkg"))),
            "{picked:?}"
        );
    }

    /// Only a landed program with a warm, active, and not removed, held or dev-linked —
    /// each with the build that is active now.
    #[test]
    fn the_plan_skips_removed_held_linked_inactive_and_unwarmed_programs() {
        let scratch = scratch("plan");
        let layout = &scratch.layout;
        assert!(
            planned(layout, &["claude", "codex"]).is_empty(),
            "nothing active"
        );
        install(layout, "claude", 2_001_280, "#!/bin/sh\nexit 0\n");
        install(layout, "codex", 1_560_001, "#!/bin/sh\nexit 0\n");
        install(layout, "ay", 18, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            planned(layout, &["claude", "codex", "ay", "trust"]),
            [("claude", 2_001_280), ("codex", 1_560_001)],
            "only the vendor rows, at their active builds"
        );
        assert_eq!(planned(layout, &["codex"]), [("codex", 1_560_001)]);
        std::fs::write(layout.removed(), "# removed\ncodex\n").unwrap();
        assert_eq!(
            planned(layout, &["claude", "codex"]),
            [("claude", 2_001_280)]
        );
        std::fs::remove_file(layout.removed()).unwrap();
        crate::pin::set_pinned(layout, "claude", true).unwrap();
        assert_eq!(
            planned(layout, &["claude", "codex"]),
            [("codex", 1_560_001)]
        );
        crate::pin::set_pinned(layout, "claude", false).unwrap();
        let marker = layout.link_marker("claude");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(
            &marker,
            "schema = 1\nprogram = \"claude\"\ncheckout = \"/tmp/claude\"\n",
        )
        .unwrap();
        assert!(crate::linkmode::is_linked(layout, "claude"));
        assert_eq!(
            planned(layout, &["claude", "codex"]),
            [("codex", 1_560_001)]
        );
        std::fs::remove_file(&marker).unwrap();
        assert_eq!(
            planned(layout, &["claude", "codex"]),
            [("claude", 2_001_280), ("codex", 1_560_001)],
            "the negative control"
        );
    }

    /// Exactly `<program> <absolute prefix> <build>`, for a warmed program, as
    /// [`spawn_landed`] passes them.
    #[test]
    fn the_verb_takes_exactly_a_warmed_program_an_absolute_prefix_and_a_build() {
        let prefix = abs_prefix();
        let layout = Layout {
            prefix: PathBuf::from(prefix),
        };
        let argv = verb_args("claude", &layout, 2_001_280);
        assert_eq!(argv, os(&["__warm", "claude", prefix, "2001280"]));
        assert_eq!(
            Request::parse(&argv[1..]),
            Some(Request {
                spec: crate::vendor_direct::spec("claude").unwrap(),
                prefix: PathBuf::from(prefix),
                build: 2_001_280,
            })
        );
        for bad in [
            &["claude", prefix][..],
            &["claude", prefix, "7", "extra"],
            &["claude", "relative/pkg", "7"],
            &["claude", prefix, "seven"],
            &["ay", prefix, "7"],
            &["", prefix, "7"],
        ] {
            assert_eq!(Request::parse(&os(bad)), None, "{bad:?}");
        }
    }

    /// A prefix that is not UTF-8 reaches the request byte for byte.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_prefix_is_taken_byte_for_byte() {
        use std::os::unix::ffi::OsStrExt as _;
        let prefix = std::ffi::OsStr::from_bytes(b"/opt/p\xffkg").to_os_string();
        let request = Request::parse(&[
            OsString::from("claude"),
            prefix.clone(),
            OsString::from("7"),
        ])
        .unwrap();
        assert_eq!(request.prefix.as_os_str(), prefix);
    }

    /// The `bin/` shim with the row's argv verbatim, every inherited AI variable removed
    /// but the user's own setup of that program ([`Warm::keep`]) and nothing else (another
    /// vendor's setup is removed), the row's variables set, run from home.
    #[test]
    fn the_command_runs_the_bin_shim_with_the_table_argv_and_no_ai_variables() {
        let layout = Layout {
            prefix: PathBuf::from(abs_prefix()),
        };
        let claude = crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
        let tool = ToolName::new("claude").unwrap();
        let inherited = [
            "CLAUDECODE",
            "CLAUDE_CODE_SIMPLE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "AI_AGENT",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "PATH",
            "HOME",
            "LANG",
        ]
        .map(OsString::from);
        let home = Path::new("/Users//someone");
        let envs = |command: &std::process::Command| -> Vec<(String, Option<String>)> {
            command
                .get_envs()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.map(|v| v.to_string_lossy().into_owned()),
                    )
                })
                .collect()
        };
        let removed = |names: &[&str]| -> Vec<(String, Option<String>)> {
            names.iter().map(|k| ((*k).to_string(), None)).collect()
        };
        let session = [
            "AI_AGENT",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SIMPLE",
        ];
        let command = warm_command(&layout, &tool, &claude, inherited.clone(), Some(home));
        assert_eq!(command.get_program(), layout.shim(&tool).as_os_str());
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(args, claude.argv);
        assert_eq!(command.get_current_dir(), Some(home));
        let mut expected = removed(&session);
        expected.extend(removed(&["CODEX_HOME"]));
        expected.push((
            "ENABLE_CLAUDEAI_MCP_SERVERS".to_string(),
            Some("false".to_string()),
        ));
        expected.sort();
        assert_eq!(
            envs(&command),
            expected,
            "claude's config home, its endpoint choice, PATH, HOME and LANG are inherited"
        );
        let codex = crate::vendor_direct::spec("codex").unwrap().warm.unwrap();
        let tool = ToolName::new("codex").unwrap();
        let command = warm_command(&layout, &tool, &codex, inherited, Some(home));
        let mut expected = removed(&session);
        expected.extend(removed(&[
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CONFIG_DIR",
        ]));
        expected.sort();
        assert_eq!(envs(&command), expected, "CODEX_HOME is inherited");
        let command = warm_command(&layout, &tool, &codex, [], None);
        assert_eq!(command.get_current_dir(), Some(layout.prefix.as_path()));
    }

    /// claude is not warmed when the user turned its non-essential traffic off, nor is a
    /// program with no state on this account, found where the user's own config variable
    /// puts it.
    #[test]
    fn an_opted_out_or_never_run_program_is_declined() {
        let scratch = scratch("declined");
        let home = scratch.layout.prefix.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            pairs
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect()
        };
        let claude = &crate::vendor_direct::spec("claude").unwrap().warm.unwrap();
        let codex = &crate::vendor_direct::spec("codex").unwrap().warm.unwrap();
        let none = env(&[]);
        let h = Some(home.as_path());
        assert_eq!(declined(claude, &none, h), Some(Ended::NeverRun));
        assert_eq!(declined(codex, &none, h), Some(Ended::NeverRun));
        assert_eq!(declined(claude, &none, None), Some(Ended::NeverRun));
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        assert_eq!(declined(claude, &none, h), None, "the negative control");
        assert_eq!(declined(codex, &none, h), None);
        std::fs::remove_file(home.join(".claude.json")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        assert_eq!(declined(claude, &none, h), None, "~/.claude alone");
        let elsewhere = scratch.layout.prefix.join("elsewhere");
        let moved = env(&[
            ("CLAUDE_CONFIG_DIR", elsewhere.to_str().unwrap()),
            ("CODEX_HOME", elsewhere.to_str().unwrap()),
        ]);
        assert_eq!(declined(claude, &moved, h), Some(Ended::NeverRun));
        assert_eq!(declined(codex, &moved, h), Some(Ended::NeverRun));
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(declined(claude, &moved, h), None);
        assert_eq!(declined(codex, &moved, h), None);
        let off = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")]);
        assert_eq!(declined(claude, &off, h), Some(Ended::OptedOut));
        assert_eq!(declined(codex, &off, h), None);
        let empty = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "")]);
        assert_eq!(declined(claude, &empty, h), None);
        let stateless = Warm {
            state: None,
            ..*claude
        };
        assert_eq!(declined(&stateless, &none, None), None);
    }

    /// In-process with short durations: a program that exits on stdin EOF ends once the
    /// hold closes it, with its own code; one that ignores EOF and SIGTERM is killed with
    /// its whole group at the deadline.
    #[cfg(unix)]
    #[test]
    fn the_warm_closes_stdin_at_the_hold_and_kills_the_group_at_the_deadline() {
        let scratch = scratch("drive");
        let layout = &scratch.layout;
        let marks = layout.prefix.join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        let tool = ToolName::new("claude").unwrap();
        let spec = crate::vendor_direct::spec("claude").unwrap();
        let spawn = || {
            aterm_spec::wait::past_text_busy(|| {
                warm_command(layout, &tool, &spec.warm.unwrap(), [], None).spawn()
            })
            .unwrap()
        };
        install(
            layout,
            "claude",
            2_001_280,
            &format!(
                "#!/bin/sh\necho up > '{m}/started'\ncat >/dev/null\necho eof > '{m}/eof'\n\
                 exit 3\n",
                m = marks.display()
            ),
        );
        let child = spawn();
        assert!(appears(&marks.join("started")), "the script ran");
        assert_eq!(
            drive(child, Duration::from_millis(300), Duration::from_secs(120)),
            Ended::Exit(3)
        );
        assert!(marks.join("eof").exists());
        install(
            layout,
            "claude",
            2_001_281,
            &format!(
                "#!/bin/sh\ntrap '' TERM\nsleep 120 &\necho $! > '{}/grandchild.tmp'\n\
                 mv '{0}/grandchild.tmp' '{0}/grandchild'\nwhile :; do sleep 1; done\n",
                marks.display()
            ),
        );
        let child = spawn();
        assert!(appears(&marks.join("grandchild")), "the script ran");
        let grandchild: libc::pid_t = std::fs::read_to_string(marks.join("grandchild"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            drive(child, Duration::from_millis(50), Duration::from_millis(300)),
            Ended::Killed
        );
        let gone = (0..3000).any(|_| {
            // SAFETY: signal 0 only asks whether the pid is addressable.
            let alive = unsafe { libc::kill(grandchild, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(20));
            }
            !alive
        });
        assert!(gone, "the group went with it");
    }

    /// The warm's child holds its stdio and nothing else of this process.
    #[cfg(unix)]
    #[test]
    fn the_warm_child_inherits_its_stdio_and_nothing_else() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let scratch = scratch("inherit");
        let layout = &scratch.layout;
        let marks = layout.prefix.join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        let tool = ToolName::new("claude").unwrap();
        let spec = crate::vendor_direct::spec("claude").unwrap();
        let null = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD duplicates a descriptor this test owns onto the lowest free number from 100 (and close-on-exec clear); the new one is owned below.
        let fd = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, 100) };
        assert!(fd >= 100, "F_DUPFD: {}", std::io::Error::last_os_error());
        // SAFETY: `fd` was just opened here and nothing else owns it.
        let _held = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        install(
            layout,
            "claude",
            2_001_282,
            &format!(
                "#!/bin/sh
if {held}; then w=held; else w=absent; fi
                 echo $w > '{m}/fd.tmp'
mv '{m}/fd.tmp' '{m}/fd'
",
                held = aterm_spec::host::sh_held(fd),
                m = marks.display()
            ),
        );
        let seen = || {
            let mark = marks.join("fd");
            assert!(appears(&mark), "the script ran");
            let what = std::fs::read_to_string(&mark).unwrap().trim().to_string();
            std::fs::remove_file(&mark).unwrap();
            what
        };
        let mut control = aterm_uds::spawnfd::transparent(layout.shim(&tool))
            .spawn()
            .unwrap();
        assert_eq!(
            seen(),
            "held",
            "the negative control: the pass-throughs' spawn hands it on"
        );
        assert!(control.wait().unwrap().success());
        let mut child = aterm_spec::wait::past_text_busy(|| {
            warm_command(layout, &tool, &spec.warm.unwrap(), [], None).spawn()
        })
        .unwrap();
        assert_eq!(seen(), "absent");
        drop(child.stdin.take());
        let _ = child.wait();
    }

    /// Each warm that ran or was declined is one `warm` line — its own exit code, `killed`,
    /// `spawn-failed`, `opted-out`, `never-run` — and a stale build runs and logs nothing.
    #[cfg(unix)]
    #[test]
    fn each_warm_is_one_log_line_and_a_stale_build_is_none() {
        let scratch = scratch("record");
        let layout = &scratch.layout;
        let logs = layout.prefix.join("logs");
        let _bound = crate::packages_log::test_bind::bind(logs.clone(), layout.prefix.clone());
        let home = layout.prefix.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let spec = crate::vendor_direct::spec("claude").unwrap();
        let request = |build| Request {
            spec,
            prefix: layout.prefix.clone(),
            build,
        };
        let no_env: Vec<(OsString, OsString)> = Vec::new();
        let run = |build, env: &[(OsString, OsString)], home: &Path, deadline| {
            warm(
                &request(build),
                env,
                || Some(Vec::new()),
                Some(home),
                Duration::ZERO,
                deadline,
            )
        };
        install(
            layout,
            "claude",
            2_001_280,
            "#!/bin/sh\ncat >/dev/null\nexit 3\n",
        );
        assert_eq!(
            run(2_001_280, &no_env, &home, Duration::from_secs(120)),
            Some(Ended::Exit(3))
        );
        install(
            layout,
            "claude",
            2_001_281,
            "#!/bin/sh\ntrap '' TERM\nwhile :; do sleep 1; done\n",
        );
        assert_eq!(
            run(2_001_281, &no_env, &home, Duration::from_millis(300)),
            Some(Ended::Killed)
        );
        let before = crate::packages_log::read_tail(&logs, 100).len();
        assert_eq!(run(2_001_280, &no_env, &home, Duration::from_secs(1)), None);
        assert_eq!(crate::packages_log::read_tail(&logs, 100).len(), before);
        let off = vec![(
            OsString::from("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
            OsString::from("1"),
        )];
        assert_eq!(
            run(2_001_281, &off, &home, Duration::from_secs(1)),
            Some(Ended::OptedOut)
        );
        assert_eq!(
            run(2_001_281, &no_env, &layout.prefix, Duration::from_secs(1)),
            Some(Ended::NeverRun)
        );
        let shim = layout.shim(&ToolName::new("claude").unwrap());
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(crate::active_builds(layout).get("claude"), Some(&2_001_281));
        assert_eq!(
            run(2_001_281, &no_env, &home, Duration::from_secs(1)),
            Some(Ended::SpawnFailed)
        );
        let outcomes: Vec<(String, String, String)> = crate::packages_log::read_tail(&logs, 100)
            .iter()
            .map(|e| {
                assert_eq!(e.kind, crate::packages_log::kind::WARM);
                assert!(e.get("ms").is_some());
                (
                    e.get("program").unwrap().to_string(),
                    e.get("build").unwrap().to_string(),
                    e.get("outcome").unwrap().to_string(),
                )
            })
            .collect();
        let row = |b: &str, o: &str| ("claude".to_string(), b.to_string(), o.to_string());
        assert_eq!(
            outcomes,
            [
                row("2001281", "spawn-failed"),
                row("2001281", "never-run"),
                row("2001281", "opted-out"),
                row("2001281", "killed"),
                row("2001280", "exit=3"),
            ]
        );
    }
}
