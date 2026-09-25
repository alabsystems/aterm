// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `ReleaseClaimLanding` — the release claim's writer/reader
//! contract (owner ruling R2, 2026-09-23), bound to the code that ships.
//!
//! The model (`aterm_spec::derive::release_claim_landing_model`) states the laws:
//! the release commit carries only the published commit's code, main keeps every
//! peer commit and every ledger line, build numbers strictly increase, and a
//! claimed-unpublished version is read as a recut — never fresh, never "cut
//! elsewhere", never claimed again once published. This file drives the REAL
//! writers (`ledger::claim` with `changelog::claim_changelogs`) and the REAL reader
//! (`publish::real_cut_version`, over `verify::derive_cut_mode`) against real git —
//! a bare origin, a rival clone for the peers' pushes and other claims, a work clone
//! as the cut tree — projects every observed state onto the model's variables, and
//! requires the model to admit each transition, named, on both tiers.
//!
//! The in-flight steps are observed where they happen: a hooked `GitRunner` records
//! each push attempt (the commit it would land, from which the tip it read, its build
//! number and its release commit are all read back out of git) and lands the
//! environment's push at the first attempt, which is how a lost race is made
//! deterministic. Projections are READ from git — trees, ledgers, changelogs — never
//! restated; the harness supplies only what is not in git (how many pushes it made,
//! whether the version is published).
//!
//! NEGATIVE CONTROLS, so a pass is never vacuous: the historical reader — the recut
//! signal taken from the checkout's (the published commit's) changelog — is replayed
//! on the wedged state and the model refuses its classification; and a landing built
//! from the release commit's tree is committed for real and the model refuses to let
//! main take it.

#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_spec::derive::{Model, release_claim_landing_model};
use aterm_spec::interp::State;
use ledger::{ClaimPlan, GitCli, GitRunner, RunOut};

/// The seed line's build — ordinal 1 in the model. Claims run with `now = 0`, so
/// every build is exactly its predecessor plus one and the ordinals stay small.
const SEED: u64 = ledger::LEDGER_FLOOR;
const VERSION: &str = "0.92.0";
const DATE: &str = "2026-09-23";
const PEERS: &str = "src/peers";
const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- **The published \
                              entry** — ships in v0.92.0.\n\n## [0.91.0] - 2026-09-20\n\n- older \
                              notes.\n";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn identity(repo: &Path) {
    for (key, value) in [
        ("user.name", "claim model"),
        ("user.email", "claim-model@aterm.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        git(repo, &["config", key, value]);
    }
}

fn ordinal(build: u64) -> i64 {
    i64::try_from(build - SEED + 1).unwrap()
}

// --------------------------------------------------------------------------
// the fixture: a bare origin, a rival clone (the peers and the other machines),
// and a work clone (the cut tree)
// --------------------------------------------------------------------------

struct Fixture {
    root: PathBuf,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "aterm-claim-landing-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        let origin = root.join("origin.git");
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "rival"]);
        let rival = root.join("rival");
        identity(&rival);
        git(&rival, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        fs::write(
            rival.join(ledger::LEDGER_FILE),
            format!("# release ledger\n{SEED} 0.91.0\n"),
        )
        .unwrap();
        fs::write(rival.join(changelog::CHANGELOG_FILE), SEED_CHANGELOG).unwrap();
        fs::create_dir_all(rival.join("src")).unwrap();
        fs::write(rival.join("src/lib.rs"), "// the published code\n").unwrap();
        git(&rival, &["add", "-A"]);
        git(&rival, &["commit", "-q", "-m", "the published commit"]);
        git(&rival, &["push", "-q", "-u", "origin", "main"]);
        git(&root, &["clone", "-q", origin.to_str().unwrap(), "work"]);
        let work = root.join("work");
        identity(&work);
        Fixture {
            root,
            origin,
            rival,
            work,
        }
    }
}

/// What the environment pushes to main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Env {
    /// A peer's code commit.
    Peer,
    /// Another version's claim: one ledger line.
    Rival,
    /// Another machine's claim of THIS version: a ledger line and the section.
    Elsewhere,
}

impl Env {
    fn action(self) -> &'static str {
        match self {
            Env::Peer => "PeerPush",
            Env::Rival => "RivalClaim",
            Env::Elsewhere => "ElsewhereClaim",
        }
    }
}

/// The rival lands `env` on origin's main. `k` names the peer file.
fn env_push(rival: &Path, env: Env, k: usize) {
    git(rival, &["fetch", "-q", "origin", "main"]);
    git(rival, &["reset", "-q", "--hard", "origin/main"]);
    let ledger_path = rival.join(ledger::LEDGER_FILE);
    let ledger_text = fs::read_to_string(&ledger_path).unwrap();
    let next = ledger::tail(&ledger_text).unwrap().build + 1;
    match env {
        Env::Peer => {
            fs::create_dir_all(rival.join(PEERS)).unwrap();
            fs::write(
                rival.join(PEERS).join(format!("peer{k}.rs")),
                "// a peer's code\n",
            )
            .unwrap();
        }
        Env::Rival => {
            fs::write(&ledger_path, format!("{ledger_text}{next} 0.91.{k}\n")).unwrap();
        }
        Env::Elsewhere => {
            fs::write(&ledger_path, format!("{ledger_text}{next} {VERSION}\n")).unwrap();
            let changelog_path = rival.join(changelog::CHANGELOG_FILE);
            let text = fs::read_to_string(&changelog_path).unwrap();
            fs::write(
                &changelog_path,
                format!("{text}\n## [{VERSION}] - 2026-09-01\n\n- cut elsewhere.\n"),
            )
            .unwrap();
        }
    }
    git(rival, &["add", "-A"]);
    git(rival, &["commit", "-q", "-m", &format!("{env:?} {k}")]);
    git(rival, &["push", "-q", "origin", "main"]);
}

/// A tree, projected: how many peer commits' code it carries, its ledger's record
/// count and tail ordinal, and whether its changelog carries the version's section.
#[derive(Debug, Clone)]
struct View {
    commit: String,
    code: i64,
    lines: i64,
    tail: i64,
    section: i64,
}

fn view(repo: &Path, rev: &str) -> View {
    let commit = git(repo, &["rev-parse", rev]);
    let code = git(
        repo,
        &["ls-tree", "-r", "--name-only", &commit, "--", PEERS],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .count();
    let records = ledger::parse(&git(
        repo,
        &["show", &format!("{commit}:{}", ledger::LEDGER_FILE)],
    ))
    .unwrap();
    let text = git(
        repo,
        &["show", &format!("{commit}:{}", changelog::CHANGELOG_FILE)],
    );
    View {
        code: i64::try_from(code).unwrap(),
        lines: i64::try_from(records.len()).unwrap(),
        tail: ordinal(records.last().unwrap().build),
        section: i64::from(changelog::has_section(&text, VERSION)),
        commit,
    }
}

fn parents(repo: &Path, commit: &str) -> Vec<String> {
    git(repo, &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

// --------------------------------------------------------------------------
// the hooked runner: records every push attempt, lands the environment's push
// at the first one
// --------------------------------------------------------------------------

enum Event {
    /// The claimant is about to push this commit to main.
    Attempt(String),
    /// The environment landed a push first; main as it then stood.
    Env(Env, View),
}

type EnvAction<'a> = Box<dyn FnOnce() -> (Env, View) + 'a>;

struct Hooked<'a> {
    inner: GitCli,
    events: RefCell<Vec<Event>>,
    first_push: RefCell<Option<EnvAction<'a>>>,
}

impl GitRunner for Hooked<'_> {
    fn git(&self, args: &[&str]) -> ledger::Result<RunOut> {
        if args.first() == Some(&"push") {
            let landed = args
                .last()
                .and_then(|spec| spec.split(':').next())
                .unwrap()
                .to_string();
            self.events.borrow_mut().push(Event::Attempt(landed));
            let env = self.first_push.borrow_mut().take();
            if let Some(env) = env {
                let (env, main) = env();
                self.events.borrow_mut().push(Event::Env(env, main));
            }
        }
        self.inner.git(args)
    }
}

// --------------------------------------------------------------------------
// the harness: the projected state, and every transition checked by the model
// --------------------------------------------------------------------------

struct Harness {
    fix: Fixture,
    model: Model,
    state: State,
    /// The published commit — P.
    source: String,
    /// origin/main's tips in push order: `seq` is an index here.
    tips: Vec<String>,
    pushes: usize,
    steps: usize,
}

impl Harness {
    fn new(label: &str) -> Self {
        let fix = Fixture::new(label);
        let source = git(&fix.origin, &["rev-parse", "main"]);
        let model = release_claim_landing_model();
        let state = model.init_state();
        let harness = Harness {
            fix,
            model,
            state,
            tips: vec![source.clone()],
            source,
            pushes: 0,
            steps: 0,
        };
        // The initial state IS the fixture: nothing but the seed line, no peer code,
        // no section.
        let main = view(&harness.fix.origin, "main");
        assert_eq!(
            (main.code, main.lines, main.tail, main.section),
            (0, 1, 1, 0)
        );
        harness
    }

    fn seq_of(&self, tip: &str) -> i64 {
        i64::try_from(
            self.tips
                .iter()
                .position(|seen| seen == tip)
                .unwrap_or_else(|| panic!("{tip} is not a tip this harness saw land")),
        )
        .unwrap()
    }

    fn with(&self, fields: &[(&'static str, i64)]) -> State {
        let mut next = self.state.clone();
        for &(name, value) in fields {
            assert!(next.contains_key(name), "no model variable {name}");
            next.insert(name, value);
        }
        next
    }

    fn admits(&mut self, action: &str, next: &State) -> (bool, String) {
        self.steps += 1;
        aterm_spec::verify::validate_transition_tiered(
            &self.model,
            &[],
            &self.state,
            next,
            Some(action),
            &format!("ReleaseClaimLanding step {} {action}", self.steps),
        )
    }

    fn step(&mut self, action: &str, next: State) {
        let (ok, why) = self.admits(action, &next);
        assert!(
            ok,
            "the model refuses production's {action}: {why}\nfrom {:?}\nto   {next:?}",
            self.state
        );
        self.state = next;
    }

    fn refuses(&mut self, action: &str, next: &State) {
        let (ok, why) = self.admits(action, next);
        assert!(
            !ok,
            "NEGATIVE CONTROL passed the model — {action} to {next:?} was admitted ({why}); \
             the conformance would not catch the defect it replays"
        );
    }

    /// The environment step `env` observed as `main`, with its counter moved.
    fn env_state(&self, env: Env, main: &View) -> State {
        let counter = match env {
            Env::Peer => "peers",
            Env::Rival => "rivals",
            Env::Elsewhere => "foreign",
        };
        self.with(&[
            (counter, self.state[counter] + 1),
            ("main_code", main.code),
            ("main_lines", main.lines),
            ("tail", main.tail),
            ("section", main.section),
            ("seq", self.seq_of(&main.commit)),
        ])
    }

    fn env(&mut self, env: Env) {
        self.pushes += 1;
        env_push(&self.fix.rival, env, self.pushes);
        let main = view(&self.fix.origin, "main");
        self.tips.push(main.commit.clone());
        let next = self.env_state(env, &main);
        self.step(env.action(), next);
    }

    /// The REAL reader over `main_changelog`, as the transition it makes.
    fn classification(&self, main_changelog: &str) -> (&'static str, State) {
        let source = git(
            &self.fix.origin,
            &[
                "show",
                &format!("{}:{}", self.source, changelog::CHANGELOG_FILE),
            ],
        );
        let published = self.state["published"] == 1;
        match publish::real_cut_version(&source, main_changelog, VERSION, &mut |_| Ok(published)) {
            Ok((version, recut)) => {
                assert_eq!(version, VERSION);
                (
                    if recut {
                        "ClassifyRecut"
                    } else {
                        "ClassifyFresh"
                    },
                    self.with(&[
                        ("phase", 1),
                        ("mode", if recut { 2 } else { 1 }),
                        ("allow", i64::from(recut)),
                    ]),
                )
            }
            Err(error) => {
                assert!(error.to_string().contains("already published"), "{error}");
                (
                    "ClassifyRefuse",
                    self.with(&[("phase", 5), ("mode", 0), ("allow", 0)]),
                )
            }
        }
    }

    fn main_changelog(&self) -> String {
        git(
            &self.fix.origin,
            &["show", &format!("main:{}", changelog::CHANGELOG_FILE)],
        )
    }

    fn classify(&mut self) -> &'static str {
        let (action, next) = self.classification(&self.main_changelog());
        self.step(action, next);
        action
    }

    /// NEGATIVE CONTROL: the historical reader — main's section read from the
    /// checkout, i.e. the published commit's changelog.
    fn replay_the_checkout_reader(&mut self) {
        let checkout = git(
            &self.fix.origin,
            &[
                "show",
                &format!("{}:{}", self.source, changelog::CHANGELOG_FILE),
            ],
        );
        let (action, next) = self.classification(&checkout);
        assert_eq!(action, "ClassifyFresh", "the checkout never carries it");
        self.refuses(action, &next);
    }

    /// The REAL claim from the cut tree at the published commit, with `during` landed
    /// on main at its first push, every observed step checked. `bad_landing` also
    /// commits the landing built from the release commit's tree and requires the model
    /// to refuse it.
    fn claim(&mut self, during: Option<Env>, bad_landing: bool) -> ledger::Result<ledger::Claim> {
        // What `gates::place_cut_tree` does before a claim.
        git(
            &self.fix.work,
            &["checkout", "-q", "--detach", &self.source],
        );
        let k = self.pushes + 1;
        let rival = self.fix.rival.clone();
        let origin = self.fix.origin.clone();
        let hooked = Hooked {
            inner: GitCli::new(&self.fix.work),
            events: RefCell::new(Vec::new()),
            first_push: RefCell::new(during.map(|env| {
                Box::new(move || {
                    env_push(&rival, env, k);
                    (env, view(&origin, "main"))
                }) as EnvAction<'_>
            })),
        };
        let plan = ClaimPlan {
            version: VERSION,
            now: 0,
            allow_existing_section: self.state["allow"] == 1,
            max_attempts: ledger::MAX_CLAIM_ATTEMPTS,
            source: &self.source,
        };
        let result = ledger::claim(&hooked, &self.fix.work, &plan, &|source, main| {
            changelog::claim_changelogs(source, main, VERSION, DATE)
        });
        if during.is_some() {
            self.pushes += 1;
        }

        let mut attempts = 0;
        for event in hooked.events.into_inner() {
            match event {
                Event::Attempt(landed) => {
                    let parents = parents(&self.fix.work, &landed);
                    let (read, release) = match parents.as_slice() {
                        [tip, release] => (tip.clone(), release.clone()),
                        [published] => (published.clone(), landed.clone()),
                        other => panic!("a landing with parents {other:?}"),
                    };
                    let landing = view(&self.fix.work, &landed);
                    let next = if attempts == 0 {
                        self.with(&[
                            ("phase", 2),
                            ("read_seq", self.seq_of(&read)),
                            ("n", landing.tail),
                            ("release_code", view(&self.fix.work, &release).code),
                        ])
                    } else {
                        self.with(&[("read_seq", self.seq_of(&read)), ("n", landing.tail)])
                    };
                    let action = if attempts == 0 {
                        "ClaimRead"
                    } else {
                        "ClaimRetry"
                    };
                    self.step(action, next);
                    attempts += 1;
                }
                Event::Env(env, main) => {
                    self.tips.push(main.commit.clone());
                    let next = self.env_state(env, &main);
                    self.step(env.action(), next);
                }
            }
        }

        match &result {
            Ok(claim) => {
                let main = view(&self.fix.origin, "main");
                assert_eq!(main.commit, claim.landed, "main is the landing");
                let before = parents(&self.fix.work, &claim.landed)[0].clone();
                // The two structural laws, read straight off git: R is the published
                // commit plus the claim files, and main moved by exactly those files.
                for (from, to) in [(&self.source, &claim.commit), (&before, &claim.landed)] {
                    let moved = git(&self.fix.work, &["diff", "--name-only", from, to]);
                    for path in moved.lines() {
                        assert!(
                            path == ledger::LEDGER_FILE || path == changelog::CHANGELOG_FILE,
                            "{from}..{to} moves {path}"
                        );
                    }
                }
                let prior = view(&self.fix.work, &before).tail;
                let landings = self.state["landings"] + 1;
                let after_publish = self.state["after_publish"] + self.state["published"];
                let landed_state = |main: &View, seq: i64| {
                    self.with(&[
                        ("phase", 3),
                        ("prior", prior),
                        ("tail", main.tail),
                        ("last", ordinal(claim.build)),
                        ("landings", landings),
                        ("main_lines", main.lines),
                        ("main_code", main.code),
                        ("section", main.section),
                        ("seq", seq),
                        ("after_publish", after_publish),
                    ])
                };
                let seq = i64::try_from(self.tips.len()).unwrap();
                // NEGATIVE CONTROL: main takes the RELEASE commit's tree — the same
                // parents, R's tree. Committed for real, never pushed.
                let bad_state = bad_landing.then(|| {
                    let tree = git(
                        &self.fix.work,
                        &["rev-parse", &format!("{}^{{tree}}", claim.commit)],
                    );
                    let bad = git(
                        &self.fix.work,
                        &[
                            "commit-tree",
                            &tree,
                            "-p",
                            &before,
                            "-p",
                            &claim.commit,
                            "-m",
                            "a landing built from the release commit",
                        ],
                    );
                    landed_state(&view(&self.fix.work, &bad), seq)
                });
                let next = landed_state(&main, seq);
                if let Some(bad_state) = bad_state {
                    self.refuses("ClaimLand", &bad_state);
                }
                self.tips.push(main.commit.clone());
                self.step("ClaimLand", next);
            }
            Err(error) => {
                assert!(error.to_string().contains("cut elsewhere"), "{error}");
                let next = self.with(&[("phase", 5), ("elsewhere", 1)]);
                self.step("ClaimElsewhere", next);
            }
        }
        result
    }

    fn die(&mut self) {
        let next = self.with(&[("phase", 4), ("mode", 0), ("allow", 0)]);
        self.step("Die", next);
    }

    fn publish(&mut self) {
        let next = self.with(&[("phase", 0), ("published", 1), ("mode", 0), ("allow", 0)]);
        self.step("Publish", next);
    }
}

/// Peers push; the fresh claim loses a race to another version's claim, re-reads
/// and lands by a merge; the cut dies; the reader calls the next cut a recut (and
/// the historical reader, replayed, is refused); the recut loses a race to a peer,
/// lands again (and a landing from the release commit's tree, committed for real,
/// is refused); it publishes; the next cut is refused.
#[test]
fn the_real_claim_and_reader_refine_the_model_through_a_wedge_and_a_recut() {
    let mut h = Harness::new("wedge");
    h.env(Env::Peer);
    assert_eq!(h.classify(), "ClassifyFresh");
    let first = h
        .claim(Some(Env::Rival), false)
        .expect("a lost race to another version's claim is retried");
    assert_ne!(first.landed, first.commit, "main moved, so it took a merge");
    h.die();

    h.replay_the_checkout_reader();
    assert_eq!(h.classify(), "ClassifyRecut");
    let second = h
        .claim(Some(Env::Peer), true)
        .expect("a recut's lost race onto its own section is retried, not cut elsewhere");
    assert!(second.build > first.build);
    h.publish();
    assert_eq!(h.classify(), "ClassifyRefuse");
    assert_eq!(h.state["landings"], 2);
    assert_eq!(h.state["main_code"], 2);
    assert_eq!(h.state["main_lines"], 4);
}

/// Main has not moved: main takes the release commit itself.
#[test]
fn an_unmoved_main_takes_the_release_commit_as_a_fast_forward() {
    let mut h = Harness::new("fast-forward");
    assert_eq!(h.classify(), "ClassifyFresh");
    let claim = h.claim(None, false).unwrap();
    assert_eq!(claim.landed, claim.commit);
    h.publish();
    assert_eq!(h.classify(), "ClassifyRefuse");
}

/// Another machine claims this version while ours is in flight: the lost race finds
/// the section and aborts as cut elsewhere — the one case the abort is for.
#[test]
fn another_machines_claim_of_the_version_is_the_cut_elsewhere_abort() {
    let mut h = Harness::new("elsewhere");
    assert_eq!(h.classify(), "ClassifyFresh");
    h.claim(Some(Env::Elsewhere), false)
        .expect_err("cut elsewhere");
    assert_eq!(h.state["phase"], 5);
    assert_eq!(h.state["foreign"], 1);
}
