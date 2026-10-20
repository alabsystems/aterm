// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab-proof [--package DIR] [--aterm PATH] [--keep]`: the codex-alab
//! native harness, end to end (its contract, `alab/CONTRACT.md` §9.3), against
//! the REAL package in a private headless aterm sandboxed to one loopback port
//! where the lab answers as the model ([`crate::codex_rig`]): no key, no
//! account, no byte off the machine, nothing of the owner's touched.
//!
//! The package is linked as the owner links it (`aterm pkg link codex <pkg>
//! bin/codex` under the scratch HOME), so a tab's `codex` is atpkg's twin, and
//! every leg is a tab of ONE instance whose `[harness]` is on (headless,
//! `human_grace_s = 5`). Each leg's prompt names it (`LEG-<name>`), and the
//! fake model answers by that name ([`answer`]), as the scenario kit of the
//! codex-alab end-to-end run did (its `continue`, `approval`, `usage-limit`,
//! question and crash scenarios, ported step for step):
//!
//! * `first` (the instance's FIRST session): the claim is proven
//!   (`supervisor_proven=1`) and aterm's lane parks (its host only watches);
//!   one reply ending `ATERM-TASK-DONE` is the done stop (`finished@v1`, no
//!   continuation). Then the crash rule: `SIGKILL` → relaunched on its
//!   conversation and proven again; the relaunched one `SIGKILL`ed as soon as
//!   its claim is proven (before its record may have been written) →
//!   relaunched again.
//! * `continue`: a turn that ends without the done mark gets `continue@v1`
//!   (the `continue_text`, `keep going`); the next reply's done mark stops it.
//!   Later the re-exec upgrade: the package re-linked to a copy of itself
//!   (another path, the same bytes) moves the running codex-alab onto it
//!   (`upgrade@v1`, its record's `execGeneration` up and its `exe` the copy's),
//!   proven again.
//! * `approval`: an escalated `exec_command` under `on-request` is approved
//!   once (`approve-all@v1`) and runs.
//! * `question`: a `request_user_input` overlay is answered with its
//!   `(Recommended)` option (`answer-recommended@v1`).
//! * `usage`: the model's 429 `usage_limit_reached` is waited out: nothing is
//!   sent before `resets_at` + 60 s, then `usage-resume@v1` continues.
//! * `hold`: a hold put on the tab while its first turn runs keeps the claim
//!   and stops the continuation while it stands; lifted, `continue@v1`.
//! * `carry` (codex-alab's carry-on, CONTRACT §5): its first turn is held by
//!   the model and the codex-alab `SIGKILL`ed mid-turn (its record `busy`,
//!   its resume line in aterm's copy). The crash rule relaunches it under
//!   aterm's relaunch lease, held to the relaunched process's first idle (its
//!   ledger's `done:resumed` row says so, never its bound), so codex-alab's
//!   launch probe (its first `@self status`) sees
//!   `hand=lease:aterm-harness@<aterm>` and the relaunched process carries
//!   the interrupted turn on by itself, once (`keep going`, `harness-turn@v1`
//!   by the relaunched pid), then `finished@v1`. A lease given back as the
//!   new TUI leads the tab (aterm 0.106.0) is read as `hand=-`: no carry-on,
//!   and the conversation waits at its prompt.
//! * all seven at once: five and more codex-alab sessions in one aterm each
//!   hold a proven claim (each keeps one `subscribe @self` stream).
//! * `question`, once finished, then Autopilot Off (`meta set autopilot off`,
//!   the chip's and Settings' write): codex-alab gives its claim back, and a
//!   manager tab's `post kind=task` puts the mail ALONE over the lab's own bus
//!   (a guarded broker the lab starts, the instance's `[fabric]` bridge on
//!   it): the task in the worker's inbox, no turn on its ledger and no `Inbox:
//!   task @` on its screen. Autopilot back On (`cruise`): codex-alab claims
//!   again.
//!
//! Exit 0 every check held, 1 a check failed, 2 usage, 3 not run (no
//! codex-alab package: `--package`, `$LAB_CODEX`, or a managed `codex` that is
//! an ALab build; no aterm). About three minutes before the Autopilot Off step (measured: `docs/measured/codex-alab-proof-2026-10-16.md`), which adds under one; run it alone.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::codex_rig::{self, Answered, call_item, message_item, request_view};
use crate::instance::{self, Instance, Launch, World};
use crate::pyjson::J;
use crate::screen::field;
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// The done mark a turn ends a task with.
const DONE: &str = "ATERM-TASK-DONE";

/// `[harness]`'s continuation text, as aterm's default spells it.
const KEEP_GOING: &str = "keep going";

/// How long the usage leg's 429 says the window lasts.
const RESETS_IN_S: i64 = 5;

/// How long the hold leg's first answer is held: the hold lands meanwhile.
const HOLD_FIRST_TURN: Duration = Duration::from_secs(30);

/// How long the carry leg's first answer is held: never before the kill that
/// ends that turn (the leg kills it as soon as it is relaunchable).
const CARRY_FIRST_TURN: Duration = Duration::from_secs(600);

/// The fleet the lab's own bus names (`/f/lab/…`).
const FLEET: &str = "lab";

/// The one-line nudge the removed `aterm drive task` typed into a worker
/// (`DESIGN-sessions-as-subagents-2026-10-15.md` §7): nothing types it now,
/// so the Autopilot Off step holds the screen and the ledger to its absence.
const NUDGE: &str = "Inbox: task @";

/// The proof's own bus (the Autopilot Off step's mail): a guarded broker the
/// lab starts outside the jail on a Unix socket in the scratch, under a mint
/// secret only the scratch holds, and the node ring the instance's bridge
/// attaches with (THE ring `aterm fabric on` mints,
/// `aterm_link::enable::node_grants`). The instance's `[fabric] command`
/// names the bridge, so it launches it as it launches any configured one; the
/// broker ends with the proof (its pid, the lab's own child).
struct Fabric {
    broker: Option<aterm_uds::spawnfd::Child>,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    table: String,
}

impl Fabric {
    /// Lay the bus out under `dir` and start its broker with `aterm`'s own
    /// `link broker`.
    fn start(aterm: &Path, dir: &Path) -> Result<Fabric, String> {
        use std::os::unix::fs::PermissionsExt as _;
        let state = dir.join("state");
        std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
        for d in [dir, state.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let mut random = [0u8; 40];
        {
            use std::io::Read as _;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| format!("/dev/urandom: {e}"))?;
        }
        let secret = dir.join("mint.secret");
        let write_private = |path: &Path, bytes: &[u8]| {
            std::fs::write(path, bytes)
                .and_then(|()| {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                })
                .map_err(|e| format!("{}: {e}", path.display()))
        };
        write_private(&secret, &random[..32])?;
        let node = format!(
            "n-{}",
            random[32..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        write_private(&state.join("node"), format!("{node}\n").as_bytes())?;
        // The program's own spelling, linked where no word of it can hold a
        // space: the command below is written as a LINE, which is split on
        // whitespace (an argv array would carry the space instead).
        let program = dir.join("aterm");
        std::os::unix::fs::symlink(aterm, &program)
            .map_err(|e| format!("{}: {e}", program.display()))?;
        let mut caps = String::new();
        for grant in aterm_link::enable::node_grants(FLEET, &node) {
            let out = aterm_uds::spawnfd::command(&program)
                .args(["link", "mint", &grant, "--secret-file"])
                .arg(&secret)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("aterm link mint: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "aterm link mint {grant}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            caps.push_str(&String::from_utf8_lossy(&out.stdout));
        }
        let cap = dir.join("node.cap");
        write_private(&cap, caps.as_bytes())?;
        let sock = dir.join("b.sock");
        let log = std::fs::File::create(dir.join("broker.out"))
            .map_err(|e| format!("broker.out: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("broker.out: {e}"))?;
        let broker = aterm_uds::spawnfd::command(&program)
            .args(["link", "broker"])
            .arg(&sock)
            .arg(dir.join("b.log"))
            .arg("--secret-file")
            .arg(&secret)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("aterm link broker: {e}"))?;
        let mut fabric = Fabric {
            broker: Some(broker),
            table: format!(
                "[fabric]\ncommand = \"{program} link serve --fleet {FLEET} --broker {sock} \
                 --cap-file {cap} --state {state} --accept-from {node}\"\n",
                program = program.display(),
                sock = sock.display(),
                cap = cap.display(),
                state = state.display(),
            ),
        };
        // A hang detector, never a budget: the broker listens in milliseconds.
        let end = Instant::now() + Duration::from_secs(60);
        while !sock.exists() {
            if Instant::now() >= end
                || fabric
                    .broker
                    .as_mut()
                    .is_some_and(|b| matches!(b.try_wait(), Ok(Some(_))))
            {
                fabric.stop();
                let said = std::fs::read_to_string(dir.join("broker.out")).unwrap_or_default();
                return Err(format!("the lab's broker never listened: {}", said.trim()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(fabric)
    }

    /// End the broker (`SIGTERM`, then `SIGKILL`): only the pid the lab started.
    fn stop(&mut self) {
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        // Only a positively unreaped child still owns its numeric pid.
        if !matches!(broker.try_wait(), Ok(None)) {
            return;
        }
        if let Ok(pid) = i32::try_from(broker.id()) {
            let _ = crate::sys::kill(pid, libc::SIGTERM);
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if !matches!(broker.try_wait(), Ok(None)) {
                    return;
                }
                if Instant::now() >= end {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // `try_wait` may have reaped it during the grace; `kill` keeps that status.
        let _ = broker.kill();
        let _ = broker.wait();
    }
}

impl Drop for Fabric {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The leg a request belongs to: the `LEG-<name>` its user text names.
fn leg_of(body: &J) -> Option<String> {
    let input = body.get("input").and_then(J::as_arr).unwrap_or_default();
    input
        .iter()
        .filter(|it| it.get("role").and_then(J::as_str) == Some("user"))
        .map(crate::stub::item_text)
        .find_map(|text| {
            let (_, rest) = text.split_once("LEG-")?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .collect();
            (!name.is_empty()).then_some(name)
        })
}

/// The usage leg's window: its reset, fixed at its first request.
static RESETS: Mutex<Vec<(String, i64)>> = Mutex::new(Vec::new());

fn reset_of(leg: &str) -> i64 {
    let mut resets = RESETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, at)) = resets.iter().find(|(l, _)| l == leg) {
        return *at;
    }
    let at = crate::util::now_secs() + RESETS_IN_S;
    resets.push((leg.to_string(), at));
    at
}

/// What the fake model answers (module header). A structured side request
/// (the thread's title) is answered with JSON that fits its schema.
#[must_use]
pub fn answer(body: &J) -> Answered {
    let leg = leg_of(body).unwrap_or_else(|| "-".to_string());
    let tagged = |mut a: Answered, step: &str| {
        a.tag = format!("{leg}:{step}");
        a
    };
    let say = |text: &str| Answered::items(vec![message_item(text)]);
    let format = body.get("text").and_then(|t| t.get("format"));
    if format.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        let schema = format.and_then(|f| f.get("schema"));
        return tagged(say(&crate::stub::schema_answer(schema)), "side");
    }
    let (user, tool) = request_view(body);
    let asked_on = user.contains(KEEP_GOING);
    match leg.as_str() {
        "continue" | "hold" if asked_on => tagged(
            say(&format!("Continued as asked. All done.\n\n{DONE}")),
            "after-continue",
        ),
        "continue" | "hold" => {
            let mut a = say("Here is step one of the plan. I will continue when asked.");
            if leg == "hold" {
                a.delay = HOLD_FIRST_TURN;
            }
            tagged(a, "t1")
        }
        "carry" if asked_on => tagged(
            say(&format!(
                "Carried on after the relaunch. All done.\n\n{DONE}"
            )),
            "after-carry",
        ),
        "carry" => {
            let mut a = say("The long task's first step.");
            a.delay = CARRY_FIRST_TURN;
            tagged(a, "t1")
        }
        "approval" if tool.is_none() => tagged(
            Answered::items(vec![call_item(
                "exec_command",
                &J::obj([
                    ("cmd", J::s("echo approved-run >> ran.txt; echo done")),
                    ("sandbox_permissions", J::s("require_escalated")),
                    ("justification", J::s("write the marker file")),
                ]),
            )]),
            "call",
        ),
        "approval" => tagged(say(&format!("The command ran.\n\n{DONE}")), "after-output"),
        "question" if tool.is_none() => {
            let option = |label: &str, description: &str| {
                J::obj([("label", J::s(label)), ("description", J::s(description))])
            };
            tagged(
                Answered::items(vec![call_item(
                    "request_user_input",
                    &J::obj([(
                        "questions",
                        J::Arr(vec![J::obj([
                            ("id", J::s("db_choice")),
                            ("header", J::s("Database")),
                            ("question", J::s("Which database should I set up?")),
                            (
                                "options",
                                J::Arr(vec![
                                    option("SQLite", "A single local file."),
                                    option(
                                        "Postgres (Recommended)",
                                        "A server, closer to production.",
                                    ),
                                ]),
                            ),
                        ])]),
                    )]),
                )]),
                "ask",
            )
        }
        "question" => tagged(
            say(&format!("Setting up the chosen database.\n\n{DONE}")),
            "after-answer",
        ),
        "usage" if asked_on => tagged(
            say(&format!("Resumed after the reset.\n\n{DONE}")),
            "after-reset",
        ),
        "usage" => {
            let reset = reset_of(&leg);
            let a = Answered {
                status: 429,
                kind: "application/json",
                body: crate::stub::usage_limit_body("five_hour", reset, Some("plus")).py(),
                headers: crate::stub::usage_limit_headers("five_hour", reset),
                delay: Duration::ZERO,
                tag: String::new(),
            };
            tagged(a, &format!("limit resets_at={reset}"))
        }
        _ => tagged(
            say(&format!(
                "Hello from the lab model. Nothing else to do.\n\n{DONE}"
            )),
            "t1",
        ),
    }
}

/// A `codex-package.json` root that carries `alab-build.json`: the package
/// `given` names (a package directory, or a binary in one), else `$LAB_CODEX`,
/// else the owner's managed `codex` — the binary its atpkg shim execs (the
/// last path of `aterm pkg which codex`), since a dev-linked package's shim is
/// a script in `pkg/bin` that no package directory holds; the shim's own path
/// when atpkg names none.
///
/// # Errors
/// Why none is a codex-alab package (the verb's NOT RUN).
pub fn package(given: Option<&str>, home: &str) -> Result<PathBuf, String> {
    let (from, why) = match given {
        Some(g) => (PathBuf::from(crate::paths::from_caller(g)), "--package"),
        None => match std::env::var("LAB_CODEX") {
            Ok(v) if !v.is_empty() => (PathBuf::from(v), "$LAB_CODEX"),
            _ => (
                crate::lab::managed_store_bin("codex").map_or_else(
                    || Path::new(home).join("Library/Application Support/aterm/pkg/bin/codex"),
                    PathBuf::from,
                ),
                "the managed codex",
            ),
        },
    };
    let real = std::fs::canonicalize(&from).map_err(|_| {
        format!(
            "no codex-alab package ({why}: {} is not there; pass --package <dir>)",
            from.display()
        )
    })?;
    let root = real
        .ancestors()
        .take(4)
        .find(|d| {
            d.join(aterm_agent::harness::codex_alab::PACKAGE_JSON)
                .is_file()
        })
        .ok_or_else(|| format!("{why} ({}) is in no Codex package", real.display()))?
        .to_path_buf();
    let text = std::fs::read_to_string(root.join(aterm_agent::harness::codex_alab::PACKAGE_JSON))
        .unwrap_or_default();
    aterm_agent::harness::codex_alab::package_entrypoint(&text)
        .ok_or_else(|| format!("{} holds no layout-1 package manifest", root.display()))?;
    if !root
        .join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON)
        .is_file()
    {
        return Err(format!(
            "{why} ({}) is a vendor Codex, not a codex-alab package (no alab-build.json)",
            root.display()
        ));
    }
    Ok(root)
}

/// One leg's tab and files.
struct Leg {
    name: &'static str,
    sid: String,
    /// Its `CODEX_HOME`.
    home: PathBuf,
    proj: PathBuf,
}

struct Proof {
    inst: Instance,
    /// The scratch root and HOME.
    s: PathBuf,
    h: PathBuf,
    world_env: Vec<(String, String)>,
    api_log: PathBuf,
    port: u16,
    fails: Vec<String>,
    /// The bus the Autopilot Off step's mail rides.
    fabric: Fabric,
}

fn sleep(d: Duration) -> Result<(), Stopped> {
    stop::sleep(d)
}

impl Proof {
    fn check(&mut self, label: &str, held: bool) -> bool {
        if held {
            println!("  ok   {label}");
        } else {
            println!("  FAIL {label}");
            self.fails.push(label.to_string());
        }
        held
    }

    fn on(&self, sid: &str, args: &[&str]) -> instance::Reply {
        let at = format!("@{sid}");
        let mut v = vec![at.as_str()];
        v.extend_from_slice(args);
        self.inst.ctl(&v)
    }

    fn status(&self, sid: &str) -> String {
        self.on(sid, &["status"]).first()
    }

    /// `supervisor=codex-alab@<pid> supervisor_proven=1`: that pid.
    fn proven_pid(&self, sid: &str) -> Option<i32> {
        let st = self.status(sid);
        if field(&st, "supervisor_proven").as_deref() != Some("1") {
            return None;
        }
        field(&st, "supervisor")?
            .strip_prefix("codex-alab@")?
            .parse()
            .ok()
    }

    /// The `harness-act` acts of the session's timeline, in order.
    fn acts(&self, sid: &str) -> Vec<String> {
        self.on(sid, &["timeline"])
            .stdout
            .lines()
            .filter(|l| l.contains("kind=harness-act"))
            .filter_map(|l| field(l, "act"))
            .collect()
    }

    /// The fake model's log rows for `leg` (`t`, `tag`, `u`, `tool_output`, `status`).
    fn requests(&self, leg: &str) -> Vec<J> {
        let head = format!("{leg}:");
        std::fs::read_to_string(&self.api_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| J::parse(l).ok())
            .filter(|v| {
                v.get("tag")
                    .and_then(J::as_str)
                    .is_some_and(|t| t.starts_with(&head) && !t.ends_with(":side"))
            })
            .collect()
    }

    fn tags(&self, leg: &str) -> Vec<String> {
        self.requests(leg)
            .iter()
            .filter_map(|v| v.get("tag").and_then(J::as_str).map(str::to_string))
            .collect()
    }

    /// The record codex-alab keeps for `pid` under the leg's `CODEX_HOME`.
    fn record(leg: &Leg, pid: i32) -> Option<J> {
        let path = leg.home.join(format!("alab/sessions/{pid}.json"));
        J::parse(&std::fs::read_to_string(path).ok()?).ok()
    }

    fn aterm_log(&self) -> String {
        let bytes = std::fs::read(self.h.join("Library/Logs/aterm/aterm.log")).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The host's log lines for one tab.
    fn host_lines(&self, sid: &str) -> Vec<String> {
        let at = format!("@{sid}:");
        self.aterm_log()
            .lines()
            .filter(|l| l.contains(&at))
            .map(str::to_string)
            .collect()
    }

    /// Every `step` until `ok` holds, at most `limit`: a hang detector, never
    /// a latency budget. Whether it held.
    fn wait_for(
        limit: Duration,
        step: Duration,
        mut ok: impl FnMut() -> bool,
    ) -> Result<bool, Stopped> {
        let end = Instant::now() + limit;
        loop {
            if ok() {
                return Ok(true);
            }
            if Instant::now() >= end {
                return Ok(false);
            }
            sleep(step)?;
        }
    }

    /// The leg's world (its `CODEX_HOME` and project), its tab, and the
    /// launch line typed at the tab's shell as a person types it.
    fn spawn_leg(&self, name: &'static str, prompt: &str, extra: &str) -> Result<Leg, Fail> {
        let dir = self.s.join("legs").join(name);
        let home = dir.join("codex-home");
        let proj = dir.join("proj");
        let made = (|| -> std::io::Result<PathBuf> {
            std::fs::create_dir_all(&home)?;
            std::fs::create_dir_all(&proj)?;
            let proj = std::fs::canonicalize(&proj)?;
            let _ = aterm_uds::spawnfd::command("/usr/bin/git")
                .args(["init", "-q"])
                .current_dir(&proj)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            std::fs::write(proj.join("README.md"), "# lab scratch project\n")?;
            std::fs::write(
                home.join("config.toml"),
                codex_config(self.port, &proj, &home, extra),
            )?;
            Ok(proj)
        })();
        let proj = made.map_err(|e| format!("cannot lay out leg {name}: {e}"))?;
        let spawned = self
            .inst
            .ctl(&["spawn", &format!("cwd={}", proj.display())]);
        let sid = spawned
            .first()
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().next())
            .map(str::to_string)
            .ok_or_else(|| format!("spawn for leg {name}: {}", spawned.first()))?;
        let _ = self.on(&sid, &["await", "idle", "400", "timeout=30000"]);
        let line = format!(
            "export CODEX_HOME={}; cd {} && codex {}\r",
            sh_quote(&home.to_string_lossy()),
            sh_quote(&proj.to_string_lossy()),
            sh_quote(&format!("LEG-{name} {prompt}")),
        );
        let sent = self
            .inst
            .ctl_stdin(&[&format!("@{sid}"), "send", "--stdin"], line.as_bytes());
        if sent.code != 0 {
            return Err(Fail::die(format!(
                "leg {name}: the launch line was not typed: {}",
                sent.first()
            )));
        }
        println!("  leg {name}: tab {sid}");
        Ok(Leg {
            name,
            sid,
            home,
            proj,
        })
    }

    /// `aterm pkg link codex <pkg> bin/codex` under the scratch HOME.
    fn link(&self, pkg: &Path) -> Result<(), String> {
        let out = aterm_uds::spawnfd::command(&self.inst.aterm)
            .env_clear()
            .envs(self.world_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(["pkg", "link", "codex"])
            .arg(pkg)
            .arg("bin/codex")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("aterm pkg link: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "aterm pkg link codex {} bin/codex: {}{}",
                pkg.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    /// The claim of `leg` proven, and the host watching only beside it.
    fn claim(&mut self, leg: &Leg) -> Result<Option<i32>, Stopped> {
        let mut pid = None;
        Self::wait_for(Duration::from_secs(180), Duration::from_millis(500), || {
            pid = self.proven_pid(&leg.sid);
            pid.is_some()
        })?;
        let name = leg.name;
        self.check(
            &format!(
                "{name}: the claim is proven (supervisor=codex-alab@<pid> supervisor_proven=1)"
            ),
            pid.is_some(),
        );
        Ok(pid)
    }

    /// `act` on the leg's timeline within `limit`.
    fn await_act(&self, leg: &Leg, act: &str, limit: Duration) -> Result<bool, Stopped> {
        Self::wait_for(limit, Duration::from_secs(1), || {
            self.acts(&leg.sid).iter().any(|a| a == act)
        })
    }

    fn first_leg(&mut self, first: &Leg) -> Result<(), Stopped> {
        let held = self.await_act(first, "finished@v1", Duration::from_secs(120))?;
        let parked = self
            .host_lines(&first.sid)
            .iter()
            .any(|l| l.contains("WATCHING a proven self-claim supervises this session"));
        self.check("first: aterm's lane parked (its host watches only)", parked);
        self.check("first: the done mark stops it (finished@v1)", held);
        // Nothing after the done: no continuation, no request.
        sleep(Duration::from_secs(15))?;
        let acts = self.acts(&first.sid);
        self.check(
            "first: no continuation after the done mark",
            !acts.iter().any(|a| a.starts_with("continue")) && self.requests("first").len() == 1,
        );
        Ok(())
    }

    fn continue_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(150))?;
        let acts = self.acts(&leg.sid);
        let at = |a: &str| acts.iter().position(|x| x == a);
        self.check(
            "continue: a turn without the done mark is continued (continue@v1, then finished@v1)",
            done && at("continue@v1")
                .zip(at("finished@v1"))
                .is_some_and(|(c, f)| c < f),
        );
        let keep: Vec<J> = self
            .requests("continue")
            .into_iter()
            .filter(|r| {
                r.get("u")
                    .and_then(J::as_str)
                    .is_some_and(|u| u.contains(KEEP_GOING))
            })
            .collect();
        self.check(
            "continue: the continuation sent `keep going` once",
            keep.len() == 1,
        );
        sleep(Duration::from_secs(15))?;
        self.check(
            "continue: the done mark stops it (nothing sent after it)",
            self.tags("continue") == ["continue:t1", "continue:after-continue"],
        );
        Ok(())
    }

    fn approval_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(150))?;
        let acts = self.acts(&leg.sid);
        self.check(
            "approval: the escalated exec is approved once (approve-all@v1)",
            acts.iter().filter(|a| *a == "approve-all@v1").count() == 1,
        );
        let ran = std::fs::read_to_string(leg.proj.join("ran.txt")).unwrap_or_default();
        self.check(
            "approval: the approved command ran (ran.txt) and the task finished",
            ran.contains("approved-run") && done,
        );
        Ok(())
    }

    fn question_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(150))?;
        let acts = self.acts(&leg.sid);
        let answered = self.requests("question").iter().any(|r| {
            r.get("tool_output")
                .and_then(J::as_str)
                .is_some_and(|o| o.contains("Postgres (Recommended)"))
        });
        self.check(
            "question: the overlay is answered with its (Recommended) option (answer-recommended@v1)",
            answered && acts.iter().any(|a| a == "answer-recommended@v1"),
        );
        self.check("question: the task finished after the answer", done);
        Ok(())
    }

    fn hold_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        // The first turn's answer came (held by the model until the hold stood).
        let answered = Self::wait_for(Duration::from_secs(90), Duration::from_secs(1), || {
            !self.requests("hold").is_empty()
        })?;
        let t1 = self
            .requests("hold")
            .first()
            .and_then(|r| r.get("t").and_then(J::as_i64))
            .unwrap_or(0);
        let since = Instant::now();
        // The answer is sent HOLD_FIRST_TURN after its request: past it, and
        // past the grace, with room for a continuation that must not come.
        let quiet = HOLD_FIRST_TURN + Duration::from_secs(25);
        while since.elapsed() < quiet
            && crate::util::now_secs() * 1000 < t1 + i64::try_from(quiet.as_millis()).unwrap_or(0)
        {
            sleep(Duration::from_secs(1))?;
        }
        let held_acts = self.acts(&leg.sid);
        self.check(
            "hold: nothing continued while the hold stood",
            answered
                && !held_acts.iter().any(|a| a.starts_with("continue"))
                && self.tags("hold") == ["hold:t1"],
        );
        self.check(
            "hold: codex-alab kept its proven claim under the hold",
            pid.is_some() && self.proven_pid(&leg.sid) == pid,
        );
        let off = self.inst.ctl(&["hold", &leg.sid, "off"]);
        self.check(
            &format!("hold: lifted ({})", off.first()),
            off.first().contains("hold=0"),
        );
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(120))?;
        let acts = self.acts(&leg.sid);
        self.check(
            "hold: lifted, the turn is continued (continue@v1, then finished@v1)",
            done && acts.iter().any(|a| a == "continue@v1"),
        );
        Ok(())
    }

    fn usage_leg(&mut self, leg: &Leg) -> Result<(), Stopped> {
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(300))?;
        let rows = self.requests("usage");
        let reset = rows
            .first()
            .and_then(|r| r.get("tag").and_then(J::as_str))
            .and_then(|t| t.split_once("resets_at="))
            .and_then(|(_, n)| n.parse::<i64>().ok());
        let resumed = rows
            .iter()
            .find(|r| r.get("tag").and_then(J::as_str) == Some("usage:after-reset"))
            .and_then(|r| r.get("t").and_then(J::as_i64));
        let limited = rows
            .iter()
            .filter(|r| r.get("status").and_then(J::as_i64) == Some(429))
            .count();
        if let (Some(reset), Some(at)) = (reset, resumed) {
            println!(
                "    usage: resets_at={reset}; resumed {:.1} s after it",
                (at - reset * 1000) as f64 / 1000.0
            );
        }
        self.check(
            "usage: the 429 is waited out: nothing sent until resets_at + 60 s",
            limited >= 1
                && reset
                    .zip(resumed)
                    .is_some_and(|(reset, at)| at >= (reset + 60) * 1000),
        );
        let acts = self.acts(&leg.sid);
        self.check(
            "usage: then resumed (usage-resume@v1) and finished",
            done && acts.iter().any(|a| a == "usage-resume@v1"),
        );
        Ok(())
    }

    /// `SIGKILL` the codex-alab `pid` of `leg`; the crash rule's relaunch
    /// proven again (a new pid). `fast`: poll the new claim tightly.
    fn crash_once(&mut self, leg: &Leg, pid: i32, label: &str) -> Result<Option<i32>, Stopped> {
        let _ = crate::sys::kill(pid, libc::SIGKILL);
        println!("    {label}: SIGKILL {pid}");
        let mut next = None;
        Self::wait_for(Duration::from_secs(150), Duration::from_millis(50), || {
            next = self.proven_pid(&leg.sid).filter(|p| *p != pid);
            next.is_some()
        })?;
        self.check(
            &format!("{label}: relaunched on its conversation and proven again"),
            next.is_some(),
        );
        Ok(next)
    }

    fn crash_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        let Some(pid1) = pid else {
            self.check("crash: no proven codex-alab to end", false);
            return Ok(());
        };
        let session = Self::record(leg, pid1)
            .and_then(|r| r.get("sessionId").and_then(J::as_str).map(str::to_string));
        let Some(pid2) = self.crash_once(leg, pid1, "crash (the instance's first session)")? else {
            return Ok(());
        };
        // At once: the relaunched one's record may not be written yet.
        let written = leg
            .home
            .join(format!("alab/sessions/{pid2}.json"))
            .is_file();
        let Some(pid3) = self.crash_once(
            leg,
            pid2,
            &format!(
                "second crash (as soon as the relaunch's claim was proven; its record {})",
                if written {
                    "written"
                } else {
                    "not yet written"
                }
            ),
        )?
        else {
            return Ok(());
        };
        let relaunches = self
            .host_lines(&leg.sid)
            .iter()
            .filter(|l| l.contains("the self-supervised codex-alab crashed; relaunching it"))
            .count();
        self.check(
            "crash: each crash relaunched by the crash rule (two relaunches)",
            relaunches == 2,
        );
        let mut resumed = None;
        Self::wait_for(Duration::from_secs(60), Duration::from_secs(1), || {
            resumed = Self::record(leg, pid3)
                .and_then(|r| r.get("sessionId").and_then(J::as_str).map(str::to_string));
            resumed.is_some()
        })?;
        self.check(
            "crash: the twice-relaunched codex-alab resumed the same conversation",
            session.is_some() && resumed == session,
        );
        Ok(())
    }

    /// The leg's codex-alab journal (`<CODEX_HOME>/log/codex-tui.log`, every
    /// process of the leg in one file).
    fn codex_log(leg: &Leg) -> String {
        std::fs::read_to_string(leg.home.join("log/codex-tui.log")).unwrap_or_default()
    }

    /// The instance's harness upgrade ledger (`aterm harness ledger upgrade
    /// --json` under the scratch HOME, as the owner reads it), row by row.
    fn upgrade_ledger(&self) -> Vec<J> {
        let out = aterm_uds::spawnfd::command(&self.inst.aterm)
            .env_clear()
            .envs(self.world_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(["harness", "ledger", "upgrade", "4096", "--json"])
            .stdin(Stdio::null())
            .output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
            .lines()
            .filter_map(|l| J::parse(l).ok())
            .collect()
    }

    /// THE CARRY-ON AFTER A CRASH RELAUNCH (module header): `SIGKILL` the
    /// leg's codex-alab mid-turn, once its crash is relaunchable; the
    /// relaunched one carries the turn on by itself only when its launch probe
    /// saw aterm's relaunch lease.
    fn carry_leg(&mut self, leg: &Leg, pid: Option<i32>) -> Result<(), Stopped> {
        let Some(pid1) = pid else {
            self.check("carry: no proven codex-alab to end", false);
            return Ok(());
        };
        // Mid-turn and relaunchable: the turn's request is at the model, the
        // record says `busy` and names its resume line, and the claim renewed
        // since that line moved (aterm re-reads the record into its copy at
        // the renewal, before it answers). A hang detector, never a budget.
        let renewed = "renewed the claim: the record's resume line moved";
        let ready = Self::wait_for(Duration::from_secs(120), Duration::from_millis(250), || {
            !self.requests("carry").is_empty()
                && Self::record(leg, pid1).is_some_and(|r| {
                    r.get("status").and_then(J::as_str) == Some("busy")
                        && r.get("resumeArgv")
                            .and_then(J::as_arr)
                            .is_some_and(|a| !a.is_empty())
                })
                && Self::codex_log(leg).contains(renewed)
        })?;
        self.check(
            "carry: mid-turn and relaunchable (its request at the model, its record busy naming its resume line, its claim renewed with it)",
            ready,
        );
        if !ready {
            return Ok(());
        }
        let killed_ms = crate::util::now_secs() * 1000;
        let Some(pid2) = self.crash_once(leg, pid1, "carry: crash mid-turn")? else {
            return Ok(());
        };
        // codex-alab's carry-on (its continuation after the grace), and the
        // act it records once its turn started: hang detectors, never budgets.
        let carried = Self::wait_for(Duration::from_secs(120), Duration::from_secs(1), || {
            self.tags("carry").iter().any(|t| t == "carry:after-carry")
        })?;
        let turned = self.await_act(leg, "harness-turn@v1", Duration::from_secs(60))?;
        let log = Self::codex_log(leg);
        let probe = format!("launch probe: hand=lease:aterm-harness@{} ", self.inst.pid);
        let probes: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("launch probe:"))
            .map(|l| l.split_once("launch probe:").map_or(l, |(_, r)| r.trim()))
            .collect();
        self.check(
            &format!(
                "carry: the relaunched codex-alab's launch probe saw aterm's relaunch lease and resumed as relaunched (probes: {})",
                probes.join(" | ")
            ),
            log.contains(&probe) && log.contains("relaunched=true)"),
        );
        // How the hold ended, by aterm's own ledger: at the relaunched
        // process's first idle, never at its bound (which would also let the
        // carry-on go, a minute late) nor at its end.
        let held = self
            .upgrade_ledger()
            .iter()
            .filter(|row| {
                row.get("step").and_then(J::as_str) == Some("done:resumed")
                    && row.get("pid").and_then(J::as_i64) == Some(i64::from(pid2))
            })
            .filter_map(|row| row.get("detail").and_then(J::as_str).map(str::to_string))
            .next_back()
            .unwrap_or_default();
        let held = held
            .rsplit_once("; ")
            .map_or(held.as_str(), |(_, h)| h)
            .to_string();
        self.check(
            &format!(
                "carry: aterm held its relaunch hand to the relaunched process's first idle (its ledger: {held:?})"
            ),
            held == "the hand held to its first idle",
        );
        let keep = self
            .requests("carry")
            .iter()
            .filter(|r| {
                r.get("u")
                    .and_then(J::as_str)
                    .is_some_and(|u| u.contains(KEEP_GOING))
                    && r.get("t").and_then(J::as_i64).unwrap_or(0) >= killed_ms
            })
            .count();
        let holder = format!("codex-alab@{pid2}");
        let by_relaunched = turned
            && self
                .on(&leg.sid, &["timeline"])
                .stdout
                .lines()
                .filter(|l| l.contains("kind=harness-act"))
                .any(|l| {
                    field(l, "act").as_deref() == Some("harness-turn@v1")
                        && field(l, "holder").as_deref() == Some(holder.as_str())
                });
        self.check(
            &format!(
                "carry: the relaunched codex-alab carried the interrupted turn on by itself, once (`keep going` x{keep} after the relaunch, harness-turn@v1 by {holder}: {by_relaunched})"
            ),
            carried && keep == 1 && by_relaunched,
        );
        let done = self.await_act(leg, "finished@v1", Duration::from_secs(60))?;
        self.check("carry: then finished (finished@v1)", done);
        Ok(())
    }

    fn upgrade_leg(&mut self, leg: &Leg, pkg_b: &Path) -> Result<(), Stopped> {
        let Some(pid) = self.proven_pid(&leg.sid) else {
            self.check("upgrade: no proven codex-alab to move", false);
            return Ok(());
        };
        let generation = |r: &J| r.get("execGeneration").and_then(J::as_i64).unwrap_or(0);
        let before = Self::record(leg, pid).map_or(0, |r| generation(&r));
        let linked = self.link(pkg_b);
        self.check(
            &format!(
                "upgrade: the package re-linked to its copy ({})",
                linked.as_ref().err().map_or("ok", String::as_str)
            ),
            linked.is_ok(),
        );
        let b = std::fs::canonicalize(pkg_b).unwrap_or_else(|_| pkg_b.to_path_buf());
        let moved = Self::wait_for(Duration::from_secs(150), Duration::from_secs(1), || {
            Self::record(leg, pid).is_some_and(|r| {
                generation(&r) > before
                    && r.get("exe")
                        .and_then(J::as_str)
                        .and_then(|e| std::fs::canonicalize(e).ok())
                        .is_some_and(|e| e.starts_with(&b))
            })
        })?;
        let acts = self.acts(&leg.sid);
        self.check(
            "upgrade: the running codex-alab re-exec'd onto the re-linked build (upgrade@v1, execGeneration up, exe the copy's)",
            moved && acts.iter().any(|a| a == "upgrade@v1"),
        );
        let reproven = Self::wait_for(Duration::from_secs(90), Duration::from_secs(1), || {
            self.proven_pid(&leg.sid) == Some(pid)
        })?;
        self.check("upgrade: the new image proved its claim again", reproven);
        Ok(())
    }

    /// The instance's bridge is connected to the lab's broker
    /// (`fabric status state=connected`): a hang detector, never a budget.
    fn bus_up(&mut self) -> Result<bool, Stopped> {
        let mut last = String::new();
        let up = Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
            last = self.inst.ctl(&["fabric", "status"]).first();
            field(&last, "state").as_deref() == Some("connected")
        })?;
        self.check(
            &format!("the lab's bus: the instance's bridge is connected ({last})"),
            up,
        );
        Ok(up)
    }

    /// THE AUTOPILOT OFF STEP (module header): Off gives the claim back and the
    /// mail goes alone; On, the claim comes back.
    fn autopilot_off_leg(&mut self, leg: &Leg, manager: &str) -> Result<(), Stopped> {
        let name = leg.name;
        let before = self.proven_pid(&leg.sid);
        self.check(
            &format!("autopilot: {name} holds a proven claim before the off"),
            before.is_some(),
        );
        let off = self.on(&leg.sid, &["meta", "set", "autopilot", "off"]);
        self.check(
            &format!("autopilot: Autopilot Off on {name} ({})", off.first()),
            off.code == 0 && off.first().starts_with("OK"),
        );
        let mut status = String::new();
        let released =
            Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
                status = self.status(&leg.sid);
                field(&status, "supervisor_proven").as_deref() != Some("1")
                    && !field(&status, "supervisor").is_some_and(|s| s.starts_with("codex-alab@"))
            })?;
        self.check(
            &format!(
                "autopilot: under Autopilot Off codex-alab gives its claim back (supervisor={} supervisor_proven={})",
                field(&status, "supervisor").unwrap_or_default(),
                field(&status, "supervisor_proven").unwrap_or_default()
            ),
            released,
        );
        let turns_before = self.on(&leg.sid, &["history"]).stdout;
        let asked_before = self.requests(name).len();
        // The manager's mail, posted from its own tab: `post` types nothing
        // into anyone (nothing aterm types wakes an agent for mail), and
        // `kind=task` waits for the landing, `OK <id> off=<n>`.
        let to = format!("to=@{}", leg.sid);
        let text = format!("LEG-{name} lab mail sent under Autopilot Off");
        let task = self.on(manager, &["post", &to, "kind=task", "--", &text]);
        let said = task.first();
        let off_at = said
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().find_map(|w| w.strip_prefix("off=")))
            .map(str::to_string);
        self.check(
            &format!(
                "autopilot: the manager's `post kind=task` lands the mail ({said}{})",
                if task.code == 0 {
                    String::new()
                } else {
                    format!("; exit {}: {}", task.code, task.stderr.trim())
                }
            ),
            task.code == 0 && off_at.is_some(),
        );
        let mut rows = String::new();
        let landed = Self::wait_for(Duration::from_secs(60), Duration::from_millis(500), || {
            rows = self.on(&leg.sid, &["inbox", "--peek"]).stdout;
            rows.lines().any(|row| {
                row.starts_with("msg ")
                    && field(row, "kind").as_deref() == Some("task")
                    && off_at
                        .as_deref()
                        .is_some_and(|off| field(row, "off").as_deref() == Some(off))
            })
        })?;
        self.check(
            "autopilot: the task is in the worker's inbox (kind=task, its offset)",
            landed,
        );
        // Something that must NOT happen gets a quiet window, never a budget
        // for something that must: past the harness's grace, nothing typed.
        sleep(Duration::from_secs(15))?;
        let screen = self.on(&leg.sid, &["text"]).stdout;
        let turns_after = self.on(&leg.sid, &["history"]).stdout;
        self.check(
            "autopilot: no `Inbox: task @` line was typed (none on screen, no turn on the ledger, no model request)",
            !screen.contains(NUDGE)
                && turns_after == turns_before
                && !turns_after.contains(NUDGE)
                && self.requests(name).len() == asked_before,
        );
        let on = self.on(&leg.sid, &["meta", "set", "autopilot", "cruise"]);
        self.check(
            &format!("autopilot: back On (cruise) on {name} ({})", on.first()),
            on.code == 0 && on.first().starts_with("OK"),
        );
        let mut again = None;
        Self::wait_for(Duration::from_secs(120), Duration::from_millis(500), || {
            again = self.proven_pid(&leg.sid);
            again.is_some()
        })?;
        self.check(
            &format!(
                "autopilot: back On, codex-alab claims again (supervisor=codex-alab@{} supervisor_proven=1)",
                again.map_or_else(|| "-".to_string(), |p| p.to_string())
            ),
            again.is_some(),
        );
        Ok(())
    }

    /// What each leg's host and codex-alab said, after a failed check.
    fn diagnose(&self, legs: &[&Leg]) {
        for leg in legs {
            let host = self.host_lines(&leg.sid);
            for l in &host[host.len().saturating_sub(25)..] {
                println!(
                    "  host {} {}",
                    leg.name,
                    l.chars().take(260).collect::<String>()
                );
            }
            println!("  status {} {}", leg.name, self.status(&leg.sid));
            println!("  acts {} {}", leg.name, self.acts(&leg.sid).join(" "));
            let log =
                std::fs::read_to_string(leg.home.join("log/codex-tui.log")).unwrap_or_default();
            let alab: Vec<&str> = log.lines().filter(|l| l.contains("codex_alab")).collect();
            for l in &alab[alab.len().saturating_sub(15)..] {
                println!(
                    "  codex {} {}",
                    leg.name,
                    l.chars().take(260).collect::<String>()
                );
            }
            println!("  model {} {}", leg.name, self.tags(leg.name).join(" "));
        }
    }

    fn end_all(&mut self) {
        codex_rig::end_all(self.inst.pid, &self.s);
        self.inst.stop();
        self.fabric.stop();
    }
}

/// `s` as one POSIX shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A leg's `config.toml`: the lab's model provider, Codex's own updater,
/// analytics, feedback and telemetry off, the TUI's file log in its home, and
/// the project trusted as spelled and as resolved.
fn codex_config(port: u16, proj: &Path, home: &Path, extra: &str) -> String {
    let p = proj.to_string_lossy();
    let mut trusted = vec![p.to_string()];
    if let Some(short) = p.strip_prefix("/private") {
        trusted.push(short.to_string());
    }
    let projects: String = trusted
        .iter()
        .map(|d| format!("[projects.\"{d}\"]\ntrust_level = \"trusted\"\n"))
        .collect();
    format!(
        "# aterm-lab codex-alab-proof: codex talks only to the lab's model on 127.0.0.1:{port}\n\
         model_provider = \"lab\"\nmodel = \"gpt-5.5\"\napproval_policy = \"on-request\"\n\
         sandbox_mode = \"workspace-write\"\ncheck_for_update_on_startup = false\n\
         cli_auth_credentials_store = \"file\"\nmcp_oauth_credentials_store = \"file\"\n\
         log_dir = \"{log}\"\n\n\
         [analytics]\nenabled = false\n\n[feedback]\nenabled = false\n\n\
         [otel]\nexporter = \"none\"\nmetrics_exporter = \"none\"\ntrace_exporter = \"none\"\n\n\
         [model_providers.lab]\nname = \"lab fake model\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n\
         wire_api = \"responses\"\nexperimental_bearer_token = \"{key}\"\nrequest_max_retries = 0\n\
         stream_max_retries = 0\nstream_idle_timeout_ms = 300000\n\n{projects}\n{extra}",
        log = home.join("log").display(),
        key = crate::stub::LAB_CODEX_KEY,
    )
}

/// `codex-alab-proof [--package DIR] [--aterm PATH] [--keep]`.
///
/// # Errors
/// Usage, a run that could not start, or an interrupt.
pub fn cmd_codex_alab_proof(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let keep = instance::take_flag(&mut a, "--keep");
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let given = instance::take_value(&mut a, "--package")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "codex-alab-proof: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let not_run = |why: String| Fail::exit(3, format!("NOT RUN: {why}"));
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    let home = crate::paths::paths().need_home().map_err(not_run)?;
    let pkg = package(given.as_deref(), home).map_err(not_run)?;
    let build =
        std::fs::read_to_string(pkg.join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON))
            .ok()
            .and_then(|t| J::parse(&t).ok());
    let said = |k: &str| {
        build
            .as_ref()
            .and_then(|b| b.get(k).and_then(J::as_str).map(str::to_string))
            .unwrap_or_else(|| "?".to_string())
    };
    println!(
        "codex-alab {} (source {}) at {}; aterm {}",
        said("version"),
        said("source"),
        pkg.display(),
        aterm.display()
    );
    let mut world = World::new(
        "agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
         [packages]\nenabled = false\n[machine]\nuniversal_control = \"leave\"\n\
         [harness]\nenabled = true\nheadless = true\nhuman_grace_s = 5\n",
    )?;
    if keep {
        world.keep();
    }
    let s = world.dir.clone();
    let h = world.home.clone();
    let prefix = h.join("Library/Application Support/aterm/pkg");
    let api_log = s.join("api.log");
    let made = (|| -> std::io::Result<u16> {
        for d in ["zdot", "legs"] {
            std::fs::create_dir_all(s.join(d))?;
        }
        codex_rig::stand_ins(&s.join("bin"))?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let log = api_log.clone();
        std::thread::spawn(move || codex_rig::serve(listener, log, Arc::new(answer)));
        std::fs::write(
            s.join("zdot/.zshrc"),
            format!(
                "PS1='%# '\nunsetopt PROMPT_SP\nHISTFILE=/dev/null\n\
                 export PATH=\"{bin}:{agents}:/usr/bin:/bin:/usr/sbin:/sbin\"\n\
                 export RUST_LOG='codex_alab_autopilot=debug,codex_alab_aterm=debug,codex_alab_tui=debug,codex_tui=info'\n",
                bin = s.join("bin").display(),
                agents = prefix.join("agents").display(),
            ),
        )?;
        Ok(port)
    })();
    let port =
        made.map_err(|e| format!("codex-alab-proof: cannot lay out {}: {e}", s.display()))?;
    // The lab's own bus, named in the instance's `aterm.toml` before it starts.
    let fabric = Fabric::start(&aterm, &s.join("fabric"))
        .map_err(|e| format!("codex-alab-proof: cannot start the lab's bus: {e}"))?;
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(world.cfg.join("aterm/aterm.toml"))
            .and_then(|mut f| f.write_all(fabric.table.as_bytes()))
            .map_err(|e| format!("codex-alab-proof: cannot name the bridge: {e}"))?;
    }
    // The re-exec upgrade's build: the same bytes at another path (a clone).
    let pkg_b = s.join("pkg-b");
    let cloned = aterm_uds::spawnfd::command("/bin/cp")
        .arg("-cR")
        .arg(&pkg)
        .arg(&pkg_b)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|st| st.success());
    if !cloned {
        return Err(Fail::die(format!(
            "codex-alab-proof: cannot copy {} to {}",
            pkg.display(),
            pkg_b.display()
        )));
    }
    let sstr = |sub: &str| s.join(sub).to_string_lossy().into_owned();
    let env: Vec<(String, String)> = [
        (
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", sstr("bin")),
        ),
        ("SHELL", "/bin/zsh".to_string()),
        ("HOME", h.to_string_lossy().into_owned()),
        ("XDG_CONFIG_HOME", world.cfg.to_string_lossy().into_owned()),
        ("XDG_RUNTIME_DIR", world.run.to_string_lossy().into_owned()),
        ("ZDOTDIR", sstr("zdot")),
        ("TERM", "xterm-256color".to_string()),
        ("LANG", "en_US.UTF-8".to_string()),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .to_vec();
    let world_env = {
        let mut e = world.env();
        e.retain(|(k, _)| k != "PATH");
        e.push(("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()));
        e
    };
    let inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: true,
            args: ["--columns", "120", "--lines", "40"]
                .map(str::to_string)
                .to_vec(),
            env,
            cwd: s.clone(),
            sandbox: Some(codex_rig::jail_policy(port)),
        },
    )?;
    let mut proof = Proof {
        inst,
        s,
        h,
        world_env,
        api_log,
        port,
        fails: Vec::new(),
        fabric,
    };
    let outcome = legs(&mut proof, &pkg, &pkg_b);
    proof.end_all();
    if let Err(f) = outcome
        && (f.stopped.is_some() || proof.fails.is_empty())
    {
        return Err(f);
    }
    if proof.fails.is_empty() {
        println!("PASS");
        Ok(0)
    } else {
        println!("FAIL: {} check(s)", proof.fails.len());
        for f in &proof.fails {
            println!("  - {f}");
        }
        Ok(1)
    }
}

fn legs(proof: &mut Proof, pkg: &Path, pkg_b: &Path) -> Result<(), Fail> {
    // The front door lays agents/codex and the stable exec path before any
    // tab: linked as the owner links it.
    if let Err(why) = proof.link(pkg) {
        proof.check(&format!("the package is linked ({why})"), false);
        return Ok(());
    }
    if let Err(f) = proof.inst.await_socket(instance::BOOT) {
        proof.check("the headless aterm answers its socket", false);
        return Err(f);
    }
    let _ = proof.inst.ctl(&["ready"]);
    // The Autopilot Off step's mail: the bridge on the lab's bus, and a plain
    // tab to post it from (`post kind=task`).
    let bus = proof.bus_up()?;
    let manager = {
        let spawned = proof
            .inst
            .ctl(&["spawn", &format!("cwd={}", proof.s.display())]);
        spawned
            .first()
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().next())
            .map(str::to_string)
    };
    proof.check(
        &format!(
            "the manager's tab ({})",
            manager.as_deref().unwrap_or("not spawned")
        ),
        manager.is_some(),
    );
    println!("legs (one instance, each a tab; the model answers by the leg its prompt names)");
    let first = proof.spawn_leg("first", "Say hello and finish.", "")?;
    let hold = proof.spawn_leg("hold", "Write a plan, then stop.", "")?;
    let hold_pid = proof.claim(&hold)?;
    let on = proof.inst.ctl(&["hold", &hold.sid, "on", "reason=lab"]);
    proof.check(
        &format!("hold: put on while its first turn runs ({})", on.first()),
        on.first().contains("hold=1"),
    );
    let cont = proof.spawn_leg("continue", "Write a plan, then stop.", "")?;
    let approval = proof.spawn_leg("approval", "Run the build.", "")?;
    let question = proof.spawn_leg(
        "question",
        "Set up the database.",
        "[features]\ndefault_mode_request_user_input = true\n",
    )?;
    let usage = proof.spawn_leg("usage", "Start the task.", "")?;
    let carry = proof.spawn_leg("carry", "Do the long task.", "")?;
    let all = [&first, &hold, &cont, &approval, &question, &usage, &carry];
    let mut pids = vec![None; all.len()];
    for (i, leg) in all.iter().enumerate() {
        pids[i] = if leg.name == "hold" {
            hold_pid
        } else {
            proof.claim(leg)?
        };
    }
    let at_once = all
        .iter()
        .filter(|leg| proof.proven_pid(&leg.sid).is_some())
        .count();
    proof.check(
        &format!(
            "{at_once} codex-alab sessions in one aterm hold proven claims at once (each keeps a subscribe stream; five and more)"
        ),
        at_once == all.len(),
    );
    proof.first_leg(&first)?;
    proof.continue_leg(&cont)?;
    proof.approval_leg(&approval)?;
    proof.question_leg(&question)?;
    match (bus, manager.as_deref()) {
        (true, Some(manager)) => proof.autopilot_off_leg(&question, manager)?,
        _ => {
            proof.check(
                "autopilot: the Autopilot Off step needs the lab's bus and the manager's tab",
                false,
            );
        }
    }
    proof.hold_leg(&hold, hold_pid)?;
    proof.crash_leg(&first, pids[0])?;
    proof.carry_leg(&carry, pids[6])?;
    proof.usage_leg(&usage)?;
    proof.upgrade_leg(&cont, pkg_b)?;
    if !proof.fails.is_empty() {
        proof.diagnose(&all);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(texts: &[&str], tool: Option<&str>) -> J {
        let mut input: Vec<J> = texts
            .iter()
            .map(|t| J::obj([("role", J::s("user")), ("content", J::s(*t))]))
            .collect();
        if let Some(out) = tool {
            input.push(J::obj([
                ("type", J::s("function_call_output")),
                ("output", J::s(out)),
            ]));
        }
        J::obj([("input", J::Arr(input))])
    }

    fn text_of(a: &Answered) -> String {
        a.body
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| J::parse(d).ok())
            .filter_map(|d| d.get("item").cloned())
            .map(|it| it.py())
            .collect()
    }

    #[test]
    fn each_leg_is_answered_by_the_name_its_prompt_carries() {
        assert_eq!(
            leg_of(&body(&["<env>", "LEG-continue Write a plan"], None)).as_deref(),
            Some("continue")
        );
        assert_eq!(leg_of(&body(&["no leg here"], None)), None);
        let first = answer(&body(&["LEG-continue Write a plan"], None));
        assert_eq!(first.tag, "continue:t1");
        assert!(
            !text_of(&first).contains(DONE),
            "the first turn ends undone"
        );
        let after = answer(&body(&["LEG-continue Write a plan", "keep going"], None));
        assert_eq!(after.tag, "continue:after-continue");
        assert!(text_of(&after).contains(DONE));
        let long = answer(&body(&["LEG-carry Do the long task."], None));
        assert_eq!(long.tag, "carry:t1");
        assert!(
            long.delay >= CARRY_FIRST_TURN && !text_of(&long).contains(DONE),
            "the carry leg's first turn is still running when it is killed"
        );
        let carried = answer(&body(&["LEG-carry Do the long task.", "keep going"], None));
        assert_eq!(
            (carried.tag.as_str(), carried.delay),
            ("carry:after-carry", Duration::ZERO)
        );
        assert!(text_of(&carried).contains(DONE));
        let call = answer(&body(&["LEG-approval Run the build."], None));
        assert!(text_of(&call).contains("require_escalated"));
        let ran = answer(&body(&["LEG-approval Run the build."], Some("done")));
        assert_eq!(ran.tag, "approval:after-output");
        let ask = answer(&body(&["LEG-question Set up"], None));
        assert!(text_of(&ask).contains("request_user_input"));
        assert!(text_of(&ask).contains("Postgres (Recommended)"));
        // Negative control: an unnamed request is plainly finished, never a wall.
        let plain = answer(&body(&["hello"], None));
        assert_eq!((plain.status, plain.tag.as_str()), (200, "-:t1"));
    }

    #[test]
    fn the_usage_leg_keeps_one_reset_until_it_is_continued() {
        let a = answer(&body(&["LEG-usage Start the task."], None));
        let b = answer(&body(&["LEG-usage Start the task."], None));
        assert_eq!((a.status, b.status), (429, 429));
        assert_eq!(a.tag, b.tag, "one window: the reset never moves");
        assert!(a.body.contains("usage_limit_reached"));
        assert!(
            a.headers
                .iter()
                .any(|(k, _)| k == "x-codex-primary-used-percent")
        );
        let after = answer(&body(&["LEG-usage Start the task.", "keep going"], None));
        assert_eq!(
            (after.status, after.tag.as_str()),
            (200, "usage:after-reset")
        );
    }

    #[test]
    fn a_side_request_is_answered_by_its_schema_not_its_leg() {
        let mut b = body(&["LEG-usage Start the task."], None);
        b.set(
            "text",
            J::obj([(
                "format",
                J::obj([
                    ("type", J::s("json_schema")),
                    (
                        "schema",
                        J::obj([(
                            "properties",
                            J::obj([("title", J::obj([("type", J::s("string"))]))]),
                        )]),
                    ),
                ]),
            )]),
        );
        let a = answer(&b);
        assert_eq!((a.status, a.tag.as_str()), (200, "usage:side"));
    }

    #[test]
    fn a_package_without_the_alab_mark_is_a_vendor_codex() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive package fixture");
        let dir = scratch.path();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/codex"), "").unwrap();
        std::fs::write(
            dir.join("codex-package.json"),
            r#"{"layoutVersion":1,"version":"0.164.0-dev.1+gabc","entrypoint":"bin/codex"}"#,
        )
        .unwrap();
        let bin = dir.join("bin/codex").to_string_lossy().into_owned();
        let vendor = package(Some(&bin), "/nonexistent").unwrap_err();
        assert!(vendor.contains("vendor Codex"), "{vendor}");
        std::fs::write(dir.join("alab-build.json"), "{}").unwrap();
        let root = package(Some(&bin), "/nonexistent").unwrap();
        assert_eq!(root, std::fs::canonicalize(dir).unwrap());
        assert!(package(Some("/nonexistent/codex"), "/nonexistent").is_err());
    }
}
