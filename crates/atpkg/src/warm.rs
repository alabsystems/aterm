// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WARM OF A LANDED AGENT BUILD (owner, 2026-09-23: *"latest models ready
//! immediately"*).
//!
//! A vendor CLI keeps account-bound caches that only a run of it refreshes: Claude Code
//! its bootstrap (the model options, model access and org default an account is served),
//! its feature flags and the served model catalog; codex `~/.codex/models_cache.json`. A
//! build the vendor lane just landed has never run, so the user's FIRST interactive launch
//! would pay for those fetches — and until they answer, it offers the models the build
//! shipped knowing, not the ones the account has today. So when a pass lands a new build
//! of a program whose row carries a warm ([`crate::vendor_direct::VendorSpec::warm`]), it
//! starts ONE detached `atpkg __warm <program> <prefix> <build>` ([`spawn_landed`]), which
//! runs the build once, headless, and exits ([`run_hidden`]).
//!
//! What holds it to that, and why:
//!
//! * **Silent.** A pass's stdout/stderr are a marker channel a window reads to EOF: the
//!   warm is started with every stdio on `/dev/null` in a process group of its own
//!   ([`crate::platform::spawn_detached`]), prints nothing, and always exits 0. What it
//!   did is one `warm` line in the package log ([`crate::packages_log`]).
//! * **Never a model turn.** The argv is the compiled table's, never the index's, and it
//!   carries no prompt; claude's reads stream-json from a stdin that is never written,
//!   and is closed after [`HOLD`] so the CLI exits on its own.
//! * **Never someone else's session, always the user's own setup.** The variables aterm
//!   strips from the shells it spawns ([`aterm_types::domain::is_ai_env_var`]) are removed —
//!   a pass run under a Claude session (`claude update` → `__selfupdate`) carries
//!   `CLAUDECODE`, the session's id and its messaging socket, which would tie the warm to
//!   that session — EXCEPT the ones that say where the user keeps the program's state or
//!   which endpoint it talks to ([`KEEP`]: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, an
//!   identity session's among them): stripping those refreshed the default home's caches,
//!   which that user's program never reads. It runs through the `bin/` shim, so the shim's
//!   own environment (`DISABLE_AUTOUPDATER=1`) applies.
//! * **Never against the user's choice.** A program the user turned the vendor's
//!   non-essential traffic off for ([`OPT_OUTS`]) is not warmed (its whole point is that
//!   traffic); one that has never run on this account ([`STATE`]: a default-set install
//!   the user never launched) has no account caches to refresh and is not warmed either;
//!   and nothing is warmed as root (a `sudo aterm pkg update` would leave root-owned files
//!   in the user's home).
//! * **Bounded.** Past [`DEADLINE`] the warm's whole process group is killed.
//! * **Best effort.** A warm that fails, times out or finds the build already replaced
//!   fails nothing and is never retried: the user's own first launch does the same work.
//! * **Governed by the existing switches.** Only the configured store is warmed, never a
//!   development `dir:` registry's, never under an unpinned manager, and never a program
//!   this machine removed, holds by a local pin or dev-links. No key of its own.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crate::store::{Layout, ToolName};
use crate::vendor_direct::VendorSpec;

/// `atpkg __warm <program> <prefix> <build>`: unlisted, answered on the raw argv before
/// the verb match and the store lock, like the lane helpers — it never mutates the store.
pub const HIDDEN_VERB: &str = "__warm";

/// How long the warm leaves the program's stdin open before closing it (claude's fetches
/// run while it waits for input; a closed stdin is what ends it).
pub const HOLD: Duration = Duration::from_secs(10);

/// When the warm's process group is killed, counted from its start.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// How often the warm looks at the program.
const POLL: Duration = Duration::from_millis(50);

/// Set on the program: Claude Code's claude.ai MCP connectors stay unstarted (the warm
/// wants the account's caches, not its tools).
const CHILD_ENV: &[(&str, &str)] = &[("ENABLE_CLAUDEAI_MCP_SERVERS", "false")];

/// AI variables the warm passes on although [`aterm_types::domain::is_ai_env_var`] names
/// them: not a session's state but the user's own setup — where the program keeps its
/// config and caches (the very files the warm refreshes; an aterm identity session sets
/// these two after its own strip), and which endpoint it talks to.
const KEEP: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// (program, variable): a non-empty value is the user's opt-out of exactly the traffic a
/// warm of that program exists to make, so it is not warmed. (`DISABLE_TELEMETRY` is not
/// an AI variable: it reaches the program, which honours it itself.)
const OPT_OUTS: &[(&str, &str)] = &[("claude", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")];

/// (program, the variable naming its state directory, its default state under HOME): a
/// program none of whose state exists has never run on this account — nothing to refresh.
/// A program with no row here is always warmed.
const STATE: &[(&str, &str, &[&str])] = &[
    ("claude", "CLAUDE_CONFIG_DIR", &[".claude", ".claude.json"]),
    ("codex", "CODEX_HOME", &[".codex"]),
];

/// Warm each of `programs` — the ones a vendor lane of this process just LANDED a build
/// of — once, detached and silent ([`launches`] picks which). A start that fails is a
/// `warm` line in the package log, never a word on stdout or stderr.
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

/// The programs of `landed` to warm, each with its ACTIVE build: a vendor row whose warm
/// is not empty ([`warm_argv`]), not removed, held by a local pin or dev-linked, and
/// active now.
fn plan(layout: &Layout, landed: &BTreeSet<String>) -> Vec<(&'static VendorSpec, u64)> {
    let removed = layout.removed_programs();
    let active = crate::active_builds(layout);
    landed
        .iter()
        .filter_map(|program| {
            let spec = crate::vendor_direct::spec(program)?;
            warm_argv(spec)?;
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

/// `spec`'s warm arguments, or `None` when its row warms nothing.
fn warm_argv(spec: &VendorSpec) -> Option<&'static [&'static str]> {
    (!spec.warm.is_empty()).then_some(spec.warm)
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
/// when it IS `atpkg` (a development build), else none ([`pick_executable`]).
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
    /// program that is warmed. The operands are the raw argv: the prefix is taken byte for
    /// byte (a lossy conversion would name a store that does not exist).
    fn parse(rest: &[OsString]) -> Option<Self> {
        let [program, prefix, build] = rest else {
            return None;
        };
        let spec = crate::vendor_direct::spec(program.to_str()?)?;
        warm_argv(spec)?;
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

/// `atpkg __warm <program> <prefix> <build>`: warm that build if it is still the active
/// one. Prints nothing and exits 0 whatever happens (module doc); as root it does nothing
/// at all, not even a log line (a root-owned log in the user's home is the harm).
pub fn run_hidden(rest: &[OsString]) -> ExitCode {
    crate::notice::silence();
    if let Some(request) = Request::parse(rest).filter(|_| !running_as_root()) {
        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let home = aterm_types::dirs::home_dir();
        let _ = warm(&request, &inherited, home.as_deref(), HOLD, DEADLINE);
    }
    ExitCode::SUCCESS
}

/// How a warm ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The program exited with this code.
    Exit(i32),
    /// A signal ended it (not this warm's kill).
    Signal(i32),
    /// It outlived the deadline and its group was killed.
    Killed,
    /// It could not be started.
    SpawnFailed,
    /// Not run: the user opted out of the traffic it makes ([`OPT_OUTS`]).
    OptedOut,
    /// Not run: the program has never run on this account ([`STATE`]).
    NeverRun,
}

impl Ended {
    /// The log's `outcome` value.
    fn words(&self) -> String {
        match self {
            Self::Exit(code) => format!("exit={code}"),
            Self::Signal(n) => format!("signal={n}"),
            Self::Killed => String::from("killed"),
            Self::SpawnFailed => String::from("spawn-failed"),
            Self::OptedOut => String::from("opted-out"),
            Self::NeverRun => String::from("never-run"),
        }
    }
}

/// Why `program` is not run under the `inherited` environment and `home`: the user's
/// opt-out ([`OPT_OUTS`]), or no state of it on this account ([`STATE`]) — else `None`.
fn declined(
    program: &str,
    inherited: &[(OsString, OsString)],
    home: Option<&Path>,
) -> Option<Ended> {
    let set = |name: &str| {
        inherited
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .filter(|value| !value.is_empty())
    };
    if OPT_OUTS
        .iter()
        .any(|(p, name)| *p == program && set(name).is_some())
    {
        return Some(Ended::OptedOut);
    }
    let (_, name, defaults) = STATE.iter().find(|(p, _, _)| *p == program)?;
    let has_run = match set(name) {
        Some(dir) => Path::new(dir).exists(),
        None => home.is_some_and(|home| defaults.iter().any(|d| home.join(d).exists())),
    };
    (!has_run).then_some(Ended::NeverRun)
}

/// Run `request`'s warm with its stdin held `hold` and the whole of it bounded by
/// `deadline`, under the `inherited` environment from `home`, and log it — or nothing at
/// all when the build is no longer the active one (a later landing, a rollback: that
/// build's own warm, or none, is what counts). A warm [`declined`] is logged, not run.
fn warm(
    request: &Request,
    inherited: &[(OsString, OsString)],
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
    let argv = warm_argv(request.spec)?;
    let started = std::time::Instant::now();
    let ended = if let Some(declined) = declined(program, inherited, home) {
        declined
    } else {
        let keys = inherited.iter().map(|(key, _)| key.clone());
        match warm_command(&layout, &tool, argv, keys, home).spawn() {
            Ok(child) => drive(child, hold, deadline),
            Err(_) => Ended::SpawnFailed,
        }
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    log(&layout, program, request.build, &ended.words(), ms);
    Some(ended)
}

/// The warm's command: `<prefix>/bin/<tool> <argv…>` ([`crate::landing::shim_command`]),
/// every inherited AI variable among `inherited` removed but the user's own setup
/// ([`KEEP`]), [`CHILD_ENV`] set, run from `home` (else the prefix), stdin a pipe the warm
/// never writes, stdout and stderr `/dev/null`, in a process group of its own so the
/// deadline can kill all of it.
fn warm_command(
    layout: &Layout,
    tool: &ToolName,
    argv: &[&str],
    inherited: impl IntoIterator<Item = OsString>,
    home: Option<&Path>,
) -> std::process::Command {
    let args: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
    let mut command = crate::landing::shim_command(layout, tool, &args);
    for key in inherited {
        if key
            .to_str()
            .is_some_and(|k| aterm_types::domain::is_ai_env_var(k) && !KEEP.contains(&k))
        {
            command.env_remove(key);
        }
    }
    for (name, value) in CHILD_ENV {
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
            // It cannot be watched: end it now rather than leave it unbounded.
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
    // The group is the child's own (`process_group(0)`), not yet reaped here.
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

    /// A scratch store, removed when the test ends however it ends.
    struct Scratch(Layout);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.prefix);
        }
    }

    fn scratch(label: &str) -> Scratch {
        Scratch(crate::vendor_direct::lane::world::layout(&format!(
            "warm-{label}"
        )))
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
    fn appears(path: &Path) -> bool {
        (0..1200).any(|_| {
            let there = path.exists();
            if !there {
                std::thread::sleep(Duration::from_millis(50));
            }
            there
        })
    }

    #[test]
    fn the_hidden_verb_keeps_its_name() {
        assert_eq!(HIDDEN_VERB, "__warm");
        assert!(HOLD < DEADLINE);
    }

    /// THE GATES: no warm under an unpinned manager, from a development `dir:` registry,
    /// over a store that is not the configured one, or as root; the negative control warms.
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

    /// THE LAUNCHES [`spawn_landed`] starts: `<atpkg> __warm <program> <prefix> <build>`
    /// once per landed, planned build — none when not admitted or with no executable, and
    /// none again for a build this process already started.
    #[test]
    fn a_landed_build_is_launched_once_through_atpkg_only_when_admitted() {
        let scratch = scratch("launches");
        let layout = &scratch.0;
        // A program name no other test starts, so the per-process set is this test's own.
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

    /// THE EXECUTABLE is only ever one named `atpkg`: the co-located alias when it exists,
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

    /// THE PLAN: only a landed program with a warm, active, and not removed, held or
    /// dev-linked — each with the build that is active now.
    #[test]
    fn the_plan_skips_removed_held_linked_inactive_and_unwarmed_programs() {
        let scratch = scratch("plan");
        let layout = &scratch.0;
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
        // A row with no warm is never warmed.
        let claude = crate::vendor_direct::spec("claude").unwrap();
        let silent = VendorSpec {
            warm: &[],
            ..*claude
        };
        assert_eq!(warm_argv(&silent), None);
        assert_eq!(warm_argv(claude), Some(claude.warm));
    }

    #[test]
    fn a_build_is_warmed_once_per_process() {
        assert!(first_in_this_process("warm-once-test", 7));
        assert!(!first_in_this_process("warm-once-test", 7));
        assert!(
            first_in_this_process("warm-once-test", 8),
            "a new build warms"
        );
    }

    /// THE OPERANDS: exactly `<program> <absolute prefix> <build>`, for a warmed program;
    /// and they are what [`spawn_landed`] passes.
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

    /// THE COMMAND: the `bin/` shim with the table's argv verbatim, every inherited AI
    /// variable removed but the user's own setup ([`KEEP`]) and nothing else, the MCP
    /// connectors off, run from home.
    #[test]
    fn the_command_runs_the_bin_shim_with_the_table_argv_and_no_ai_variables() {
        let layout = Layout {
            prefix: PathBuf::from(abs_prefix()),
        };
        let claude = crate::vendor_direct::spec("claude").unwrap();
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
        let command = warm_command(&layout, &tool, claude.warm, inherited, Some(home));
        assert_eq!(command.get_program(), layout.shim(&tool).as_os_str());
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(args, claude.warm);
        assert_eq!(command.get_current_dir(), Some(home));
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let mut expected: Vec<(String, Option<String>)> = [
            "AI_AGENT",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_SIMPLE",
        ]
        .iter()
        .map(|k| ((*k).to_string(), None))
        .collect();
        expected.push((
            "ENABLE_CLAUDEAI_MCP_SERVERS".to_string(),
            Some("false".to_string()),
        ));
        expected.sort();
        assert_eq!(
            envs, expected,
            "the config homes, the endpoint choice, PATH, HOME and LANG are inherited"
        );
        // With no home, the prefix.
        let command = warm_command(&layout, &tool, claude.warm, [], None);
        assert_eq!(command.get_current_dir(), Some(layout.prefix.as_path()));
    }

    /// THE USER'S CHOICE: claude is not warmed when the user turned its non-essential
    /// traffic off, nor is a program with no state on this account (never launched) —
    /// whose state is found where the user's own config variable puts it.
    #[test]
    fn an_opted_out_or_never_run_program_is_declined() {
        let scratch = scratch("declined");
        let home = scratch.0.prefix.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            pairs
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect()
        };
        let none = env(&[]);
        let h = Some(home.as_path());
        assert_eq!(declined("claude", &none, h), Some(Ended::NeverRun));
        assert_eq!(declined("codex", &none, h), Some(Ended::NeverRun));
        assert_eq!(declined("claude", &none, None), Some(Ended::NeverRun));
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        assert_eq!(declined("claude", &none, h), None, "the negative control");
        assert_eq!(declined("codex", &none, h), None);
        std::fs::remove_file(home.join(".claude.json")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        assert_eq!(declined("claude", &none, h), None, "~/.claude alone");
        // The user's own config homes decide, not the defaults.
        let elsewhere = scratch.0.prefix.join("elsewhere");
        let moved = env(&[
            ("CLAUDE_CONFIG_DIR", elsewhere.to_str().unwrap()),
            ("CODEX_HOME", elsewhere.to_str().unwrap()),
        ]);
        assert_eq!(declined("claude", &moved, h), Some(Ended::NeverRun));
        assert_eq!(declined("codex", &moved, h), Some(Ended::NeverRun));
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(declined("claude", &moved, h), None);
        assert_eq!(declined("codex", &moved, h), None);
        // The opt-out, for claude only; an empty value is no opt-out.
        let off = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")]);
        assert_eq!(declined("claude", &off, h), Some(Ended::OptedOut));
        assert_eq!(declined("codex", &off, h), None);
        let empty = env(&[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "")]);
        assert_eq!(declined("claude", &empty, h), None);
        // A program with no state row is always warmed.
        assert_eq!(declined("ay", &none, None), None);
    }

    /// THE BOUNDS, in-process with short durations (the binary has no knob for them): a
    /// program that exits on stdin EOF ends once the hold closes it, with its own code; one
    /// that ignores EOF and SIGTERM is killed with its whole group at the deadline. Each
    /// clock starts once the script is known to be running, so a slow first exec of a
    /// freshly written script (the system's code-policy check) is never charged to it.
    #[cfg(unix)]
    #[test]
    fn the_warm_closes_stdin_at_the_hold_and_kills_the_group_at_the_deadline() {
        let scratch = scratch("drive");
        let layout = &scratch.0;
        let marks = layout.prefix.join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        let tool = ToolName::new("claude").unwrap();
        let spec = crate::vendor_direct::spec("claude").unwrap();
        let spawn = || {
            warm_command(layout, &tool, spec.warm, [], None)
                .spawn()
                .unwrap()
        };
        // Reads stdin to EOF, then exits 3.
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
        let t = std::time::Instant::now();
        assert_eq!(
            drive(child, Duration::from_millis(300), Duration::from_secs(120)),
            Ended::Exit(3)
        );
        assert!(t.elapsed() >= Duration::from_millis(300), "stdin held");
        assert!(marks.join("eof").exists());
        // Ignores EOF and SIGTERM, and leaves a grandchild in its group.
        install(
            layout,
            "claude",
            2_001_281,
            &format!(
                "#!/bin/sh\ntrap '' TERM\nsleep 60 &\necho $! > '{}/grandchild.tmp'\n\
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
        let gone = (0..500).any(|_| {
            // SAFETY: signal 0 only asks whether the pid is addressable.
            let alive = unsafe { libc::kill(grandchild, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(20));
            }
            !alive
        });
        assert!(gone, "the group went with it");
    }

    /// THE RECORD: each warm that ran or was declined is one `warm` line — its own exit
    /// code, `killed`, `spawn-failed`, `opted-out`, `never-run` — and a stale build runs
    /// nothing and logs nothing. No outcome here depends on how fast a script starts.
    #[cfg(unix)]
    #[test]
    fn each_warm_is_one_log_line_and_a_stale_build_is_none() {
        let scratch = scratch("record");
        let layout = &scratch.0;
        let logs = layout.prefix.join("logs");
        let _bound = crate::packages_log::test_bind::bind(logs.clone(), layout.prefix.clone());
        // The account has run claude: its state is under this home.
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
            warm(&request(build), env, Some(home), Duration::ZERO, deadline)
        };
        // Exits 3 once stdin closes (at once: no hold), however long its exec takes.
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
        // Never ends: killed at the deadline, started or not by then.
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
        // A stale build: nothing runs, nothing is logged.
        let before = crate::packages_log::read_tail(&logs, 100).len();
        assert_eq!(run(2_001_280, &no_env, &home, Duration::from_secs(1)), None);
        assert_eq!(crate::packages_log::read_tail(&logs, 100).len(), before);
        // Declined: the opt-out, then a home claude never ran in.
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
        // Unstartable: the `bin/` shim is not executable (the build stays active).
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
        // One line per warm that ran or was declined, newest first.
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
