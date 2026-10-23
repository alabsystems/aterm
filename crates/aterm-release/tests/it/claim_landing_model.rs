// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `ReleaseClaimLanding` (`aterm_models_release::release_claim_landing_model`).
//!
//! Drives the real writers (`ledger::reserve`, `ledger::claim`, `changelog::claim_changelogs`)
//! and reader (`publish::real_cut_version`) against real git and verifies that the model admits
//! each projected transition and refuses the negative controls.

use crate::{changelog, ledger, publish};

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use aterm_models_release::release_claim_landing_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use ledger::testing::Git;
use ledger::{ClaimPlan, GitRunner, RunOut};

const SEED: u64 = ledger::LEDGER_FLOOR;
const VERSION: &str = "0.92.0";
const DATE: &str = "2026-09-23";
const PEERS: &str = "src/peers";
const SEED_CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- **The published \
                              entry** — ships in v0.92.0.\n\n## [0.91.0] - 2026-09-20\n\n- older \
                              notes.\n";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = ledger::testing::git()
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
    ledger::testing::config(
        repo,
        &[
            ("user.name", "claim model"),
            ("user.email", "claim-model@aterm.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
}

fn ordinal(build: u64) -> i64 {
    i64::try_from(build - SEED + 1).unwrap()
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    origin: PathBuf,
    rival: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-claim-landing-{label}-"))
            .tempdir()
            .unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        let origin = root.join("origin.git");
        ledger::testing::config(&origin, &[]);
        git(root, &["clone", "-q", origin.to_str().unwrap(), "rival"]);
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
        git(root, &["clone", "-q", origin.to_str().unwrap(), "work"]);
        let work = root.join("work");
        identity(&work);
        Fixture {
            _root: dir,
            origin,
            rival,
            work,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Env {
    Peer,
    Rival,
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

#[derive(Debug, Clone)]
struct View {
    commit: String,
    parents: Vec<String>,
    code: i64,
    lines: i64,
    tail: i64,
    section: i64,
}

fn objects<const N: usize>(repo: &Path, specs: [String; N]) -> [(String, Vec<u8>); N] {
    let input: String = specs.iter().map(|spec| format!("{spec}\n")).collect();
    let out = Git::new(repo)
        .git_input(&["cat-file", "--batch"], input.as_bytes())
        .unwrap();
    assert_eq!(
        out.status,
        0,
        "git cat-file --batch in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut rest = out.stdout.as_slice();
    specs.map(|spec| {
        let eol = rest.iter().position(|&b| b == b'\n').unwrap();
        let header = String::from_utf8(rest[..eol].to_vec()).unwrap();
        let size: usize = match header.rsplit_once(' ') {
            Some((_, size)) if !header.ends_with(" missing") => size.parse().unwrap(),
            _ => panic!("{spec} in {}: {header}", repo.display()),
        };
        let bytes = rest[eol + 1..eol + 1 + size].to_vec();
        rest = &rest[eol + 1 + size + 1..];
        (header, bytes)
    })
}

fn view(repo: &Path, rev: &str) -> View {
    let [(header, commit), (_, ledger_text), (_, changelog_text)] = objects(
        repo,
        [
            format!("{rev}^{{commit}}"),
            format!("{rev}:{}", ledger::LEDGER_FILE),
            format!("{rev}:{}", changelog::CHANGELOG_FILE),
        ],
    );
    let id = header.split(' ').next().unwrap().to_string();
    let parents = String::from_utf8(commit)
        .unwrap()
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("parent "))
        .map(str::to_string)
        .collect();
    let code = git(repo, &["ls-tree", "-r", "--name-only", &id, "--", PEERS])
        .lines()
        .filter(|line| !line.is_empty())
        .count();
    let records = ledger::parse(&String::from_utf8(ledger_text).unwrap()).unwrap();
    let text = String::from_utf8(changelog_text).unwrap();
    View {
        parents,
        code: i64::try_from(code).unwrap(),
        lines: i64::try_from(records.len()).unwrap(),
        tail: ordinal(records.last().unwrap().build),
        section: i64::from(changelog::has_section(&text, VERSION)),
        commit: id,
    }
}

enum Event {
    Attempt(String),
    Env(Env, View),
}

type EnvAction<'a> = Box<dyn FnOnce() -> (Env, View) + 'a>;

struct Hooked<'a> {
    inner: Git,
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

    fn git_input(&self, args: &[&str], input: &[u8]) -> ledger::Result<RunOut> {
        self.inner.git_input(args, input)
    }
}

struct Harness {
    fix: Fixture,
    model: Model,
    state: State,
    source: String,
    source_changelog: String,
    tips: Vec<String>,
    pushes: usize,
    steps: usize,
}

impl Harness {
    fn new(label: &str) -> Self {
        let fix = Fixture::new(label);
        let source = git(&fix.origin, &["rev-parse", "main"]);
        let source_changelog = git(
            &fix.origin,
            &["show", &format!("{source}:{}", changelog::CHANGELOG_FILE)],
        );
        let model = release_claim_landing_model();
        let state = model.init_state();
        let harness = Harness {
            fix,
            model,
            state,
            tips: vec![source.clone()],
            source,
            source_changelog,
            pushes: 0,
            steps: 0,
        };
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

    fn classification(&self, main_changelog: &str) -> (&'static str, State) {
        let published = self.state["published"] == 1;
        match publish::real_cut_version(
            &self.source_changelog,
            main_changelog,
            VERSION,
            &mut |_| Ok(published),
        ) {
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

    fn replay_the_checkout_reader(&mut self) {
        let (action, next) = self.classification(&self.source_changelog);
        assert_eq!(action, "ClassifyFresh", "the checkout never carries it");
        self.refuses(action, &next);
    }

    fn claim(&mut self, during: Option<Env>, bad_landing: bool) -> ledger::Result<ledger::Claim> {
        git(
            &self.fix.work,
            &["checkout", "-q", "--detach", &self.source],
        );
        let k = self.pushes + 1;
        let rival = self.fix.rival.clone();
        let origin = self.fix.origin.clone();
        let hooked = Hooked {
            inner: Git::new(&self.fix.work),
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
        let rolls =
            |source: &str, main: &str| changelog::claim_changelogs(source, main, VERSION, DATE);
        let result = ledger::reserve(&hooked, &plan, &rolls)
            .and_then(|reserved| ledger::claim(&hooked, &self.fix.work, &plan, &reserved, &rolls));
        if during.is_some() {
            self.pushes += 1;
        }

        let mut attempts = 0;
        for event in hooked.events.into_inner() {
            match event {
                Event::Attempt(landed) => {
                    let landing = view(&self.fix.work, &landed);
                    let (read, release) = match landing.parents.as_slice() {
                        [tip, release] => (tip.clone(), release.clone()),
                        [published] => (published.clone(), landed.clone()),
                        other => panic!("a landing with parents {other:?}"),
                    };
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
                let before = main.parents[0].clone();
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
            Err(error) if error.to_string().contains("at or past the reserved build") => {
                let next = self.with(&[("phase", 1)]);
                self.step("ClaimStale", next);
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

#[test]
fn the_real_claim_and_reader_refine_the_model_through_a_wedge_and_a_recut() {
    let mut h = Harness::new("wedge");
    h.env(Env::Peer);
    assert_eq!(h.classify(), "ClassifyFresh");
    let stale = h
        .claim(Some(Env::Rival), false)
        .expect_err("a reservation another claim reached is never landed");
    assert!(stale.to_string().contains("nothing was burned"), "{stale}");
    assert_eq!(h.state["phase"], 1, "the release reserves again");
    let first = h
        .claim(None, false)
        .expect("a fresh reservation lands above the rival's line");
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

#[test]
fn an_unmoved_main_takes_the_release_commit_as_a_fast_forward() {
    let mut h = Harness::new("fast-forward");
    assert_eq!(h.classify(), "ClassifyFresh");
    let claim = h.claim(None, false).unwrap();
    assert_eq!(claim.landed, claim.commit);
    h.publish();
    assert_eq!(h.classify(), "ClassifyRefuse");
}

#[test]
fn another_machines_claim_of_the_version_is_the_cut_elsewhere_abort() {
    let mut h = Harness::new("elsewhere");
    assert_eq!(h.classify(), "ClassifyFresh");
    h.claim(Some(Env::Elsewhere), false)
        .expect_err("cut elsewhere");
    assert_eq!(h.state["phase"], 5);
    assert_eq!(h.state["foreign"], 1);
}
