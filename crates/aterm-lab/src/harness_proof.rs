// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `harness-proof <id> --repo DIR`: THE HARNESS PROOF of `docs/DESIGN-harness-repos-2026-10-24.md`
//! §9 step 2, modelled on `codex-alab-proof`. A rostered harness (`<id>`, one of [`SUBJECTS`]),
//! built from its repository's checkout with the Trust toolchain that runs the lab (the toolchain
//! every harness repo pins), runs in a private headless aterm against a stand-in vendor
//! (`aterm-lab-standin`, this package's other binary) laid into a scratch atpkg store as two
//! builds; nothing of the owner's store, no vendor byte, account or server is touched, and the
//! instance is jailed off the network and out of the owner's homes.
//!
//! Each leg is judged by what aterm showed ([`crate::harness_proof_judge`]): the stand-in's frame
//! on aterm's screen, `status`, `who`'s foreground group, aterm's log and the tab's command blocks.
//! On an aterm serving contract 3:
//! 1. the proven scoped claim (`<id>@<leader> contract=3 scope=relaunch,upgrade`);
//! 2. a crash relaunch, by the harness, on the same session, aterm's built-in lane standing down;
//! 3. no move while a draft is present, none while a person types (`key|send as=person`), then
//!    the move onto the second build at a safe point, under the same harness pid;
//! 4. the harness's own re-exec onto a second build of itself: same pid, claimed again;
//! 5. signal and exit propagation: the group's `SIGINT` reaches the child and both run on; the
//!    child's exit 130, a forwarded `SIGTERM` (143) and `SIGHUP` (129) reach the shell, and nothing
//!    is relaunched after an intended end.
//!
//! THE NEGATIVE CONTROL is an aterm that answers no `contract=3` (`ATERM_DEV_NO_CONTRACT3=1`,
//! `crates/aterm-gui/src/harness_claim.rs`): the harness must pass through (the vendor in its
//! place, unclaimed) and aterm's built-in lanes must take the session (a crash relaunched by aterm's
//! resume line). It is calibrated first: the stand-in run bare must be taken by those lanes, else
//! the control is UNOBSERVED, never a harness's FAIL.
//!
//! A wait for something that must happen is a one-minute hang detector ([`MUST`]); a window in
//! which nothing may happen is sized from the harness's own clocks ([`Play`]). The worlds are always
//! kept (their logs are the proof's record), and the store's copies of the stand-in are never
//! removed by the lab.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::harness_proof_judge::{self as judge, After, Check, ClaimSeen, Frame};
use crate::instance::{self, Instance, Launch, World};
use crate::stop::{self, Stopped};
use crate::{Fail, LAB, Res};

/// A wait for something that must happen: a hang detector, never a latency budget.
const MUST: Duration = Duration::from_secs(60);

/// How often a wait or a window reads the tab.
const STEP: Duration = Duration::from_millis(500);

/// The draft a leg leaves in the composer.
const DRAFT: &str = "lab draft";

/// How a harness's clocks are shortened and what the stand-in plays for its vendor.
pub(crate) struct Play {
    /// The two vendor builds the scratch store holds: the session starts on the first, the second
    /// is landed underneath it.
    pub builds: [&'static str; 2],
    /// A vendor version's atpkg build id (its store directory's name).
    pub build_id: fn(&str) -> Option<u64>,
    /// The harness's debug-build timing seam (`<variable>`, `<value>`).
    pub timing: (&'static str, &'static str),
    /// The person grace and the settle after a turn that value sets.
    pub grace: Duration,
    pub settle: Duration,
}

/// One harness of the roster the proof can name (§1).
pub(crate) struct Subject {
    /// The holder id: repo, atpkg program and claim holder (D1).
    pub id: &'static str,
    /// The binary its repo builds and atpkg exposes (D19).
    pub tool: &'static str,
    /// The vendor's atpkg program and command: the stand-in's name.
    pub vendor: &'static str,
    /// The scope its claim names in §9 step 2 and 3.
    pub scope: &'static str,
    /// What the stand-in plays for it, `None` while it plays no such vendor.
    pub play: Option<Play>,
}

/// The harnesses `harness-proof` names: Muse first (§9 step 2); the others are named so the verb
/// says what it lacks for them.
pub(crate) static SUBJECTS: [Subject; 5] = [
    Subject {
        id: "muse-alab",
        tool: "muse-alab",
        vendor: "muse",
        scope: "relaunch,upgrade",
        play: Some(Play {
            builds: ["1.4.10-R6427.1", "1.4.13-R6996.1"],
            build_id: crate::upgrade_muse::revision_build_id,
            timing: ("MUSE_ALAB_TEST_TIMING", "100,250,4000,1500,10000"),
            grace: Duration::from_millis(4000),
            settle: Duration::from_millis(1500),
        }),
    },
    Subject {
        id: "agy-alab",
        tool: "agy-alab",
        vendor: "agy",
        scope: "relaunch,upgrade",
        play: None,
    },
    Subject {
        id: "claude-code-alab",
        tool: "claude-alab",
        vendor: "claude",
        scope: "relaunch,upgrade",
        play: None,
    },
    Subject {
        id: "grok-alab",
        tool: "grok-alab",
        vendor: "grok",
        scope: "relaunch,upgrade",
        play: None,
    },
    Subject {
        id: "kimi-alab",
        tool: "kimi-alab",
        vendor: "kimi",
        scope: "relaunch,upgrade",
        play: None,
    },
];

/// The subject `id` names.
#[must_use]
pub(crate) fn subject(id: &str) -> Option<&'static Subject> {
    SUBJECTS.iter().find(|s| s.id == id)
}

/// The subjects' ids, for a usage line.
fn subject_ids() -> String {
    SUBJECTS.iter().map(|s| s.id).collect::<Vec<_>>().join(", ")
}

/// The window in which nothing may move: the person grace, the settle, and six of the harness's
/// looks and its build probe past them.
#[must_use]
pub(crate) fn still_window(play: &Play) -> Duration {
    play.grace + play.settle + Duration::from_secs(6)
}

/// How often a person types in the typing leg: well inside the person grace, well past the settle,
/// so only the typing holds the move.
#[must_use]
pub(crate) fn typing_every(play: &Play) -> Duration {
    (play.grace / 2).max(play.settle + Duration::from_millis(300))
}

/// The harness binaries the proof runs: its plain build and a second build of a newer version.
struct Built {
    plain: PathBuf,
    next: PathBuf,
    version: String,
    next_version: String,
    head: String,
}

fn not_run(why: impl Into<String>) -> Fail {
    Fail::exit(3, format!("harness-proof: NOT RUN: {}", why.into()))
}

/// The package version a harness repo's `Cargo.toml` names (`[workspace.package]`, else
/// `[package]`).
#[must_use]
pub(crate) fn package_version(cargo_toml: &str) -> Option<String> {
    let doc: aterm_toml::Value = aterm_toml::from_str(cargo_toml).ok()?;
    let of = |table: Option<&aterm_toml::Value>| {
        table?
            .get("version")?
            .as_str()
            .map(str::to_string)
            .filter(|v| !v.is_empty())
    };
    of(doc.get("workspace").and_then(|w| w.get("package"))).or_else(|| of(doc.get("package")))
}

/// Build `tool` in `repo` into `target` with `$CARGO` (the Trust targo running the lab), with
/// `ALAB_HARNESS_VERSION` set to `version` when given (D44): the binary's path.
fn build_tool(
    repo: &Path,
    tool: &str,
    target: &Path,
    version: Option<&str>,
) -> Result<PathBuf, Fail> {
    let mut cmd =
        aterm_uds::spawnfd::command(std::env::var_os("CARGO").unwrap_or_else(|| "targo".into()));
    cmd.args([
        "--unverified",
        "build",
        "--locked",
        "--bin",
        tool,
        "--target-dir",
    ])
    .arg(target)
    .args(["--message-format", "json-render-diagnostics"])
    .current_dir(repo)
    .stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(atpkg::build_cache::said_by_cargo_run)
        {
            cmd.env_remove(name);
        }
    }
    cmd.env_remove("CARGO_TARGET_DIR");
    match version {
        Some(v) => cmd.env("ALAB_HARNESS_VERSION", v),
        None => cmd.env_remove("ALAB_HARNESS_VERSION"),
    };
    let out = cmd
        .output()
        .map_err(|e| not_run(format!("targo in {}: {e}", repo.display())))?;
    let json = String::from_utf8_lossy(&out.stdout);
    crate::mac_nux::bin_executable(&json, tool)
        .filter(|_| out.status.success())
        .ok_or_else(|| {
            not_run(format!(
                "{tool} did not build in {} (targo --unverified build --locked --bin {tool}): {}",
                repo.display(),
                crate::util::one_line(String::from_utf8_lossy(&out.stderr).trim(), 600)
            ))
        })
}

/// Build the harness twice from `repo`: plain, and as `<version>+g<HEAD's first 9>` (a second
/// build of a newer version, for its own re-exec), each in its own `*.noindex` target under
/// `target`.
fn build_harness(s: &Subject, repo: &Path, target: &Path) -> Result<Built, Fail> {
    let toml = std::fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| not_run(format!("{}: {e}", repo.join("Cargo.toml").display())))?;
    let version = package_version(&toml)
        .ok_or_else(|| not_run(format!("{} names no package version", repo.display())))?;
    let head = aterm_uds::spawnfd::command("/usr/bin/git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| h.len() >= 9 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| not_run(format!("{} is no git checkout", repo.display())))?;
    let next_version = format!("{version}+g{}", &head[..9]);
    println!(
        "harness-proof: building {} ({version}, and {next_version}) from {} at {head}",
        s.tool,
        repo.display()
    );
    let plain = build_tool(repo, s.tool, &target.join("plain.noindex"), None)?;
    let next = build_tool(
        repo,
        s.tool,
        &target.join("next.noindex"),
        Some(&next_version),
    )?;
    Ok(Built {
        plain,
        next,
        version,
        next_version,
        head,
    })
}

/// Build the stand-in vendor (`aterm-lab-standin`) as the lab builds its other helper.
fn build_standin() -> Result<PathBuf, Fail> {
    const BIN: &str = "aterm-lab-standin";
    let out = crate::util::targo(&[
        "--unverified",
        "build",
        "-p",
        "aterm-lab",
        "--bin",
        BIN,
        "--message-format",
        "json-render-diagnostics",
    ])
    .stdin(Stdio::null())
    .output()
    .map_err(|e| not_run(format!("targo: {e}")))?;
    crate::mac_nux::bin_executable(&String::from_utf8_lossy(&out.stdout), BIN)
        .filter(|_| out.status.success())
        .ok_or_else(|| {
            not_run(format!(
                "the stand-in did not build (targo --unverified build -p aterm-lab --bin {BIN}): {}",
                crate::util::one_line(String::from_utf8_lossy(&out.stderr).trim(), 600)
            ))
        })
}

/// One leg's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    /// Nothing the lab could read judges it (a calibration failed, a seam is missing).
    Unobserved,
}

/// What the proof found, leg by leg.
#[derive(Default)]
struct Report {
    legs: Vec<(String, Verdict)>,
}

impl Report {
    fn leg(&mut self, name: &str, checks: &[Check]) -> bool {
        let ok = judge::passed(checks);
        println!("  {} {name}", if ok { "PASS" } else { "FAIL" });
        for c in checks {
            println!("      {} {}", if c.ok { "ok  " } else { "FAIL" }, c.label);
        }
        self.legs.push((
            name.to_string(),
            if ok { Verdict::Pass } else { Verdict::Fail },
        ));
        ok
    }

    fn unobserved(&mut self, name: &str, why: &str) {
        println!("  ---- {name}: UNOBSERVED ({why})");
        self.legs.push((name.to_string(), Verdict::Unobserved));
    }
}

/// The scratch world of one instance, its store and the instance.
struct Rig {
    s: &'static Subject,
    play: &'static Play,
    inst: Instance,
    world: World,
    prefix: PathBuf,
    proj: PathBuf,
    standin: PathBuf,
    standin_log: PathBuf,
}

/// The instance's sandbox: no network at all, and the owner's homes fenced
/// ([`crate::codex_rig::home_guards`]).
fn policy(scratch: &Path) -> Result<String, String> {
    let homes = crate::paths::paths().need_guarded_homes()?;
    if let Some(h) = homes.iter().find(|h| scratch.starts_with(h.as_str())) {
        return Err(format!(
            "its scratch {} lies in the owner's home {h}, which the sandbox fences",
            scratch.display()
        ));
    }
    Ok(format!(
        "(version 1)(allow default)(deny network-outbound (remote ip))(deny network-inbound (local ip))\
         (deny network-outbound (literal \"/private/var/run/mDNSResponder\"))\n{}{}",
        crate::jail::PREFERENCES,
        crate::codex_rig::home_guards(&homes)?
    ))
}

impl Rig {
    /// Lay a world (the harness host on, Autopilot's default), its scratch store holding the
    /// vendor's first build and the harness's plain build, and start a headless jailed aterm in it,
    /// one answering as an aterm before contract 3 when `serves` is false.
    fn start(
        s: &'static Subject,
        play: &'static Play,
        aterm: &Path,
        standin: &Path,
        harness: &Path,
        serves: bool,
    ) -> Result<Rig, Fail> {
        let mut world = World::new(
            "agents_auto_prime = false\n[update]\nenabled = false\nauto_apply = false\n\
             [packages]\nenabled = false\n[machine]\nuniversal_control = \"leave\"\n\
             [harness]\nenabled = true\nheadless = true\nhuman_grace_s = 5\n",
        )?;
        world.keep();
        let dir = world.dir.clone();
        let prefix = world.home.join("Library/Application Support/aterm/pkg");
        let proj = dir.join("proj");
        let standin_log = dir.join("standin.log");
        let made = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir.join("zdot"))?;
            std::fs::create_dir_all(&proj)?;
            let q = |p: &Path| crate::util::shell_quote(&p.to_string_lossy());
            std::fs::write(
                dir.join("zdot/.zshrc"),
                format!(
                    "PS1='%# '\nunsetopt PROMPT_SP\nHISTFILE=/dev/null\n\
                     export PATH={}:{}:/usr/bin:/bin:/usr/sbin:/sbin\n\
                     export ATERM_LAB_STANDIN_LOG={}\nexport {}={}\n",
                    q(&prefix.join("bin")),
                    q(&prefix.join("agents")),
                    q(&standin_log),
                    play.timing.0,
                    crate::util::shell_quote(play.timing.1),
                ),
            )
        })();
        made.map_err(|e| {
            Fail::die(format!(
                "harness-proof: cannot lay out {}: {e}",
                dir.display()
            ))
        })?;
        let sandbox = policy(&dir).map_err(|e| Fail::die(format!("harness-proof: {e}")))?;
        let sstr = |p: &Path| p.to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> = vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("SHELL".into(), "/bin/zsh".into()),
            ("HOME".into(), sstr(&world.home)),
            ("XDG_CONFIG_HOME".into(), sstr(&world.cfg)),
            ("XDG_RUNTIME_DIR".into(), sstr(&world.run)),
            ("ZDOTDIR".into(), sstr(&dir.join("zdot"))),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
        ];
        env.extend(instance::seams());
        if !serves {
            env.push(("ATERM_DEV_NO_CONTRACT3".into(), "1".into()));
        }
        let mut inst = Instance::start(
            &world,
            Launch {
                aterm: aterm.to_path_buf(),
                headless: true,
                args: ["--columns", "120", "--lines", "40"]
                    .map(str::to_string)
                    .to_vec(),
                env,
                cwd: dir.clone(),
                sandbox: Some(sandbox),
                reroute: false,
            },
        )?;
        inst.await_socket(instance::BOOT)?;
        let rig = Rig {
            s,
            play,
            inst,
            world,
            prefix,
            proj,
            standin: standin.to_path_buf(),
            standin_log,
        };
        rig.land_vendor(play.builds[0])
            .map_err(|e| Fail::die(format!("harness-proof: the vendor's first build: {e}")))?;
        rig.land_harness(harness, 1)
            .map_err(|e| Fail::die(format!("harness-proof: the harness's build: {e}")))?;
        Ok(rig)
    }

    fn store(&self, program: &str) -> PathBuf {
        self.prefix.join("store").join(program)
    }

    /// Point `program`'s `current` at `build`, as atpkg swaps it (a new link renamed over).
    fn swap_current(&self, program: &str, build: u64) -> Result<(), String> {
        let store = self.store(program);
        let tmp = store.join(format!(".current-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(build.to_string(), &tmp).map_err(|e| format!("current: {e}"))?;
        let current = store.join("current");
        std::fs::rename(&tmp, &current).map_err(|e| format!("current: {e}"))?;
        let landed = current
            .canonicalize()
            .map_err(|e| format!("current: {e}"))?;
        let expected = store
            .join(build.to_string())
            .canonicalize()
            .map_err(|e| format!("build {build}: {e}"))?;
        if landed != expected {
            return Err(format!("current did not select build {build}"));
        }
        Ok(())
    }

    fn notice(&self, program: &str) -> Result<(), String> {
        crate::sys::put(
            &self.prefix.join("activation-notices").join(program),
            0o600,
            &crate::sys::PutSrc::Bytes(b"1\n"),
        )
        .map_err(|e| format!("the activation notice: {e}"))
    }

    /// LAND vendor build `version` as atpkg lands one: the stand-in as its tree, its record and
    /// `.ready` (atpkg's own marker), `current` swapped, the `agents/` twin rewritten, and the
    /// activation notice published.
    fn land_vendor(&self, version: &str) -> Result<PathBuf, String> {
        let v = self.s.vendor;
        let id = (self.play.build_id)(version).ok_or("no build id")?;
        let build = self.store(v).join(id.to_string());
        let bin = build.join("bin").join(v);
        crate::sys::put(&bin, 0o755, &crate::sys::PutSrc::File(&self.standin))
            .map_err(|e| format!("the build's tree: {e}"))?;
        crate::sys::put(
            &self.store(v).join(format!("{id}.vendor")),
            0o644,
            &crate::sys::PutSrc::Bytes(
                format!("schema = 1\nprogram = \"{v}\"\nversion = \"{version}\"\n").as_bytes(),
            ),
        )
        .map_err(|e| format!("its record: {e}"))?;
        atpkg::store::mark_build_ready(&build).map_err(|e| format!("its .ready: {e}"))?;
        self.swap_current(v, id)?;
        crate::sys::put(
            &self.prefix.join("agents").join(v),
            0o755,
            &crate::sys::PutSrc::Bytes(
                crate::relaunch_muse::twin_script(&bin.to_string_lossy()).as_bytes(),
            ),
        )
        .map_err(|e| format!("the twin: {e}"))?;
        self.notice(v)?;
        Ok(bin)
    }

    /// LAND the harness binary `exe` as its program's build `build`: the tree, atpkg's `.ready`,
    /// `current` swapped, the `bin/<tool>` shim rewritten, and the activation notice.
    fn land_harness(&self, exe: &Path, build: u64) -> Result<PathBuf, String> {
        let (pkg, tool) = (self.s.id, self.s.tool);
        let dir = self.store(pkg).join(build.to_string());
        let bin = dir.join("bin").join(tool);
        crate::sys::put(&bin, 0o755, &crate::sys::PutSrc::File(exe))
            .map_err(|e| format!("the build's tree: {e}"))?;
        atpkg::store::mark_build_ready(&dir).map_err(|e| format!("its .ready: {e}"))?;
        self.swap_current(pkg, build)?;
        crate::sys::put(
            &self.prefix.join("bin").join(tool),
            0o755,
            &crate::sys::PutSrc::Bytes(
                crate::relaunch_muse::twin_script(&bin.to_string_lossy()).as_bytes(),
            ),
        )
        .map_err(|e| format!("the shim: {e}"))?;
        self.notice(pkg)?;
        Ok(bin)
    }

    fn on(&self, sid: &str, args: &[&str]) -> instance::Reply {
        let at = format!("@{sid}");
        let mut v = vec![at.as_str()];
        v.extend_from_slice(args);
        self.inst.ctl(&v)
    }

    /// A new tab in the project folder, its shell at its prompt.
    fn open_tab(&self) -> Result<String, Fail> {
        let spawned = self
            .inst
            .ctl(&["spawn", &format!("cwd={}", self.proj.display())]);
        let sid = spawned
            .first()
            .strip_prefix("OK ")
            .and_then(|r| r.split_whitespace().next())
            .map(str::to_string)
            .ok_or_else(|| Fail::die(format!("harness-proof: spawn: {}", spawned.first())))?;
        let _ = self.on(&sid, &["await", "idle", "400", "timeout=30000"]);
        Ok(sid)
    }

    /// Type `line` and Enter at the tab's shell.
    fn type_line(&self, sid: &str, line: &str) -> Result<(), Fail> {
        let sent = self.inst.ctl_stdin(
            &[&format!("@{sid}"), "send", "--stdin"],
            format!("{line}\r").as_bytes(),
        );
        if sent.code == 0 && !sent.stdout.starts_with("ERR") {
            Ok(())
        } else {
            Err(Fail::die(format!(
                "harness-proof: `{line}` was not typed: {}",
                sent.first()
            )))
        }
    }

    fn screen(&self, sid: &str) -> String {
        self.on(sid, &["text"]).stdout
    }

    fn frame(&self, sid: &str) -> Option<Frame> {
        judge::frame_of(&self.screen(sid))
    }

    fn seen(&self, sid: &str) -> ClaimSeen {
        judge::claim_seen(&self.on(sid, &["status"]).first())
    }

    fn leader(&self, sid: &str) -> Option<u32> {
        judge::fgpgid(&self.inst.ctl(&["who"]).stdout, sid)
    }

    fn blocks(&self, sid: &str) -> Vec<judge::Block> {
        judge::blocks_of(&self.on(sid, &["blocks", "20", "--json"]).stdout)
    }

    fn aterm_log(&self) -> String {
        crate::util::scratch_text(&self.world.home.join("Library/Logs/aterm/aterm.log"))
    }

    /// aterm's log past byte `from` (a length an earlier read took).
    fn log_since(&self, from: usize) -> String {
        let log = self.aterm_log();
        log.get(from..).unwrap_or_default().to_string()
    }

    /// How many starts the stand-in has logged.
    fn starts(&self) -> usize {
        crate::util::scratch_text(&self.standin_log)
            .lines()
            .filter(|l| l.split_whitespace().nth(1) == Some("start"))
            .count()
    }

    /// The tab's harness acts as aterm's timeline books them (informational: aterm books acts
    /// only from a proven v2 self-claim today).
    fn acts(&self, sid: &str) -> Vec<String> {
        self.on(sid, &["timeline"])
            .stdout
            .lines()
            .filter(|l| l.contains("kind=harness-act"))
            .filter_map(|l| crate::screen::field(l, "act"))
            .collect()
    }

    /// Wait up to `limit` for a frame `ok` takes.
    fn wait_frame(
        &self,
        sid: &str,
        limit: Duration,
        ok: impl Fn(&Frame) -> bool,
    ) -> Result<Option<Frame>, Stopped> {
        let end = Instant::now() + limit;
        loop {
            if let Some(f) = self.frame(sid).filter(|f| ok(f)) {
                return Ok(Some(f));
            }
            if Instant::now() >= end {
                return Ok(None);
            }
            stop::sleep(STEP)?;
        }
    }

    /// Wait up to `limit` for `ok`.
    fn wait_for(limit: Duration, mut ok: impl FnMut() -> bool) -> Result<bool, Stopped> {
        let end = Instant::now() + limit;
        loop {
            if ok() {
                return Ok(true);
            }
            if Instant::now() >= end {
                return Ok(false);
            }
            stop::sleep(STEP)?;
        }
    }

    /// Read the tab's frame every [`STEP`] for `window`.
    fn reads(&self, sid: &str, window: Duration) -> Result<Vec<Option<Frame>>, Stopped> {
        let end = Instant::now() + window;
        let mut reads = Vec::new();
        while Instant::now() < end {
            reads.push(self.frame(sid));
            stop::sleep(STEP)?;
        }
        Ok(reads)
    }

    /// Start the harness at the tab's shell and wait for the stand-in's first frame under it.
    fn start_harness(&self, sid: &str) -> Result<Option<Frame>, Fail> {
        let old = self.frame(sid).map(|f| f.pid);
        self.type_line(sid, self.s.tool)?;
        Ok(self.wait_frame(sid, MUST, |f| Some(f.pid) != old)?)
    }

    /// Whether the stand-in started again (a start logged past `starts`) within the harness's
    /// look window after an intended end.
    fn relaunched_after(&self, starts: usize) -> Result<bool, Stopped> {
        stop::sleep(self.play.settle + Duration::from_secs(3))?;
        Ok(self.starts() > starts)
    }

    /// Wait for the tab's newest block running the harness to end, and read its exit.
    fn harness_exit(&self, sid: &str) -> Result<Option<i64>, Stopped> {
        let tool = self.s.tool;
        let mut exit = None;
        Self::wait_for(MUST, || {
            exit = judge::exit_of(&self.blocks(sid), tool);
            exit.is_some()
        })?;
        Ok(exit)
    }

    /// The harness's own journal, where its repo keeps one (`$XDG_STATE_HOME/<id>/journal`).
    fn journal_tail(&self) -> String {
        let text = crate::util::scratch_text(
            &self
                .world
                .home
                .join(".local/state")
                .join(self.s.id)
                .join("journal"),
        );
        tail(&text, 12)
    }

    fn diagnose(&self, sid: &str) {
        println!("    --- what the tab showed ({sid}):");
        for l in tail(&self.screen(sid), 12).lines() {
            println!("    | {l}");
        }
        println!("    --- the stand-in's log:");
        for l in tail(&crate::util::scratch_text(&self.standin_log), 10).lines() {
            println!("    | {l}");
        }
        let journal = self.journal_tail();
        if !journal.is_empty() {
            println!("    --- {}'s journal:", self.s.id);
            for l in journal.lines() {
                println!("    | {l}");
            }
        }
        let at = format!("@{sid}:");
        let host: Vec<String> = self
            .aterm_log()
            .lines()
            .filter(|l| l.contains(&at))
            .map(str::to_string)
            .collect();
        println!("    --- aterm's lines for {sid}:");
        for l in host.iter().rev().take(10).rev() {
            println!("    | {}", crate::util::one_line(l, 220));
        }
    }

    fn end(mut self) -> PathBuf {
        self.inst.stop();
        self.world.dir.clone()
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The legs on an aterm serving contract 3.
#[allow(clippy::too_many_lines)]
fn serving_legs(rig: &Rig, built: &Built, r: &mut Report) -> Result<(), Fail> {
    let s = rig.s;
    let [_, second] = rig.play.builds;
    let sid = rig.open_tab()?;
    println!("  tab {sid}: {}", s.tool);
    let rostered = aterm_agent::harness::contract::row_named(s.id).is_some();
    let Some(first) = rig.start_harness(&sid)? else {
        for leg in LEGS {
            r.leg(
                leg,
                &[Check {
                    label: format!(
                        "the stand-in came up under {} (it never showed its frame)",
                        s.tool
                    ),
                    ok: false,
                }],
            );
        }
        rig.diagnose(&sid);
        return Ok(());
    };
    let leader = rig.leader(&sid);
    println!(
        "  the stand-in runs {} as pid {} on session {}; the job's leader is {}",
        first.version,
        first.pid,
        first.session,
        leader.map_or_else(|| "unread".to_string(), |p| p.to_string())
    );

    // 1. The proven scoped claim.
    r.leg(
        LEGS[0],
        &judge::claim(
            s.id,
            s.scope,
            rostered,
            &rig.seen(&sid),
            leader,
            &judge::held_lines(&rig.aterm_log(), &sid),
        ),
    );

    // 2. A crash relaunch.
    let mark = rig.aterm_log().len();
    rig.type_line(&sid, "/crash")?;
    let back = rig.wait_frame(&sid, MUST, |f| f.pid != first.pid)?;
    let builtin = !judge::builtin_acts(&rig.log_since(mark), &sid, "relaunch").is_empty()
        || judge::resume_typed(&rig.blocks(&sid), &first.session);
    r.leg(
        LEGS[1],
        &judge::relaunch(
            s.id,
            &first,
            &After {
                frame: back.as_ref(),
                leader: (leader, rig.leader(&sid)),
                seen: rig.seen(&sid),
            },
            builtin,
        ),
    );
    println!(
        "      info: aterm booked the harness's acts {:?}",
        rig.acts(&sid)
    );
    let Some(base) = (match back {
        Some(f) => Some(f),
        None => rig.start_harness(&sid)?,
    }) else {
        rig.diagnose(&sid);
        for leg in &LEGS[2..] {
            r.unobserved(leg, "no stand-in runs under the harness after the crash");
        }
        return Ok(());
    };
    let leader = rig.leader(&sid);

    // 3. No move while a draft or a person's typing is present; then the move.
    let sent = rig.on(&sid, &["send", DRAFT]);
    let drafted = rig.wait_frame(&sid, MUST, |f| f.composer == DRAFT)?;
    let mark = rig.aterm_log().len();
    rig.land_vendor(second)
        .map_err(|e| Fail::die(format!("harness-proof: the vendor's second build: {e}")))?;
    match drafted {
        Some(with_draft) => {
            let reads = rig.reads(&sid, still_window(rig.play))?;
            r.leg(
                LEGS[2],
                &judge::held_still("a draft is present", &with_draft, &reads, Some(DRAFT)),
            );
        }
        None => r.unobserved(
            LEGS[2],
            &format!("the draft never showed in the composer ({})", sent.first()),
        ),
    }
    let _ = rig.on(&sid, &["key", "enter"]);
    let _ = rig.wait_frame(&sid, MUST, |f| f.composer.is_empty())?;
    let typing = rig.frame(&sid).unwrap_or_else(|| base.clone());
    let end = Instant::now() + still_window(rig.play);
    let mut reads = Vec::new();
    let mut refused = None;
    while Instant::now() < end {
        let typed = rig.on(&sid, &["send", "as=person", "hi"]);
        let pressed = rig.on(&sid, &["key", "as=person", "enter"]);
        if typed.first().starts_with("ERR") || pressed.first().starts_with("ERR") {
            refused = Some(format!("{} / {}", typed.first(), pressed.first()));
            break;
        }
        let next = Instant::now() + typing_every(rig.play);
        while Instant::now() < next {
            reads.push(rig.frame(&sid));
            stop::sleep(STEP)?;
        }
    }
    match refused {
        Some(why) => r.unobserved(LEGS[3], &format!("aterm took no person's keys: {why}")),
        None => {
            r.leg(
                LEGS[3],
                &judge::held_still("a person is typing", &typing, &reads, None),
            );
        }
    }
    let onto = rig.wait_frame(&sid, MUST, |f| f.version == second)?;
    let builtin = !judge::builtin_acts(&rig.log_since(mark), &sid, "upgrade").is_empty();
    r.leg(
        LEGS[4],
        &judge::moved(
            s.id,
            &typing,
            second,
            &After {
                frame: onto.as_ref(),
                leader: (leader, rig.leader(&sid)),
                seen: rig.seen(&sid),
            },
            builtin,
        ),
    );
    println!(
        "      info: aterm booked the harness's acts {:?}",
        rig.acts(&sid)
    );

    // 4. The harness's own re-exec.
    let child = rig.frame(&sid).unwrap_or_else(|| typing.clone());
    let leader = rig.leader(&sid);
    let mark = rig.aterm_log().len();
    let new_build = rig
        .land_harness(&built.next, 2)
        .map_err(|e| Fail::die(format!("harness-proof: the harness's second build: {e}")))?;
    let new_dir = new_build
        .parent()
        .and_then(Path::parent)
        .and_then(|d| std::fs::canonicalize(d).ok());
    let on_new = |pid: Option<u32>| {
        pid.and_then(|p| i32::try_from(p).ok())
            .and_then(aterm_uds::sudo::exe_path)
            .and_then(|e| std::fs::canonicalize(e).ok())
            .zip(new_dir.as_ref())
            .is_some_and(|(e, d)| e.starts_with(d))
    };
    let next = built.next_version.as_str();
    Rig::wait_for(MUST, || {
        on_new(rig.leader(&sid))
            || judge::held_lines(&rig.log_since(mark), &sid)
                .iter()
                .any(|h| h.versions.starts_with(&format!("{} {next} ", s.id)))
    })?;
    let after_leader = rig.leader(&sid);
    r.leg(
        LEGS[5],
        &judge::reexec(
            s.id,
            next,
            on_new(after_leader),
            &judge::held_lines(&rig.log_since(mark), &sid),
            (&child, rig.frame(&sid).as_ref()),
            &After {
                frame: None,
                leader: (leader, after_leader),
                seen: rig.seen(&sid),
            },
        ),
    );

    // 5. Signal and exit propagation.
    let before = rig.frame(&sid).unwrap_or(child);
    let leader = rig.leader(&sid);
    let _ = rig.on(&sid, &["signal", "int"]);
    let said = Rig::wait_for(MUST, || judge::said_signal(&rig.screen(&sid), "SIGINT"))?;
    r.leg(
        LEGS[6],
        &judge::interrupted(
            &before,
            &After {
                frame: rig.frame(&sid).as_ref(),
                leader: (leader, rig.leader(&sid)),
                seen: rig.seen(&sid),
            },
            said,
        ),
    );
    let starts = rig.starts();
    rig.type_line(&sid, "/exit 130")?;
    let exit = rig.harness_exit(&sid)?;
    let again = rig.relaunched_after(starts)?;
    r.leg(
        LEGS[7],
        &judge::ended("the child's own exit 130", exit, 130, None, again),
    );
    for (leg, word, name, want) in [
        (LEGS[8], "term", "SIGTERM", 143),
        (LEGS[9], "hup", "SIGHUP", 129),
    ] {
        let Some(_) = rig.start_harness(&sid)? else {
            r.unobserved(leg, &format!("{} did not start again", s.tool));
            continue;
        };
        let Some(pid) = rig.leader(&sid) else {
            r.unobserved(leg, "the job's leader was not read");
            continue;
        };
        let starts = rig.starts();
        let sent = rig.on(&sid, &["signal", word, &format!("pid={pid}")]);
        let exit = rig.harness_exit(&sid)?;
        let again = rig.relaunched_after(starts)?;
        let said = judge::said_signal(&rig.screen(&sid), name);
        r.leg(
            leg,
            &judge::ended(
                &format!("{name} to the harness, pid {pid} ({})", sent.first()),
                exit,
                want,
                Some((name, said)),
                again,
            ),
        );
    }
    if r.legs.iter().any(|(_, v)| *v != Verdict::Pass) {
        rig.diagnose(&sid);
    }
    Ok(())
}

/// The legs of the serving aterm, in order.
const LEGS: [&str; 10] = [
    "the proven scoped claim",
    "a crash relaunch by the harness",
    "no move while a draft is present",
    "no move while a person is typing",
    "the move to the new build at a safe point",
    "the harness's own re-exec",
    "SIGINT reaches the child and both run on",
    "the child's exit status reaches the shell",
    "SIGTERM is forwarded and its status reaches the shell",
    "SIGHUP is forwarded and its status reaches the shell",
];

/// The negative control on an aterm serving no contract 3: calibrated on the stand-in run bare,
/// then the harness.
fn control_legs(rig: &Rig, r: &mut Report) -> Result<(), Fail> {
    let s = rig.s;
    let cal = rig.open_tab()?;
    println!("  tab {cal}: {} bare (calibration)", s.vendor);
    rig.type_line(&cal, s.vendor)?;
    let bare = rig.wait_frame(&cal, MUST, |_| true)?;
    let calibrated = match &bare {
        None => Err("the stand-in run bare never showed its frame".to_string()),
        Some(f) => {
            let program = rig.seen(&cal).program;
            rig.type_line(&cal, "/crash")?;
            let back = rig.wait_frame(&cal, MUST, |g| g.pid != f.pid)?;
            let typed = judge::resume_typed(&rig.blocks(&cal), &f.session);
            if program.as_deref() == Some(s.vendor) && typed && back.is_some() {
                Ok(())
            } else {
                Err(format!(
                    "aterm named it {} and its built-in lane {} the bare stand-in",
                    program.as_deref().unwrap_or("-"),
                    if typed && back.is_some() {
                        "relaunched"
                    } else {
                        "did not relaunch"
                    }
                ))
            }
        }
    };
    if let Err(why) = calibrated {
        rig.diagnose(&cal);
        r.unobserved(CONTROL[0], &why);
        r.unobserved(CONTROL[1], &why);
        return Ok(());
    }
    println!(
        "  calibrated: aterm names the bare stand-in {} and its built-in lane relaunches it",
        s.vendor
    );
    let sid = rig.open_tab()?;
    println!("  tab {sid}: {}", s.tool);
    let Some(first) = rig.start_harness(&sid)? else {
        r.leg(
            CONTROL[0],
            &[Check {
                label: format!("the vendor came up under {}", s.tool),
                ok: false,
            }],
        );
        rig.diagnose(&sid);
        r.unobserved(CONTROL[1], "no vendor runs in the tab");
        return Ok(());
    };
    let through = r.leg(
        CONTROL[0],
        &judge::passed_through(s.vendor, &rig.seen(&sid), rig.leader(&sid), Some(&first)),
    );
    rig.type_line(&sid, "/crash")?;
    let back = rig.wait_frame(&sid, MUST, |f| f.pid != first.pid)?;
    let typed = judge::resume_typed(&rig.blocks(&sid), &first.session);
    let took = r.leg(
        CONTROL[1],
        &judge::builtin_took(&first, typed, back.as_ref()),
    );
    if !(through && took) {
        rig.diagnose(&sid);
    }
    Ok(())
}

/// The negative control's legs.
const CONTROL: [&str; 2] = [
    "no contract=3 served: the harness passes through",
    "no contract=3 served: aterm's built-in lanes take the session",
];

/// `harness-proof <id> --repo DIR [--aterm PATH] [--target-dir DIR]`.
/// # Errors
/// A usage error, NOT RUN (exit 3) when the subject, its checkout, a build or the instance is
/// missing, or an interrupt.
pub fn cmd_harness_proof(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let aterm = instance::take_value(&mut a, "--aterm")?;
    let repo = instance::take_value(&mut a, "--repo")?;
    let target = instance::take_value(&mut a, "--target-dir")?;
    let Some(id) = a.first().cloned() else {
        return Err(Fail::usage(format!(
            "harness-proof: name a harness ({}; {LAB} help)",
            subject_ids()
        )));
    };
    if let Some(extra) = a.get(1) {
        return Err(Fail::usage(format!(
            "harness-proof: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let s = subject(&id).ok_or_else(|| {
        Fail::usage(format!(
            "harness-proof: no harness `{id}` (one of {})",
            subject_ids()
        ))
    })?;
    let play = s.play.as_ref().ok_or_else(|| {
        not_run(format!(
            "the stand-in plays no {} yet, so {} has no proof here",
            s.vendor, s.id
        ))
    })?;
    let aterm = instance::resolve_aterm(aterm).map_err(not_run)?;
    let repo = repo
        .map(|r| PathBuf::from(crate::paths::from_caller(&r)))
        .ok_or_else(|| {
            not_run(format!(
                "pass --repo <a checkout of alabsystems/{}> (gh repo clone alabsystems/{})",
                s.id, s.id
            ))
        })?;
    let target = target.map_or_else(
        || {
            crate::paths::paths()
                .state
                .join(format!("harness-proof-{}.noindex", s.id))
        },
        |t| PathBuf::from(crate::paths::from_caller(&t)),
    );
    if !target.to_string_lossy().ends_with(".noindex") {
        return Err(Fail::usage(format!(
            "harness-proof: --target-dir {} must name a *.noindex folder (AGENTS.md)",
            target.display()
        )));
    }
    let row = aterm_agent::harness::contract::row_named(s.id);
    println!(
        "harness-proof {}: aterm {}; its contract 3 roster {}",
        s.id,
        aterm.display(),
        row.map_or_else(
            || format!(
                "does NOT name {} (rows: {}), so its claim is a foreign one aterm never proves",
                s.id,
                aterm_agent::harness::contract::ROSTER
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            |r| format!("names it for {:?}", r.allows)
        )
    );
    let standin = build_standin()?;
    let built = build_harness(s, &repo, &target)?;
    println!(
        "harness-proof: {} {} at {}; {} at {}; the stand-in at {}",
        s.tool,
        built.version,
        built.plain.display(),
        built.next_version,
        built.next.display(),
        standin.display()
    );
    let mut r = Report::default();
    println!("serving aterm (contract=2,3):");
    let rig = Rig::start(s, play, &aterm, &standin, &built.plain, true)?;
    let outcome = serving_legs(&rig, &built, &mut r);
    let kept = rig.end();
    println!("  kept {}", kept.display());
    outcome?;
    println!("negative control (ATERM_DEV_NO_CONTRACT3=1: no contract= served):");
    let rig = Rig::start(s, play, &aterm, &standin, &built.plain, false)?;
    let outcome = control_legs(&rig, &mut r);
    let kept = rig.end();
    println!("  kept {}", kept.display());
    outcome?;
    let failed: Vec<&str> = r
        .legs
        .iter()
        .filter(|(_, v)| *v == Verdict::Fail)
        .map(|(n, _)| n.as_str())
        .collect();
    let unobserved: Vec<&str> = r
        .legs
        .iter()
        .filter(|(_, v)| *v == Verdict::Unobserved)
        .map(|(n, _)| n.as_str())
        .collect();
    println!(
        "harness-proof {} {} ({}): {} of {} legs pass",
        s.id,
        built.version,
        &built.head[..12.min(built.head.len())],
        r.legs.len() - failed.len() - unobserved.len(),
        r.legs.len()
    );
    for n in &failed {
        println!("  FAIL {n}");
    }
    for n in &unobserved {
        println!("  UNOBSERVED {n}");
    }
    if failed.is_empty() && unobserved.is_empty() {
        println!("PASS");
        Ok(0)
    } else {
        println!("FAIL");
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rostered harness is a subject, named once; Muse is played; the windows hold the
    /// harness's own clocks.
    #[test]
    fn the_subjects_cover_the_roster_and_size_their_windows_from_the_harness() {
        for row in aterm_agent::harness::contract::ROSTER {
            assert!(subject(row.id).is_some(), "{} is rostered", row.id);
        }
        let ids: Vec<&str> = SUBJECTS.iter().map(|s| s.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "{ids:?}");
        assert!(subject("codex-alab").is_none(), "codex-alab proves itself");
        let muse = subject("muse-alab")
            .and_then(|s| s.play.as_ref())
            .expect("muse plays");
        let [a, b] = muse.builds;
        let (ia, ib) = (
            (muse.build_id)(a).expect("a build id"),
            (muse.build_id)(b).expect("a build id"),
        );
        assert!(ib > ia, "the second build is newer by atpkg's order");
        assert!(still_window(muse) > muse.grace + muse.settle);
        let every = typing_every(muse);
        assert!(every < muse.grace && every > muse.settle, "{every:?}");
        assert!(
            muse.timing
                .1
                .split(',')
                .nth(2)
                .and_then(|g| g.parse::<u64>().ok())
                == Some(u64::try_from(muse.grace.as_millis()).unwrap_or(0)),
            "the grace is the one the seam sets"
        );
    }

    #[test]
    fn a_harness_repos_version_is_its_workspace_package_version() {
        assert_eq!(
            package_version("[workspace.package]\nversion = \"0.1.0\"\n[package]\nname = \"x\"\nversion.workspace = true\n")
                .as_deref(),
            Some("0.1.0")
        );
        assert_eq!(
            package_version("[package]\nname = \"x\"\nversion = \"0.2.0\"\n").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(package_version("[package]\nname = \"x\"\n"), None);
        assert_eq!(package_version("not toml ["), None);
    }
}
