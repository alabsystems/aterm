// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `work-overlap [tag] [--keep]`: the overlap fact live. A lab manager Claude Code spawns two
//! Claude Code workers under Land, each in its own worktree of one scratch repository, and each
//! changes the same function of the same file; the second is told, on the prompt aterm types it
//! next, that one file its branch changes is also changed by the first, by the first's sid and
//! nothing else. The first then commits, pushes to the scratch origin's main and lands, and its
//! work record clears while the second's stays. Headless, the lab's stub alone.
//!
//! Each worker's turn holds on a gate file the lab writes once what it waits for is read (a
//! worker's record a floor old, a turn end seen), so the turns fall in order without a timer of
//! the worker's own; every wait for something that must happen is a one-minute hang detector.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::LAB;
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::util::atomic_write;
use crate::workers::{
    Checks, Row, keep_worker_texts, kv, spawned, transcript_of, transcripts, wait_for,
};
use crate::{Fail, Res, stop};

/// The file both workers change.
pub const FILE: &str = "src/lib.rs";

/// What the file holds on the scratch origin's main.
const SEED: &str = "pub fn shared() -> u32 {\n    0\n}\n";

/// The scratch world's folder, under the lab's project folder: a bare `origin.git`, the
/// repository `repo`, its worktrees `wa` and `wb`, and the gates.
const WORLD: &str = ".ubd/work-overlap.noindex";

/// The longest any awaited step may take: a hang detector, never a pace.
const HANG: Duration = Duration::from_secs(60);

/// How old a worker's first record must be before its turn may end: a turn break reads it again
/// only a floor after the last read started (`aterm_agent::harness::work_record::WORK_FLOOR`),
/// and the read's start is no later than its end; a second past the floor's whole seconds.
const FLOOR_PAST_MS: u64 = aterm_agent::harness::work_record::WORK_FLOOR.as_secs() * 1_000 + 1_000;

/// The most a worker's gate wait runs (`0.2 s` steps): past it the Bash fails, and the lab's
/// own hang detector has long said why.
const GATE_STEPS: u32 = 600;

/// One worker of the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worker {
    /// The token its task carries (the stub keys its script on it).
    pub token: String,
    /// Its task, as the manager's heredoc carries it.
    pub task: String,
    /// Its worktree.
    pub tree: PathBuf,
    /// The value its change makes [`FILE`]'s function return.
    pub value: u32,
    /// Its report: the final message of its first turn.
    pub report: String,
}

/// What the manager is asked and every conversation's script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The manager's token.
    pub mgr: String,
    /// The manager's prompt (one line, holding the token).
    pub prompt: String,
    /// The worker that changes the file first, then commits, pushes and lands.
    pub first: Worker,
    /// The worker that changes it second and is told the overlap.
    pub second: Worker,
    /// The text the manager ends its turn with.
    pub waiting: String,
    /// The gates' folder.
    pub gates: PathBuf,
    /// The workers' temporary folder.
    pub tmp: PathBuf,
    /// What the first answers Land's instruction with.
    pub defense: String,
    /// What the first answers Land's short land check with.
    pub landed: String,
    /// What the second answers Land's instruction with: not done.
    pub not_yet: String,
}

impl Plan {
    /// The plan of one run in the scratch world `world`, its tokens made unique by `nonce`.
    #[must_use]
    pub fn new(nonce: &str, world: &Path) -> Plan {
        let mgr = format!("LABOVM-{nonce}");
        let worker = |k: u32, tree: &str| {
            let token = format!("LABOV{k}-{nonce}");
            Worker {
                task: format!(
                    "{token}: change what shared() in {FILE} returns to {k}, with one Bash call, then report."
                ),
                token,
                tree: world.join(tree),
                value: k,
                report: format!("Worker {k}: shared() in {FILE} now returns {k}."),
            }
        };
        Plan {
            prompt: format!(
                "{mgr}: start two workers in the two worktrees, each changing shared(), then say so."
            ),
            mgr,
            first: worker(1, "wa"),
            second: worker(2, "wb"),
            waiting: "Started two workers.".into(),
            gates: world.join("gates"),
            tmp: world.join("tmp"),
            defense: "Nothing left: the change is committed and pushed to origin main, and git status is clean.".into(),
            landed: "LANDED".into(),
            not_yet: "Not yet: this change waits for the other session's to land first.".into(),
        }
    }

    /// The name of the gate a worker's `n`th held step waits on.
    #[must_use]
    pub fn gate_name(w: &Worker, n: u32) -> String {
        format!("{}-{n}", w.token)
    }

    /// The gate a worker's `n`th held step waits on.
    #[must_use]
    pub fn gate(&self, w: &Worker, n: u32) -> PathBuf {
        self.gates.join(Self::gate_name(w, n))
    }

    /// The Bash a worker's spawn runs.
    #[must_use]
    pub fn spawn_command(w: &Worker) -> String {
        format!(
            "aterm ctl @self spawn agent=claude cwd={} - <<'EOF'\n{}\nEOF",
            w.tree.display(),
            w.task
        )
    }

    /// A shell wait for `gate`, bounded by [`GATE_STEPS`], that fails where it never opened.
    #[must_use]
    pub fn hold(gate: &Path) -> String {
        let g = gate.display();
        format!(
            "n=0; until [ -e {g} ] || [ $n -ge {GATE_STEPS} ]; do sleep 0.2; n=$((n+1)); done; [ -e {g} ]"
        )
    }

    /// The Bash of a worker's first turn: its change, then its gate.
    #[must_use]
    pub fn change_command(&self, w: &Worker) -> String {
        format!(
            "printf 'pub fn shared() -> u32 {{\\n    {}\\n}}\\n' > {FILE} && {}",
            w.value,
            Self::hold(&self.gate(w, 1))
        )
    }

    /// The Bash of the first worker's answer to Land: its gate, then its commit and its push to
    /// origin's main, git's temporary files (`xcrun`'s cache) kept in the scratch world.
    #[must_use]
    pub fn land_command(&self) -> String {
        format!(
            "{} && export TMPDIR={} && git add -A && git -c user.name='aterm lab' -c \
             user.email=lab@example.invalid -c commit.gpgsign=false commit -q -m 'shared returns \
             {}' && git push -q origin HEAD:main && git status --porcelain",
            Self::hold(&self.gate(&self.first, 2)),
            self.tmp.display(),
            self.first.value
        )
    }

    /// The stub's `tool` script.
    #[must_use]
    pub fn script(&self) -> J {
        let bash = |when: &str, cmd: String, what: &str| {
            J::obj([
                ("when", J::s(when)),
                ("name", J::s("Bash")),
                (
                    "input",
                    J::obj([("command", J::s(cmd)), ("description", J::s(what))]),
                ),
            ])
        };
        let say = |when: &str, text: &str| J::obj([("when", J::s(when)), ("text", J::s(text))]);
        J::Arr(vec![
            bash(
                &self.mgr,
                Self::spawn_command(&self.first),
                "Start the first worker",
            ),
            bash(
                &self.mgr,
                Self::spawn_command(&self.second),
                "Start the second worker",
            ),
            say(&self.mgr, &self.waiting),
            bash(
                &self.first.token,
                self.change_command(&self.first),
                "Change shared()",
            ),
            say(&self.first.token, &self.first.report),
            bash(&self.first.token, self.land_command(), "Commit and push"),
            say(&self.first.token, &self.defense),
            say(&self.first.token, &self.landed),
            bash(
                &self.second.token,
                self.change_command(&self.second),
                "Change shared()",
            ),
            say(&self.second.token, &self.second.report),
            say(&self.second.token, &self.not_yet),
        ])
    }
}

/// The sentence the second worker's prompt carries, naming `first`.
#[must_use]
pub fn told(first: &str) -> String {
    format!("1 file this branch changes is also changed by {first}")
}

/// The first prompt aterm typed after a worker's `report`: the next user row of its transcript
/// with words and no tool result.
#[must_use]
pub fn typed_after<'a>(rows: &'a [Row], report: &str) -> Option<&'a Row> {
    let at = rows
        .iter()
        .position(|r| r.kind == "assistant" && r.text == report)?;
    rows[at + 1..]
        .iter()
        .find(|r| r.kind == "user" && r.result.is_none() && !r.text.is_empty())
}

/// Each `fact-overlap@v1` row a session's ledger `text` holds typed: its `command`, the fact.
#[must_use]
pub fn overlaps_typed(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| J::parse(l).ok())
        .filter(|r| {
            r.get("rule_id").and_then(J::as_str) == Some("fact-overlap@v1")
                && r.get("decision").and_then(J::as_str) == Some("typed")
        })
        .map(|r| {
            r.get("command")
                .and_then(J::as_str)
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// Whether a typed fact `fact` names nothing of where anyone works: no path, no file name, no
/// worktree or branch, no repository.
#[must_use]
pub fn names_no_place(fact: &str) -> bool {
    !fact.contains('/')
        && !fact.contains("lib.rs")
        && fact.split(' ').all(|w| {
            !["src", "wa", "wb", "main", "origin", "repo"].contains(&w.trim_end_matches(','))
        })
}

/// One session's record of a `survey --json` reply, by sid.
#[must_use]
pub fn session_of(survey: &str, sid: &str) -> Option<J> {
    let all = J::parse(survey).ok()?;
    all.as_arr()?
        .iter()
        .flat_map(|i| {
            i.get("sessions")
                .and_then(J::as_arr)
                .unwrap_or_default()
                .to_vec()
        })
        .find(|s| s.get("sid").and_then(J::as_str) == Some(sid))
}

/// What a survey record says of the session's work: its `work=` (none where it shows no record),
/// whether its `work_paths=` names [`FILE`], and its `work_ms=`.
#[must_use]
pub fn work_of(session: &J) -> (Option<String>, bool, Option<u64>) {
    let field = |k: &str| session.get(k).and_then(J::as_str).map(str::to_string);
    let names = field("work_paths").is_some_and(|p| p.split('\n').any(|n| n == FILE));
    let ms = field("work_ms").and_then(|v| v.parse().ok());
    (field("work"), names, ms)
}

/// Lays the scratch world at `world`: a bare origin whose main holds [`FILE`] as [`SEED`], a
/// clone of it, two worktrees of the clone on branches `wa` and `wb` off main, the gates' folder
/// and the workers' temporary folder.
fn lay_world(home: &Path, world: &Path) -> Result<(), String> {
    let git = |dir: &Path, args: &[&str]| crate::forever::git_out(home, dir, args).map(|_| ());
    for folder in ["gates", "tmp"] {
        fs::create_dir_all(world.join(folder)).map_err(|e| format!("{}: {e}", world.display()))?;
    }
    let origin = world.join("origin.git");
    let repo = world.join("repo");
    git(world, &["init", "-q", "--bare", "origin.git"])?;
    git(world, &["init", "-q", "repo"])?;
    fs::create_dir_all(repo.join("src")).map_err(|e| e.to_string())?;
    fs::write(repo.join(FILE), SEED).map_err(|e| e.to_string())?;
    let origin = origin.to_string_lossy().into_owned();
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "shared returns 0"])?;
    git(&repo, &["remote", "add", "origin", &origin])?;
    git(&repo, &["push", "-q", "origin", "main"])?;
    git(&repo, &["fetch", "-q", "origin"])?;
    for tree in ["wa", "wb"] {
        let at = world.join(tree).to_string_lossy().into_owned();
        git(&repo, &["worktree", "add", "-q", "-b", tree, &at, "main"])?;
    }
    Ok(())
}

/// Wait, as a hang detector, until `ready` holds; whether it did.
fn within(ready: impl FnMut() -> bool) -> Result<bool, Fail> {
    wait_for(HANG, ready).map_err(Fail::from)
}

/// The run on the lab `tag`, which [`cmd_work_overlap`] brought up: whether every check held.
#[allow(clippy::too_many_lines)]
fn run_overlap(tag: &str) -> Result<bool, Fail> {
    let could_not = |m: &str| Fail::exit(3, format!("work-overlap: COULD NOT RUN: {m}"));
    let run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let mut b = [0u8; 4];
    let _ = aterm_uds::rand::fill(&mut b);
    let world = Path::new(&run.proj).join(WORLD);
    lay_world(&run.w.join("home"), &world)
        .map_err(|e| could_not(&format!("the scratch repository: {e}")))?;
    let plan = Plan::new(&format!("{:08x}", u32::from_ne_bytes(b)), &world);
    println!(
        "work-overlap: lab '{tag}', Claude Code {}, {}",
        run.claude_ver,
        run.need_aterm()?.display()
    );
    crate::verbs::settings_merge(
        &run,
        &J::obj([(
            "permissions",
            J::obj([
                ("allow", J::Arr(vec![J::s("Bash")])),
                ("defaultMode", J::s("acceptEdits")),
            ]),
        )]),
    )?;
    atomic_write(&run.w.join("tool.json"), plan.script().py().as_bytes())
        .map_err(|e| could_not(&format!("cannot write the script: {e}")))?;
    crate::stub::control(&run.port, "/__mode?m=tool")
        .map_err(|e| could_not(&format!("the stub did not take mode tool: {e}")))?;
    crate::lab::launch_claude(
        &run,
        &Launch {
            no_wait: true,
            ..Launch::default()
        },
    )
    .map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            could_not(&format!(
                "Claude Code could not be launched: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    if !door.seen("❯", 60)? {
        return Err(could_not(
            "the manager's Claude Code never drew its composer",
        ));
    }
    if !crate::verbs::turn(&door, &plan.prompt)? {
        return Err(could_not("the manager's composer did not take the prompt"));
    }
    let mgr_sid = crate::verbs::sid(&door);
    println!("  manager   {mgr_sid}");
    let mut c = Checks::default();
    let mut sids = Vec::new();
    let spawns = within(|| {
        sids = transcript_of(&transcripts(&run), &plan.mgr)
            .map(spawned)
            .unwrap_or_default();
        sids.len() >= 2
    })?;
    if !c.check(
        spawns,
        &format!("the manager's two spawns printed OK <sid> ({sids:?})"),
    ) {
        return Ok(false);
    }
    let (first, second) = (sids[0].clone(), sids[1].clone());
    println!("  first     {first} in {}", plan.first.tree.display());
    println!("  second    {second} in {}", plan.second.tree.display());
    let survey = || door.out(&["survey", "--json"]);
    let work = |sid: &str| session_of(&survey(), sid).map(|s| work_of(&s));
    let aged = |sid: &str| {
        within(|| {
            work(sid)
                .and_then(|(w, _, ms)| w.and(ms))
                .is_some_and(|ms| ms >= FLOOR_PAST_MS)
        })
    };
    // The lab writes its gates by the folder's real path: the scratch writer follows no link,
    // and the project folder's own path may hold one (`/tmp`).
    let gates =
        fs::canonicalize(&plan.gates).map_err(|e| could_not(&format!("the gates' folder: {e}")))?;
    let open = |w: &Worker, n: u32| atomic_write(&gates.join(Plan::gate_name(w, n)), b"go\n");
    let transcript = |w: &Worker| {
        transcript_of(&transcripts(&run), &w.token)
            .map(<[Row]>::to_vec)
            .unwrap_or_default()
    };
    let both_read = within(|| {
        [&first, &second]
            .iter()
            .all(|sid| work(sid).is_some_and(|(w, _, _)| w.is_some()))
    })?;
    c.check(
        both_read,
        "both workers' first looks read a work record (survey --json work=)",
    );
    let first_aged = aged(&first)?;
    c.check(
        first_aged,
        "the first's record is a floor old: its turn may end",
    );
    open(&plan.first, 1).map_err(|e| could_not(&format!("the first's gate: {e}")))?;
    let first_ended =
        within(|| typed_after(&transcript(&plan.first), &plan.first.report).is_some())?;
    c.check(
        first_ended,
        "the first's turn ended and aterm typed its next prompt (Land's instruction)",
    );
    let first_names = within(|| work(&first).is_some_and(|(_, names, _)| names))?;
    c.check(
        first_names,
        &format!("after the first's turn end its work= row names {FILE}"),
    );

    let second_aged = aged(&second)?;
    c.check(
        second_aged,
        "the second's record is a floor old: its turn may end",
    );
    open(&plan.second, 1).map_err(|e| could_not(&format!("the second's gate: {e}")))?;
    let ledger =
        aterm_agent::supervise::approvals::ledger_under(&run.state(), Some(second.as_str()));
    let mut typed = Vec::new();
    let told_row = within(|| {
        typed = overlaps_typed(&crate::util::scratch_text(&ledger));
        !typed.is_empty()
    })?;
    let want = told(&first);
    c.check(
        told_row && typed.iter().all(|f| *f == want && names_no_place(f)),
        &format!(
            "the second's ledger holds a fact-overlap@v1 row typed {want:?}, naming no place ({typed:?})"
        ),
    );
    let mut next = None;
    let rode = within(|| {
        next = typed_after(&transcript(&plan.second), &plan.second.report).map(|r| r.text.clone());
        next.is_some()
    })?;
    let riding = format!(" (aterm: {want})");
    let next = next.unwrap_or_default();
    c.check(
        rode && next.contains(&riding) && names_no_place(&want),
        &format!(
            "the second's next aterm-typed prompt carries{riding} ({:?})",
            next.chars().take(400).collect::<String>()
        ),
    );
    let both_name = within(|| {
        [&first, &second]
            .iter()
            .all(|sid| work(sid).is_some_and(|(w, names, _)| w.is_some() && names))
    })?;
    c.check(
        both_name,
        &format!("survey --json shows both workers' work= rows naming {FILE}"),
    );

    open(&plan.first, 2).map_err(|e| could_not(&format!("the first's second gate: {e}")))?;
    let at = format!("@{first}");
    let mut task = String::new();
    let landed = within(|| {
        task = door.out(&[at.as_str(), "task"]);
        kv(&task, "state") == Some("done") && kv(&task, "why") == Some("landed")
    })?;
    c.check(
        landed,
        &format!(
            "the first committed, pushed to origin main, answered Nothing left: and landed (task {})",
            task.trim().chars().take(140).collect::<String>()
        ),
    );
    let cleared = within(|| {
        work(&first).is_some_and(|(w, _, _)| w.is_none())
            && work(&second).is_some_and(|(w, names, _)| w.is_some() && names)
    })?;
    c.check(
        cleared,
        "the first's work= row cleared at its landing while the second's stays",
    );

    let egress = crate::util::read_regular_text(&run.run.join("egress.log")).unwrap_or_default();
    let external = egress.lines().filter(|l| l.contains("EXTERNAL")).count();
    c.check(
        external == 0,
        &format!("egress: no external endpoint ({external})"),
    );
    if c.bad > 0 {
        keep_worker_texts(&run, &door, &[mgr_sid, first, second]);
        println!("work-overlap: FAIL ({} of {} checks)", c.bad, c.n);
    } else {
        println!("work-overlap: PASS ({} checks)", c.n);
    }
    Ok(c.bad == 0)
}

/// `work-overlap [tag] [--keep]`.
pub fn cmd_work_overlap(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("work-overlap: {m}"));
    let (mut tag, mut keep) = (None::<String>, false);
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => {
                return Err(usage(&format!("unknown option '{s}' (--keep)")));
            }
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("overlap-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: work-overlap brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    let mut u = UpOpts::from_env();
    u.trust = true;
    u.lifeline = !keep;
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: work-overlap: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_overlap(&tag);
    if keep {
        println!("work-overlap: lab '{tag}' left up ({LAB} down {tag})");
    } else if let Ok(k) = crate::lab::down(&tag, false) {
        println!("lab '{tag}' is down; logs kept in {}", k.display());
    }
    Ok(if verdict? { 0 } else { 1 })
}

#[cfg(test)]
#[path = "work_overlap_tests.rs"]
mod tests;
