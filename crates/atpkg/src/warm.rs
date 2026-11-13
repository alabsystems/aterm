// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The warm of a landed agent build.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::store::{Layout, ToolName};
use crate::vendor_direct::{Login, VendorSpec, Warm};

/// `atpkg __warm <program> <prefix> <build>`.
pub const HIDDEN_VERB: &str = "__warm";

/// How long the warm leaves the program's stdin open before closing it.
pub const HOLD: Duration = Duration::from_secs(10);

/// When the warm's process group is killed, counted from its start.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// TEST SEAM ([`aterm_types::dev_seam!`]): a count of milliseconds that [`run_hidden`]'s waits
/// (its [`DEADLINE`], its login shell's budget and that read's grace) each last at least, so a
/// test whose program and login shell must answer waits out only its hang detector on a loaded
/// machine.
pub const WAITS_AT_LEAST_ENV: &str = "ATPKG_WARM_WAITS_AT_LEAST_MS";

/// [`WAITS_AT_LEAST_ENV`] as a debug build was given it, else zero.
fn waits_at_least() -> Duration {
    aterm_types::dev_seam!(WAITS_AT_LEAST_ENV)
        .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
        .map_or(Duration::ZERO, Duration::from_millis)
}

const POLL: Duration = Duration::from_millis(50);

/// Warm each of `programs`.
pub(crate) fn spawn_landed(layout: &Layout, programs: &BTreeSet<String>) {
    if programs.is_empty() || !cfg!(unix) {
        return;
    }
    let admitted = admitted(
        crate::manager_enabled(),
        crate::source::registry_seam().as_deref(),
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

fn admitted(
    manager_enabled: bool,
    registry: Option<&std::ffi::OsStr>,
    records: bool,
    root: bool,
) -> bool {
    manager_enabled && crate::source::dir_registry(registry).is_none() && records && !root
}

fn running_as_root() -> bool {
    cfg!(unix) && crate::platform::our_uid() == 0
}

fn plan(layout: &Layout, landed: &BTreeSet<String>) -> Vec<(&'static VendorSpec, u64)> {
    let Ok(removed) = layout.removed_programs() else {
        return Vec::new();
    };
    let active = crate::active_builds(layout);
    landed
        .iter()
        .filter_map(|program| {
            let spec = crate::vendor_direct::spec(program)?;
            spec.warm?;
            if removed.contains(program)
                || crate::pin::holds_update(layout, program)
                || crate::linkmode::is_linked(layout, program)
            {
                return None;
            }
            active.get(program).map(|build| (spec, *build))
        })
        .collect()
}

fn first_in_this_process(program: &str, build: u64) -> bool {
    static STARTED: std::sync::Mutex<BTreeSet<(String, u64)>> =
        std::sync::Mutex::new(BTreeSet::new());
    STARTED
        .lock()
        .is_ok_and(|mut started| started.insert((program.to_string(), build)))
}

fn warm_executable() -> Option<PathBuf> {
    let embedded = crate::stub::co_located_atpkg_path();
    let is_file = crate::stub::is_live_atpkg(&embedded);
    pick_executable(embedded, is_file, std::env::current_exe().ok())
}

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
        let build = crate::store::parse_build_name(build.to_str()?)?;
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
        let floor = waits_at_least();
        let _ = warm(
            &request,
            &inherited,
            || login_shell_env(floor),
            home.as_deref(),
            HOLD,
            DEADLINE.max(floor),
        );
    }
    ExitCode::SUCCESS
}

const SHELL_MARK: &str = "ATPKG_BIN";

const SHELL_OWN: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

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

/// What neither the person's login shell nor the warmed program is left a way to do: put a
/// browser, an askpass window or a prompt on the desk. Nobody asked for a warm, so nothing it
/// runs may ask anybody anything: a program that wants a browser gets one that opens nothing
/// (`BROWSER`), ssh and git get no askpass program and no terminal to prompt on, and there is
/// no X or Wayland display to open a window on. Each child also runs in a session of its own,
/// with no controlling terminal.
const NO_DESK: &[(&str, &str)] = &[
    ("BROWSER", "/usr/bin/false"),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("SSH_ASKPASS", ""),
];

/// The displays [`NO_DESK`] takes away.
const NO_DISPLAY: &[&str] = &["DISPLAY", "WAYLAND_DISPLAY"];

/// `command` with [`NO_DESK`], over anything it was given before.
fn off_the_desk(command: &mut WarmCommand) {
    for (name, value) in NO_DESK {
        command.env(name, value);
    }
    for name in NO_DISPLAY {
        command.env_remove(name);
    }
}

/// [`login_shell_env`]'s child, unspawned.
#[cfg(unix)]
fn login_shell_command(shell: &Path) -> WarmCommand {
    let mut command = aterm_uds::spawnfd::command(shell);
    command
        .args(["-l", "-i", "-c", &format!("/usr/bin/env -0 {ASKED_MARK}=1")])
        .env_remove(ASKED_MARK)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .setsid(true);
    off_the_desk(&mut command);
    command
}

/// The person's login shell's environment ([`persons_setup`]): `<shell> -l -i -c
/// '/usr/bin/env -0 ATPKG_WARM_ASKED=1'` — the shell [`login_shell`] picks — in a session
/// of its own (no controlling terminal for `-i` to take; its id is the group the budget
/// kills), its stdin and stderr `/dev/null`, with [`NO_DESK`] for whatever their rc files
/// start. It is asked only for an account that is signed in ([`warm_with_launch`]).
/// [`LOGIN_BUDGET`] and [`LOGIN_GRACE`] each last at least `floor`.
#[cfg(unix)]
fn login_shell_env(floor: Duration) -> Option<Vec<(OsString, OsString)>> {
    use std::io::Read as _;
    let shell = login_shell(std::env::var_os("SHELL"))?;
    let mut child = login_shell_command(&shell).spawn().ok()?;
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
    let deadline = std::time::Instant::now() + LOGIN_BUDGET.max(floor);
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
    let stop = deadline.max(std::time::Instant::now()) + LOGIN_GRACE.max(floor);
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
fn login_shell_env(_floor: Duration) -> Option<Vec<(OsString, OsString)>> {
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
    /// Not run: its build could not be held for first exec, or no wrapper could
    /// faithfully pin that target ([`crate::store::hold_first_exec`]).
    Unheld,
    /// Not run: the user opted out of the traffic it makes ([`Warm::opt_out`]).
    OptedOut,
    /// Not run: the program has never run on this account ([`Warm::state`]).
    NeverRun,
    /// Not run: nothing shows the program signed in on this account ([`Warm::login`]).
    SignedOut,
    /// Not run: no shell of the person's made this environment and their login shell gave
    /// no answer ([`persons_setup`]), so an opt-out they keep there could not be read.
    SetupUnread,
}

impl Ended {
    fn words(&self) -> String {
        match self {
            Self::Exit(code) => format!("exit={code}"),
            #[cfg(unix)]
            Self::Signal(n) => format!("signal={n}"),
            Self::Killed => String::from("killed"),
            Self::SpawnFailed => String::from("spawn-failed"),
            Self::Unheld => String::from("unheld"),
            Self::OptedOut => String::from("opted-out"),
            Self::NeverRun => String::from("never-run"),
            Self::SignedOut => String::from("signed-out"),
            Self::SetupUnread => String::from("setup-unread"),
        }
    }
}

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
    if let Some(state) = warm.state {
        let has_run = match set(state.dir_var) {
            Some(dir) => Path::new(dir).exists(),
            None => home.is_some_and(|home| state.under_home.iter().any(|d| home.join(d).exists())),
        };
        if !has_run {
            return Some(Ended::NeverRun);
        }
    }
    (!signed_in(&warm.login, inherited, home)).then_some(Ended::SignedOut)
}

/// The largest profile document [`signed_in`] reads (Claude Code's grows with its projects).
const PROFILE_MAX: u64 = 64 * 1024 * 1024;

/// Whether `login` shows its program signed in under `env` and `home`: a kept variable that
/// signs a launch in, a login file with bytes in the program's home, or its profile naming an
/// account. Read from files alone; anything unreadable is not a login.
fn signed_in(login: &Login, env: &[(OsString, OsString)], home: Option<&Path>) -> bool {
    let set = |name: &str| {
        env.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .filter(|value| !value.is_empty())
    };
    if login.env.iter().any(|name| set(name).is_some()) {
        return true;
    }
    let named = set(login.home_var).map(PathBuf::from);
    let Some(dir) = named
        .clone()
        .or_else(|| home.map(|home| home.join(login.home_default)))
    else {
        return false;
    };
    if login.files.iter().any(|file| {
        std::fs::metadata(dir.join(file)).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
    }) {
        return true;
    }
    login.profile.is_some_and(|profile| {
        let path = match (&named, home) {
            (Some(dir), _) => dir.join(profile.in_home),
            (None, Some(home)) => home.join(profile.under_home),
            (None, None) => return false,
        };
        std::fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= PROFILE_MAX)
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok())
                .and_then(|doc| {
                    doc.get(profile.key)
                        .and_then(|account| account.as_object().cloned())
                })
                .is_some_and(|account| !account.is_empty())
    })
}

fn warm(
    request: &Request,
    inherited: &[(OsString, OsString)],
    login: impl FnOnce() -> Option<Vec<(OsString, OsString)>>,
    home: Option<&Path>,
    hold: Duration,
    deadline: Duration,
) -> Option<Ended> {
    warm_with_launch(request, inherited, login, home, |command| {
        match aterm_uds::spawnfd::spawn_past_text_busy(|| command.spawn()) {
            Ok(child) => drive(child, hold, deadline),
            Err(_) => Ended::SpawnFailed,
        }
    })
}

type WarmCommand = aterm_uds::spawnfd::Command;

fn warm_with_launch(
    request: &Request,
    inherited: &[(OsString, OsString)],
    login: impl FnOnce() -> Option<Vec<(OsString, OsString)>>,
    home: Option<&Path>,
    launch: impl FnOnce(&mut WarmCommand) -> Ended,
) -> Option<Ended> {
    let layout = Layout {
        prefix: request.prefix.clone(),
    };
    let program = request.spec.program;
    if crate::active_build_of(&layout, program) != Some(request.build) {
        return None;
    }
    let tool = ToolName::new(program)?;
    let warm = request.spec.warm.as_ref()?;
    // Read before anything of the person's runs: an account that opted out, never ran the
    // program or is not signed in has neither its login shell's rc files nor the program run.
    if let Some(ended) = declined(warm, inherited, home) {
        log(&layout, program, request.build, &ended.words(), 0);
        return Some(ended);
    }
    let Some(setup) = persons_setup(inherited, login) else {
        let ended = Ended::SetupUnread;
        log(&layout, program, request.build, &ended.words(), 0);
        return Some(ended);
    };
    let started = std::time::Instant::now();
    let exe = layout
        .build_dir(program, request.build)
        .join("bin")
        .join(tool.exe_file());
    let ended = if let Some(declined) = declined(warm, &setup, home) {
        declined
    } else if let Ok(first_exec) = crate::store::hold_first_exec(&exe) {
        let keys = inherited.iter().map(|(key, _)| key.clone());
        let ended = if let Some(mut command) = warm_command(&layout, &tool, &exe, warm, keys, home)
        {
            for (name, value) in setup.iter().filter(|pair| !inherited.contains(pair)) {
                if !name.to_str().is_some_and(|name| stripped(warm, name))
                    && !warm.env.iter().any(|(child, _)| name == *child)
                {
                    command.env(name, value);
                }
            }
            off_the_desk(&mut command);
            launch(&mut command)
        } else {
            Ended::Unheld
        };
        // killed at the deadline or by a signal, it may still be in macOS's check of its
        // first exec, and the build stays held
        if matches!(ended, Ended::Exit(_) | Ended::SpawnFailed | Ended::Unheld) {
            first_exec.ran();
        }
        ended
    } else {
        Ended::Unheld
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    log(&layout, program, request.build, &ended.words(), ms);
    Some(ended)
}

fn stripped(warm: &Warm, name: &str) -> bool {
    aterm_types::domain::is_ai_env_var(name) && !warm.keep.contains(&name)
}

fn warm_command(
    layout: &Layout,
    tool: &ToolName,
    exe: &Path,
    warm: &Warm,
    inherited: impl IntoIterator<Item = OsString>,
    home: Option<&Path>,
) -> Option<WarmCommand> {
    let mut command = pinned_command(layout, tool, exe)?;
    command.args(warm.argv);
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
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    // A session of its own: no controlling terminal to prompt on, and its id is the group
    // the deadline kills.
    #[cfg(unix)]
    {
        command.setsid(true);
    }
    Some(command)
}

/// Render the held build's ordinary wrapper as shell input, never reopen the
/// mutable active shim. Its name remains shell $0; each warm argument stays one
/// argument. Only the catalog's Unix warms have this wrapper representation.
#[cfg(unix)]
fn pinned_command(layout: &Layout, tool: &ToolName, exe: &Path) -> Option<WarmCommand> {
    // The existing renderer represents paths as UTF-8 shell text. Refuse a path
    // it cannot preserve instead of silently substituting bytes or an active shim.
    exe.to_str()?;
    let env = crate::shim_env::of_store_binary(exe);
    let route = crate::compat::route_for_shim(&layout.shim(tool), exe);
    let body = crate::platform::sh_shim_content_routed(exe, &env, route.as_deref());
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command.arg("-c").arg(body).arg(layout.shim(tool));
    Some(command)
}

/// No Unix wrapper can be pinned here. Automatic warming is already Unix-only;
/// a directly requested warm must refuse rather than reopen a mutable .cmd shim.
#[cfg(not(unix))]
fn pinned_command(_layout: &Layout, _tool: &ToolName, _exe: &Path) -> Option<WarmCommand> {
    None
}

fn drive(mut child: aterm_uds::spawnfd::Child, hold: Duration, deadline: Duration) -> Ended {
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

fn kill_group(child: &mut aterm_uds::spawnfd::Child) {
    crate::platform::kill_process_group(child.id());
    let _ = child.kill();
}

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
#[path = "warm_tests.rs"]
mod tests;
