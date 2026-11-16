// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `launch-trust [--answer FILE]`: the folder trust a launch settles before Claude Code starts
//! (`aterm_agent::harness::launch_trust`), run against the real Claude Code in a lab of its own.
//!
//! The lab reroutes and seeds no trust, so `claude` typed at the tab's prompt runs the reroute
//! stub, atpkg's twin, then the clone of the store build. The lab's twin names no aterm (the
//! lab's own process has no atpkg beside it, so its twin is laid without the block that asks
//! `aterm harness tap --launch`), so each case types that verb itself, as the block spells it:
//! the launch's own arguments after `--`, its stdin `/dev/null`, its stderr dropped, and
//! `ATERM_LAUNCH_STDOUT_TTY` saying whether the launch's output is a terminal. Then it types
//! `claude`.
//!
//! Each case is a folder Claude Code holds no trust record for. What a case proves is read from
//! files, not from a glimpse of the screen: Claude Code's own profile (the folder's trust key,
//! which aterm never writes) and the session's approvals ledger, whose one row for the folder
//! says whether the trust was recorded before the launch (`box_seq` 0) or its dialog was
//! answered on the screen.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::launch_trust as trust;

use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::Run;
use crate::{Fail, Res};

/// The folder-trust dialog's title, as Claude Code draws it.
const DIALOG: &str = "Accessing workspace";

/// A row of a drawn composer's footer, or the dialog: what a launch shows once it has started.
const STARTED: &str = "Accessing.workspace|shift.tab|for.agents|shortcuts";

/// A row of a drawn composer's footer.
const COMPOSER: &str = "shift.tab|for.agents|shortcuts";

/// The launch's own arguments in every case: a permission mode typed, so the twin's default
/// (`--dangerously-skip-permissions`, whose warning is a dialog of its own) does not apply.
const ARGS: [&str; 2] = ["--permission-mode", "default"];

/// The twin's word for a launch whose output is a terminal, and for one whose output is not.
const TTY: &str = "ATERM_LAUNCH_STDOUT_TTY=1";
const NO_TTY: &str = "ATERM_LAUNCH_STDOUT_TTY=0";

/// How long a wait for something that must happen may take before it is a hang.
const HANG_S: u64 = 60;

/// What `launch-trust` was asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// `--answer FILE`: where the headless start's answer line is kept.
    pub answer: Option<PathBuf>,
}

impl Opts {
    /// Parse `launch-trust`'s options.
    /// # Errors
    /// An unknown option, or `--answer` with no file.
    pub fn parse(args: &[String]) -> Result<Opts, String> {
        let mut o = Opts::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--answer" => {
                    o.answer = Some(PathBuf::from(
                        it.next().ok_or("launch-trust: --answer needs a file")?,
                    ));
                }
                other => {
                    return Err(format!(
                        "launch-trust: unknown option '{other}' (use --answer FILE)"
                    ));
                }
            }
        }
        Ok(o)
    }
}

/// One approvals-ledger row about a folder ([`rows_for`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `rule_id`.
    pub rule: String,
    /// `decision`.
    pub decision: String,
    /// `reason`.
    pub reason: String,
    /// `box_seq`: 0 for a row no box was read for.
    pub box_seq: i64,
}

impl Row {
    /// Whether the row is a trust Claude Code recorded before the launch
    /// (`approvals::book_launch_trust`): approved, no box read, and its reason says so.
    #[must_use]
    pub fn before_launch(&self) -> bool {
        self.decision == "approved"
            && self.box_seq == 0
            && self
                .reason
                .starts_with(aterm_agent::supervise::approvals::LAUNCH_TRUST_REASON)
    }
}

/// The rows of the ledger text `ledger` (JSON lines) whose subject (`command`) is `folder`.
#[must_use]
pub fn rows_for(ledger: &str, folder: &str) -> Vec<Row> {
    ledger
        .lines()
        .filter_map(|line| J::parse(line).ok())
        .filter(|row| row.get("command").and_then(J::as_str) == Some(folder))
        .map(|row| {
            let word = |key: &str| {
                row.get(key)
                    .and_then(J::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            Row {
                rule: word("rule_id"),
                decision: word("decision"),
                reason: word("reason"),
                box_seq: row.get("box_seq").and_then(J::as_i64).unwrap_or(-1),
            }
        })
        .collect()
}

/// The checks of one run, printed as they are made.
#[derive(Debug, Default)]
struct Checks {
    n: u32,
    bad: u32,
}

impl Checks {
    fn check(&mut self, label: &str, ok: bool) {
        self.n += 1;
        if ok {
            println!("  ok   {label}");
        } else {
            self.bad += 1;
            println!("  FAIL {label}");
        }
    }
}

/// One lab's tab, its Claude Code profile and its session's ledger.
struct Bench<'a> {
    run: &'a Run,
    door: Door<'a>,
    sid: String,
    profile: PathBuf,
    checks: Checks,
}

impl Bench<'_> {
    /// Type `line` at the tab's prompt and submit it.
    fn typed(&self, line: &str) -> Result<(), Fail> {
        let sent = self.door.ctl(&["send", line])?;
        let entered = self.door.ctl(&["key", "enter"])?;
        if sent.code != 0 || entered.code != 0 {
            return Err(Fail::die(format!("could not type `{line}` into the tab")));
        }
        Ok(())
    }

    /// Run `line` at the prompt and wait for it to end; its exit status, where it ended.
    /// `mark` names the run in what is echoed after it (the typed line holds `$?`, never the
    /// digits waited for).
    fn ran(&self, line: &str, mark: &str) -> Result<Option<i32>, Fail> {
        self.typed(&format!("{line}; echo {mark}=$?"))?;
        if !self.door.await_match(&format!("{mark}=[0-9]"), HANG_S)? {
            return Ok(None);
        }
        let screen = self.door.out(&["text"]);
        let said = format!("{mark}=");
        Ok(screen
            .lines()
            .rev()
            .find_map(|row| row.trim().strip_prefix(&said)?.parse().ok()))
    }

    /// Put the tab's shell in `dir`.
    fn enter(&self, dir: &Path, mark: &str) -> Result<(), Fail> {
        let line = format!("cd {}", crate::util::shell_quote(&dir.to_string_lossy()));
        if self.ran(&line, mark)? == Some(0) {
            Ok(())
        } else {
            Err(Fail::die(format!(
                "the tab's shell did not enter {}; the screen:\n{}",
                dir.display(),
                self.door.out(&["text"])
            )))
        }
    }

    /// End the Claude Code in the tab (ctrl+c twice, a person's half second apart) and wait
    /// for the shell.
    fn quit(&self) -> Result<(), Fail> {
        for _ in 0..2 {
            let _ = self.door.ctl(&["send", "\u{3}"]);
            crate::stop::sleep(Duration::from_millis(500))?;
        }
        self.door.await_match("lab\\$", 20)?;
        Ok(())
    }

    /// The ledger rows of this lab's session about `folder`.
    fn rows(&self, folder: &Path) -> Vec<Row> {
        let mut found = Vec::new();
        crate::util::walk_files(&self.run.state(), &mut found);
        let name = format!("{}.jsonl", self.sid);
        found
            .iter()
            .filter(|path| {
                path.file_name().is_some_and(|n| n == name.as_str())
                    && path.parent().and_then(Path::file_name) == Some("drive".as_ref())
            })
            .filter_map(|path| crate::util::read_regular_text(path).ok())
            .flat_map(|text| rows_for(&text, &folder.to_string_lossy()))
            .collect()
    }

    /// Whether Claude Code's profile records the trust of `dir`'s key.
    fn recorded(&self, dir: &Path) -> bool {
        trust::recorded(&self.profile, &trust::trust_key(dir))
    }

    /// `aterm harness tap --launch -- <args>` typed in the tab's folder as the twin's block
    /// spells it, under the launch's own environment words and `vars` after them; what it
    /// printed. `mark` names the run and its output file.
    fn tap(&self, mark: &str, vars: &[&str]) -> Result<String, Fail> {
        let by_name = Launch {
            by_name: true,
            ..Launch::default()
        };
        let words = crate::lab::launch_words(self.run, &by_name);
        let mut line: Vec<String> = words
            .env
            .iter()
            .flatten()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        line.extend(vars.iter().map(|kv| (*kv).to_string()));
        let out = self.run.w.join(format!("{mark}.out"));
        line.push(crate::util::shell_quote(&self.run.aterm));
        line.extend(atpkg::launch_tap::VERB.map(str::to_string));
        line.extend(ARGS.map(str::to_string));
        line.push(format!(
            "</dev/null >{} 2>/dev/null",
            crate::util::shell_quote(&out.to_string_lossy())
        ));
        match self.ran(&line.join(" "), mark)? {
            Some(0) => Ok(crate::util::read_regular_text(&out).unwrap_or_default()),
            ended => Err(Fail::die(format!(
                "launch-trust: `aterm harness tap --launch` ended {ended:?} (None: not within \
                 {HANG_S} s); the screen:\n{}",
                self.door.out(&["text"])
            ))),
        }
    }

    /// Type `claude <args>` in the tab's folder as a person does and wait for it to draw;
    /// whether the dialog is what it drew.
    fn launch(&self, args: &[&str]) -> Result<bool, Fail> {
        let l = Launch {
            by_name: true,
            no_wait: true,
            args: args.iter().map(|a| (*a).to_string()).collect(),
            ..Launch::default()
        };
        crate::lab::launch_claude(self.run, &l)?;
        if !self.door.await_match(STARTED, HANG_S)? {
            return Err(Fail::exit(
                3,
                format!(
                    "launch-trust: Claude Code drew nothing within {HANG_S} s; the screen:\n{}",
                    self.door.out(&["text"])
                ),
            ));
        }
        Ok(self.door.out(&["text"]).contains(DIALOG))
    }

    /// A LAUNCH IN A FOLDER THE POLICY TRUSTS AND CLAUDE CODE HOLDS NO RECORD FOR: once the
    /// verb has run, the key is in Claude Code's profile and the ledger's one row for the
    /// folder is the trust recorded before the launch; the Claude Code then started there
    /// draws its composer and no dialog, and no box is answered for the folder.
    fn settled(&mut self, what: &str, dir: &Path, mark: &str) -> Result<(), Fail> {
        println!("== {what}: {}", dir.display());
        self.enter(dir, &format!("at-{mark}"))?;
        let before = self.recorded(dir);
        self.tap(&format!("tap-{mark}"), &[TTY])?;
        let after = self.recorded(dir);
        let dialog = self.launch(&ARGS)?;
        let composer = self.door.await_match(COMPOSER, HANG_S)?;
        let rows = self.rows(dir);
        println!("  ledger rows for the folder: {rows:?}");
        self.checks.check(
            &format!("{what}: Claude Code held no trust record for it before the verb ran"),
            !before,
        );
        self.checks.check(
            &format!(
                "{what}: Claude Code's own profile records the trust key once the verb has run, \
                 before any Claude Code is started on the screen"
            ),
            after,
        );
        self.checks.check(
            &format!(
                "{what}: the ledger's only row for the folder is the trust recorded before the \
                 launch (no box was read: no dialog was answered on the screen)"
            ),
            rows.len() == 1 && rows[0].before_launch(),
        );
        self.checks.check(
            &format!("{what}: the launch drew its composer and no `{DIALOG}` dialog"),
            composer && !dialog && !self.door.out(&["text"]).contains(DIALOG),
        );
        self.quit()
    }
}

/// `git <args>` in `dir`, under an identity of the lab's own.
fn git(dir: &Path, args: &[&str]) -> Result<(), Fail> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .args([
            "-c",
            "user.name=lab",
            "-c",
            "user.email=lab@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| Fail::die(format!("cannot run git: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// A new directory `root/<name>`, resolved.
fn folder(root: &Path, name: &str) -> Result<PathBuf, Fail> {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::canonicalize(&dir))
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", dir.display())))
}

/// What the headless start measured ([`headless`]).
struct Measured {
    /// The answer line.
    answer: String,
    /// From the start to the answer read.
    to_answer: Duration,
    /// From its stdin closed to its exit.
    to_exit: Duration,
    /// Its exit status.
    code: Option<i32>,
}

/// THE HEADLESS START ITSELF, in the lab's jail and scratch home: the store build's clone with
/// atpkg's warm arguments and variables, handed the one request line that attests `dir`, its
/// answer read, its stdin closed and its exit awaited, each timed.
fn headless(run: &Run, dir: &Path) -> Result<Measured, Fail> {
    let warm = atpkg::vendor_direct::spec("claude")
        .and_then(|spec| spec.warm)
        .ok_or_else(|| Fail::die("atpkg names no headless start for Claude Code"))?;
    let home = run.w.join("home").to_string_lossy().into_owned();
    let config = run.w.join("home/.claude").to_string_lossy().into_owned();
    let base = format!("http://127.0.0.1:{}", run.port);
    let mut vars: Vec<(&str, &str)> = vec![
        ("HOME", &home),
        ("CLAUDE_CONFIG_DIR", &config),
        ("ANTHROPIC_BASE_URL", &base),
        ("HTTP_PROXY", &run.proxy),
        ("HTTPS_PROXY", &run.proxy),
        ("NO_PROXY", "127.0.0.1,localhost"),
        ("DISABLE_AUTOUPDATER", "1"),
    ];
    vars.extend(warm.env.iter().copied());
    let mut argv: Vec<&str> = vec![&run.claude_bin];
    argv.extend(warm.argv.iter().copied());
    crate::procs::need_witness(run)?;
    let words = crate::procs::jailed_argv(run, Some(dir), &vars, &argv);
    let (program, args) = words.split_first().ok_or("empty jail command")?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args)
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| Fail::die(format!("cannot start the headless Claude Code: {e}")))?;
    let pid = i32::try_from(child.id()).unwrap_or(0);
    crate::procs::record_jailed(&run.run, pid);
    let mut stdin = child.stdin.take().ok_or("the start has no stdin")?;
    let stdout = child.stdout.take().ok_or("the start has no stdout")?;
    let request = trust::request_line(&dir.to_string_lossy());
    stdin
        .write_all(request.as_bytes())
        .map_err(|e| Fail::die(format!("the request could not be written: {e}")))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if trust::answer_of(&line).is_some() {
                let _ = tx.send((Instant::now(), line));
                break;
            }
        }
    });
    let hang = Duration::from_secs(HANG_S);
    let answered = rx.recv_timeout(hang);
    drop(stdin);
    let closed = Instant::now();
    let mut code = None;
    let mut exited = false;
    while closed.elapsed() < hang {
        if let Ok(Some(status)) = child.try_wait() {
            code = status.code();
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let to_exit = closed.elapsed();
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = reader.join();
    let (at, answer) = answered.map_err(|_| {
        Fail::die(format!(
            "the headless Claude Code (pid {pid}) gave no answer within {HANG_S} s"
        ))
    })?;
    if !exited {
        return Err(Fail::die(format!(
            "the headless Claude Code (pid {pid}) did not exit within {HANG_S} s of its stdin \
             closing, and was ended"
        )));
    }
    Ok(Measured {
        answer,
        to_answer: at.saturating_duration_since(started),
        to_exit,
        code,
    })
}

/// The cases, in one lab.
fn cases(run: &Run, o: &Opts) -> Res {
    let door = Door::new(run)?;
    let sid = crate::verbs::active_sid(&door);
    if sid.is_empty() {
        return Err(Fail::die("launch-trust: the lab has no active tab"));
    }
    let mut b = Bench {
        run,
        door,
        sid,
        profile: run.w.join("home/.claude/.claude.json"),
        checks: Checks::default(),
    };
    let root = folder(Path::new(&run.sockdir), "trust")?;

    // A repository, launched from a folder inside it: the key is the repository's root.
    let repo = folder(&root, "repo")?;
    git(&repo, &["init", "-q"])?;
    let inner = folder(&repo, "inner")?;
    b.settled("a folder inside a repository", &inner, "repo")?;
    b.checks.check(
        "a folder inside a repository: the key Claude Code recorded is the repository's root",
        trust::trust_key(&inner) == repo,
    );

    // A linked worktree: the key is its main checkout.
    let main = folder(&root, "main")?;
    git(&main, &["init", "-q"])?;
    git(&main, &["commit", "-q", "--allow-empty", "-m", "first"])?;
    let linked = root.join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            &linked.to_string_lossy(),
            "-b",
            "linked",
        ],
    )?;
    b.settled("a linked worktree", &linked, "linked")?;
    b.checks.check(
        "a linked worktree: the key Claude Code recorded is its main checkout",
        trust::trust_key(&linked) == main,
    );

    // A folder outside any repository: the key is the folder.
    let plain = folder(&root, "plain")?;
    b.settled("a folder outside any repository", &plain, "plain")?;

    // The home directory: nothing is settled before the launch, and the dialog draws.
    let home = std::fs::canonicalize(run.w.join("home"))
        .map_err(|e| Fail::die(format!("cannot resolve the scratch home: {e}")))?;
    println!("== the home directory: {}", home.display());
    b.enter(&home, "at-home")?;
    b.tap("tap-home", &[TTY])?;
    let settled = b.rows(&home).iter().any(Row::before_launch) || b.recorded(&home);
    let dialog = b.launch(&ARGS)?;
    b.door.await_match(COMPOSER, HANG_S)?;
    let rows = b.rows(&home);
    println!("  the dialog was on the screen at the first read: {dialog}");
    println!("  ledger rows for the folder: {rows:?}");
    b.checks.check(
        "the home directory: the verb recorded nothing, in the ledger or in Claude Code's profile",
        !settled,
    );
    b.checks.check(
        "the home directory: its dialog drew and was answered on the screen (a ledger row with \
         the box it read)",
        rows.iter()
            .any(|row| row.decision == "approved" && row.box_seq > 0),
    );
    b.quit()?;

    // The verb by itself, in one folder: told the launch's output is no terminal it settles
    // nothing; told it is one, it settles the folder; and its stdout is the `--settings` value
    // alone each time.
    let verb = folder(&root, "verb")?;
    println!(
        "== `aterm harness tap --launch` by itself: {}",
        verb.display()
    );
    b.enter(&verb, "at-verb")?;
    let piped = b.tap("tap-piped", &[NO_TTY])?;
    let unsettled = b.rows(&verb).is_empty() && !b.recorded(&verb);
    let first = b.tap("tap-first", &[TTY])?;
    let recorded = b.recorded(&verb);
    let again = b.tap("tap-again", &[TTY])?;
    let value = aterm_agent::harness::tap::settings_value(Path::new(&run.aterm));
    println!("  its stdout, the run that settled: {first:?}");
    b.checks.check(
        "a launch whose output is no terminal: the verb recorded nothing for its folder, in the \
         ledger or in Claude Code's profile",
        unsettled,
    );
    b.checks.check(
        "the same folder, the launch's output a terminal: the verb had Claude Code record the \
         key, and the ledger holds the one row",
        recorded && {
            let rows = b.rows(&verb);
            rows.len() == 1 && rows[0].before_launch()
        },
    );
    b.checks.check(
        "`aterm harness tap --launch`: the run that settled the folder printed what the runs \
         that settled nothing printed: nothing, or the `--settings` value and a line end, and \
         no other byte",
        first == again
            && first == piped
            && (first.is_empty() || value.is_some_and(|value| first == format!("{value}\n"))),
    );

    // Under `--no-harness` nothing is settled and nothing answers: the dialog stands. Last of
    // the tab's launches: it leaves the session's Autopilot off (`autopilot_by=human:launch`),
    // under which the verb settles nothing for a later launch either.
    let own = folder(&root, "own")?;
    println!("== `claude --no-harness`: {}", own.display());
    b.enter(&own, "at-own")?;
    b.tap("tap-own", &[TTY, "ATERM_NO_HARNESS=1"])?;
    let settled = !b.rows(&own).is_empty() || b.recorded(&own);
    let dialog = b.launch(&["--no-harness", ARGS[0], ARGS[1]])?;
    b.checks.check(
        "`claude --no-harness`: the verb recorded nothing, in the ledger or in Claude Code's \
         profile",
        !settled,
    );
    b.checks.check(
        &format!(
            "`claude --no-harness` in an untrusted folder: the `{DIALOG}` dialog draws (the \
             control: this lab's Claude Code does draw it where nothing settles the folder)"
        ),
        dialog,
    );
    b.quit()?;

    // The headless start itself, timed, and its answer kept.
    let timed = folder(&root, "timed")?;
    println!("== the headless start itself: {}", timed.display());
    let measured = headless(run, &timed)?;
    println!(
        "  Claude Code {}: answered {} ms after its start ({} bytes), exited {} ms after its \
         stdin closed (status {:?})",
        run.claude_ver,
        measured.to_answer.as_millis(),
        measured.answer.len(),
        measured.to_exit.as_millis(),
        measured.code
    );
    b.checks.check(
        "the headless start answered that it recorded the trust, exited by itself once its \
         stdin closed, and Claude Code's profile records the key",
        matches!(
            trust::answer_of(&measured.answer),
            Some(trust::Attested::Recorded { .. })
        ) && measured.code == Some(0)
            && b.recorded(&timed),
    );
    if let Some(path) = &o.answer {
        crate::util::write_fresh(path, format!("{}\n", measured.answer))
            .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
        println!("  its answer is kept in {}", path.display());
    }

    let Checks { n, bad } = b.checks;
    if bad == 0 {
        println!("launch-trust: PASS ({n} checks)");
        Ok(0)
    } else {
        println!("launch-trust: FAIL ({bad} of {n} checks)");
        Ok(1)
    }
}

/// Runs `launch-trust [--answer FILE]` in a lab of its own, which it takes down.
pub fn cmd_launch_trust(args: &[String]) -> Res {
    let o = Opts::parse(args)?;
    crate::stop::install();
    let tag = format!("launch-trust-{}", std::process::id());
    let mut u = UpOpts::from_env();
    u.reroute = true;
    u.trust = false;
    u.lifeline = true;
    let run = crate::lab::up(&tag, &u)?;
    let rc = cases(&run, &o);
    let _ = crate::lab::down(&tag, false);
    rc
}

#[cfg(test)]
#[path = "launch_trust_tests.rs"]
mod tests;
