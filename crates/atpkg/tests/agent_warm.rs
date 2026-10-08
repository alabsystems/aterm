// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WARM OF A LANDED AGENT BUILD at the REAL process edge (2026-09-23, `atpkg::warm`):
//! the dev `atpkg` binary driven as `__warm claude <prefix> <build>`, the way a pass that
//! landed that build starts it, over a temp store whose `claude` is a fake that records
//! its argv, its environment, its working directory and whether its stdin is a terminal.
//!
//! What is pinned here and nowhere else: the verb says NOTHING and exits 0 whatever the
//! program does; it runs the `bin/` shim, so the shim's `DISABLE_AUTOUPDATER=1` reaches the
//! program, with the table's argv verbatim, from HOME, on a stdin that is no terminal, with
//! no variable of a parent Claude session but with the user's own config home; a stale
//! build, an opted-out user and an account that never ran claude run nothing; and each warm
//! that ran or was declined is one `warm` line in the package log. The deadline (30 s, no
//! knob to shorten it) is pinned in-process by `warm.rs`'s own test, which injects short
//! bounds.
//!
//! NEVER THE REAL STORE OR THE REAL LOG: HOME, the config dir and the state dir are temp
//! directories, and the store is the default prefix under that HOME.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, within};

/// AI variables that must not reach the warm, set on the verb's own environment: a parent
/// Claude session's markers (a session exports the first four — measured in one,
/// 2026-09-23), the switch that would turn claude's warm into `--bare`, and an API key.
const SESSION_VARS: &[(&str, &str)] = &[
    ("CLAUDECODE", "1"),
    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
    ("CLAUDE_CODE_SESSION_ID", "s-parent"),
    ("AI_AGENT", "claude-code"),
    ("CLAUDE_CODE_SIMPLE", "1"),
    ("ANTHROPIC_API_KEY", "not-a-key"),
];

/// Variables of the runner's own environment the verb must not inherit, so that what the
/// program records can only have come from the shim and the warm: the two the warm and the
/// shim set are given a sentinel instead, and the user's own claude settings are removed.
const SENTINELS: &[(&str, &str)] = &[
    ("DISABLE_AUTOUPDATER", "0"),
    ("ENABLE_CLAUDEAI_MCP_SERVERS", "true"),
];
const UNSET: &[&str] = &[
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "DISABLE_TELEMETRY",
];

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    prefix: PathBuf,
    record: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-agent-warm-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        // The account has run claude before: there are caches to refresh.
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        let fx = Self {
            record: root.path().join("record"),
            root,
            home,
            prefix,
        };
        fx.layout().ensure_dir(&fx.prefix).unwrap();
        fx
    }

    fn layout(&self) -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: self.prefix.clone(),
        }
    }

    /// A managed `claude` at `build`, laid the real way — its `bin/` shim exporting the
    /// table's `shim_env` as the vendor lane lays it — whose store executable records what
    /// it was run with into [`Fixture::record`] and exits `code`.
    fn install_claude(&self, build: u64, code: u8) {
        let layout = self.layout();
        let dir = layout.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin/claude");
        let r = self.record.display();
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nif [ \"${{1-}}\" = --fixture-ready ]; then echo fixture-ready; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > '{r}.args'\nenv > '{r}.env'\npwd > '{r}.pwd'\n\
                 if [ -t 0 ]; then echo tty; else echo notty; fi > '{r}.stdin'\n\
                 echo out; echo err >&2\nexit {code}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &["claude".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        let tool = atpkg::store::ToolName::new("claude").unwrap();
        let spec = atpkg::vendor_direct::spec("claude").unwrap();
        atpkg::platform::install_shim_env(
            &dir.join("bin"),
            &tool,
            &layout.shim(&tool),
            &spec.shim_env(),
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        assert_eq!(
            atpkg::ops::active_builds(&layout).get("claude"),
            Some(&build),
            "fixture: the build is active"
        );
        // Establish that each fresh executable can enter before the real warm starts
        // its unchanged 30 s product deadline. Prepare the stand-in first, then its real
        // shim, so two macOS first-exec assessments do not share one setup hang detector.
        // The readiness branch records nothing: never-runs cases still detect a warm.
        for ready_exe in [exe, layout.shim(&tool)] {
            let mut ready = Command::new(&ready_exe)
                .arg("--fixture-ready")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start the fixture readiness check");
            let ended = within(MUST_HAPPEN, Duration::from_millis(10), || {
                ready.try_wait().expect("observe fixture readiness")
            });
            if ended.is_none() {
                let _ = ready.kill();
                let _ = ready.wait();
                panic!(
                    "fixture {} did not become ready within {MUST_HAPPEN:?}",
                    ready_exe.display()
                );
            }
            let out = ready.wait_with_output().unwrap();
            assert!(out.status.success(), "fixture readiness: {out:?}");
            assert_eq!(out.stdout, b"fixture-ready\n", "fixture readiness: {out:?}");
            assert!(out.stderr.is_empty(), "fixture readiness: {out:?}");
        }
        for kind in ["args", "env", "pwd", "stdin"] {
            assert!(self.recorded(kind).is_none(), "setup recorded {kind}");
        }
        assert!(self.warm_lines().is_empty(), "setup logged a warm");
    }

    /// `atpkg __warm <operands…>`, confined to this fixture, with a parent Claude
    /// session's variables and `extra` in its environment — an environment born in the
    /// person's shell (the rc hook's `ATPKG_BIN` set), as a typed pass's is.
    fn warm_with(&self, operands: &[&str], extra: &[(&str, &str)]) -> Output {
        self.command(operands, extra)
            .output()
            .expect("run the dev atpkg")
    }

    /// [`Fixture::warm_with`]'s command, not yet run.
    fn command(&self, operands: &[&str], extra: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg(atpkg::warm::HIDDEN_VERB)
            .args(operands)
            .env("HOME", &self.home)
            .env("ATPKG_BIN", self.prefix.join("bin"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env_remove("ATERM_STATE_HOME")
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            // A zsh the warm asks reads its rc under `$ZDOTDIR`, never this HOME's.
            .env_remove("ZDOTDIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in UNSET {
            cmd.env_remove(name);
        }
        for (k, v) in SESSION_VARS.iter().chain(SENTINELS).chain(extra) {
            cmd.env(k, v);
        }
        cmd
    }

    fn warm(&self, operands: &[&str]) -> Output {
        self.warm_with(operands, &[])
    }

    fn warm_build(&self, build: u64) -> Output {
        self.warm_build_with(build, &[])
    }

    fn warm_build_with(&self, build: u64, extra: &[(&str, &str)]) -> Output {
        let prefix = self.prefix.to_str().unwrap().to_string();
        self.warm_with(&["claude", &prefix, &build.to_string()], extra)
    }

    fn recorded(&self, what: &str) -> Option<String> {
        std::fs::read_to_string(format!("{}.{what}", self.record.display())).ok()
    }

    /// The `warm` lines of the package log under this HOME, oldest first.
    fn warm_lines(&self) -> Vec<atpkg::packages_log::Entry> {
        let log = aterm_types::dirs::resolve_logs_dir(
            None,
            aterm_types::dirs::StatePlatform {
                home: Some(self.home.clone()),
                xdg_state_home: Some(self.root.path().join("state")),
                local_app_data: None,
            },
        )
        .unwrap()
        .join(atpkg::packages_log::LOG_NAME);
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|l| atpkg::packages_log::parse_line(l).unwrap_or_else(|| panic!("{l:?}")))
            .filter(|e| e.kind == atpkg::packages_log::kind::WARM)
            .collect()
    }
}

/// Exit 0 with not one byte on either pipe.
fn silent_success(out: &Output, label: &str) {
    assert!(out.status.success(), "{label}: {out:?}");
    assert!(out.stdout.is_empty(), "{label}: stdout {:?}", out.stdout);
    assert!(out.stderr.is_empty(), "{label}: stderr {:?}", out.stderr);
}

#[test]
fn the_warm_runs_the_bin_shim_silently_with_the_table_argv_and_no_session_variables() {
    let fx = Fixture::new("runs");
    let build = atpkg::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    fx.install_claude(build, 0);
    let out = fx.warm_build(build);
    silent_success(&out, "a warm");
    let spec = atpkg::vendor_direct::spec("claude").unwrap();
    let args: Vec<String> = fx
        .recorded("args")
        .expect("the program ran")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(args, spec.warm, "the table's argv, verbatim");
    let env = fx.recorded("env").unwrap();
    let vars: Vec<&str> = env.lines().collect();
    // Over the sentinels the verb was given: only the shim and the warm set these.
    assert!(
        vars.contains(&"DISABLE_AUTOUPDATER=1"),
        "the shim's own environment: {env}"
    );
    assert!(vars.contains(&"ENABLE_CLAUDEAI_MCP_SERVERS=false"), "{env}");
    for (name, _) in SESSION_VARS {
        assert!(
            !vars.iter().any(|v| v.starts_with(&format!("{name}="))),
            "{name} reached the warm: {env}"
        );
    }
    assert_eq!(
        fx.recorded("pwd").unwrap().trim(),
        fx.home.to_str().unwrap(),
        "run from HOME"
    );
    assert_eq!(fx.recorded("stdin").unwrap().trim(), "notty");
    let lines = fx.warm_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].get("program"), Some("claude"));
    assert_eq!(
        lines[0].get("build"),
        Some("2.1.280"),
        "the version, as updates name it"
    );
    assert_eq!(lines[0].get("outcome"), Some("exit=0"));
    assert!(lines[0].get("ms").is_some());
}

#[test]
fn a_failing_program_fails_nothing() {
    let fx = Fixture::new("fails");
    fx.install_claude(2_001_280, 5);
    let out = fx.warm_build(2_001_280);
    silent_success(&out, "a failing warm");
    assert!(fx.recorded("args").is_some(), "the program ran");
    let lines = fx.warm_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].get("outcome"), Some("exit=5"));
}

/// A build that is no longer the active one (a later landing, a rollback) runs nothing and
/// logs nothing; neither does anything but exactly `<warmed program> <absolute prefix>
/// <build>` — and every one of those endings is silent and exits 0.
#[test]
fn a_stale_build_or_a_malformed_request_runs_nothing() {
    let fx = Fixture::new("stale");
    fx.install_claude(2_001_281, 0);
    let prefix = fx.prefix.to_str().unwrap().to_string();
    for (label, operands) in [
        ("stale", vec!["claude", prefix.as_str(), "2001280"]),
        ("no operands", vec![]),
        ("no build", vec!["claude", prefix.as_str()]),
        ("relative prefix", vec!["claude", "home/pkg", "2001281"]),
        ("not a build", vec!["claude", prefix.as_str(), "latest"]),
        ("not warmed", vec!["ay", prefix.as_str(), "2001281"]),
        ("help", vec!["--help"]),
    ] {
        let out = fx.warm(&operands);
        silent_success(&out, label);
        assert!(fx.recorded("args").is_none(), "{label}: nothing ran");
    }
    assert!(fx.warm_lines().is_empty(), "nothing logged");
    // The negative control.
    silent_success(&fx.warm_build(2_001_281), "current");
    assert!(fx.recorded("args").is_some());
    assert_eq!(fx.warm_lines().len(), 1);
}

/// The user's own config home reaches the warm (it is where the caches the warm refreshes
/// live), and is where the warm looks for claude's state.
///
/// AND KEPT ONLY IN THE PERSON'S SHELL RC, warmed from the window (no `ATPKG_BIN`; review
/// of the 2026-10-02 fix): the config home and a variable the program honours itself
/// (`DISABLE_TELEMETRY`, the reason [`atpkg::warm`] leaves it out of its opt-outs) reach
/// the program from their login shell, as they reach their next `claude`.
#[test]
fn the_users_own_config_home_reaches_the_warm() {
    let fx = Fixture::new("config-home");
    fx.install_claude(2_001_280, 0);
    let config = fx.root.path().join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    let config = config.to_str().unwrap();
    silent_success(
        &fx.warm_build_with(2_001_280, &[("CLAUDE_CONFIG_DIR", config)]),
        "a warm",
    );
    let env = fx.recorded("env").expect("the program ran");
    assert!(
        env.lines()
            .any(|v| v == format!("CLAUDE_CONFIG_DIR={config}")),
        "{env}"
    );
    std::fs::remove_file(format!("{}.env", fx.record.display())).unwrap();
    std::fs::write(
        fx.home.join(".bash_profile"),
        format!("export CLAUDE_CONFIG_DIR='{config}' DISABLE_TELEMETRY=1\n"),
    )
    .unwrap();
    let prefix = fx.prefix.to_str().unwrap().to_string();
    let out = fx
        .command(&["claude", &prefix, "2001280"], &[("SHELL", "/bin/bash")])
        .env_remove("ATPKG_BIN")
        .output()
        .expect("run the dev atpkg");
    silent_success(&out, "kept in the rc, warmed from the window");
    let env = fx.recorded("env").expect("the program ran");
    for want in [
        format!("CLAUDE_CONFIG_DIR={config}"),
        String::from("DISABLE_TELEMETRY=1"),
    ] {
        assert!(env.lines().any(|v| v == want), "{want}: {env}");
    }
}

/// The user's opt-out of claude's non-essential traffic, and an account that never ran
/// claude (a default-set install nobody launched): nothing runs, silently, and the warm's
/// line says why.
///
/// AND THE OPT-OUT KEPT IN THE PERSON'S SHELL RC (audit 2026-10-02): most builds land from
/// the window's head watch, and a window started from the Dock carries launchd's
/// environment — no `ATPKG_BIN`, nothing the person's rc exports — so the warm read no
/// opt-out there and ran claude anyway. It now reads the person's login shell, the way
/// their next `claude` gets its environment.
#[test]
fn an_opted_out_or_never_run_claude_is_not_run() {
    let fx = Fixture::new("declined");
    fx.install_claude(2_001_280, 0);
    silent_success(
        &fx.warm_build_with(
            2_001_280,
            &[("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")],
        ),
        "opted out",
    );
    assert!(fx.recorded("args").is_none(), "opted out: nothing ran");
    let config = fx.root.path().join("never-ran");
    silent_success(
        &fx.warm_build_with(
            2_001_280,
            &[("CLAUDE_CONFIG_DIR", config.to_str().unwrap())],
        ),
        "never ran",
    );
    assert!(fx.recorded("args").is_none(), "never ran: nothing ran");
    std::fs::write(
        fx.home.join(".bash_profile"),
        "export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\n",
    )
    .unwrap();
    let prefix = fx.prefix.to_str().unwrap().to_string();
    let out = fx
        .command(&["claude", &prefix, "2001280"], &[("SHELL", "/bin/bash")])
        .env_remove("ATPKG_BIN")
        .output()
        .expect("run the dev atpkg");
    silent_success(&out, "opted out in the rc, warmed from the window");
    assert!(
        fx.recorded("args").is_none(),
        "opted out in the rc: nothing ran"
    );
    // AND AN RC THE WARM COULD NOT READ (audit round 2, 2026-10-02): unread is not unset.
    // Each shape gives the person's own terminal the opt-out (tmux and the exec'd shell
    // inherit the export, ksh reads `.profile`) and the warm's probe no environment of
    // theirs: an rc that auto-starts tmux, which refuses with no terminal (exit 1); one that
    // greets and execs another shell, which reads the empty stdin and exits 0 having
    // printed only the greeting; a `$SHELL` the warm does not ask; no `$SHELL` at all.
    // Before, claude ran in every one (`exit=0`). An opt-out already in hand says so.
    let tmux = fx.root.path().join("tmux");
    std::fs::write(
        &tmux,
        "#!/bin/sh\necho 'open terminal failed: not a terminal' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let opt_out = "export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\n";
    std::fs::write(fx.home.join(".profile"), opt_out).unwrap();
    let in_hand = [("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")];
    for (label, shell, rc_tail, extra) in [
        (
            "tmux",
            Some("/bin/bash"),
            format!("exec '{}'\n", tmux.display()),
            &[][..],
        ),
        (
            "another shell",
            Some("/bin/bash"),
            String::from("echo \"Welcome, user=$USER\"\nexec /bin/sh\n"),
            &[],
        ),
        ("ksh", Some("/bin/ksh"), String::new(), &[]),
        ("no $SHELL", None, String::new(), &[]),
        ("in hand", Some("/bin/ksh"), String::new(), &in_hand),
    ] {
        std::fs::write(fx.home.join(".bash_profile"), format!("{opt_out}{rc_tail}")).unwrap();
        let mut command = fx.command(&["claude", &prefix, "2001280"], extra);
        command.env_remove("ATPKG_BIN").env_remove("SHELL");
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        silent_success(&command.output().expect("run the dev atpkg"), label);
        assert!(fx.recorded("args").is_none(), "{label}: nothing ran");
    }
    let outcomes: Vec<String> = fx
        .warm_lines()
        .iter()
        .map(|l| l.get("outcome").unwrap().to_string())
        .collect();
    assert_eq!(
        outcomes,
        [
            "opted-out",
            "never-run",
            "opted-out",
            "setup-unread",
            "setup-unread",
            "setup-unread",
            "setup-unread",
            "opted-out"
        ]
    );
}
