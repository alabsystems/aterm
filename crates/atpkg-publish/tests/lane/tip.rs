// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tip lane and the Apple ready dispatcher, over the fake channel, real git and a scripted
//! backend: what a pass freezes, skips, refuses, qualifies and enqueues, and what the dispatcher
//! publishes from the queue.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atpkg_publish::api::{
    Baseline, IndexRequest, Indexer, PackOutcome, PackRequest, Packer, Published,
};
use atpkg_publish::artifact::qualify::Runner;
use atpkg_publish::artifact::tools::{Ran, Run, RunError};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::{Error, Result};
use atpkg_publish::host::Replace;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::lane::install::InstallFlags;
use atpkg_publish::lane::lifecycle::{Registry, Standing};
use atpkg_publish::lane::ready::{self, Record};
use atpkg_publish::lane::report::Transcript;
use atpkg_publish::lane::state::LaneState;
use atpkg_publish::lane::tip::{self, Backend, Decision, Facts, Fate, Frozen, Staged};
use atpkg_publish::lane::units::{self, UnitSpec};
use atpkg_publish::lane::{LaneKind, RunFlags};
use atpkg_publish::testkit::Keys;
use atpkg_publish::triple::{AARCH64_APPLE_DARWIN as ARM, Triple, X86_64_APPLE_DARWIN as X86};

use super::fixture::{self, Fx, git};

const REGISTRY: &str = r#"
order = ["ay"]

[repos.ay]
[repos.nn]
[repos.ny]
[repos.ty]
[repos.clean]
[repos.astream]
[repos.aterm]
[repos.trust-ir]

[repos.muse-alab]
public_release = true

[repos.claude-code-alab]
public_release = false

[lifecycle.ny]
state = "on-hold"
decision_date = "2026-11-05"
reason = "left alone for now"
"#;

/// A manifest that declares a list, as far as a pass can tell before it reads the recipe.
const MANIFEST: &str = "[workspace]\n\n[[workspace.metadata.atpkg.qualify]]\nname = \"build\"\n";

const FMT: &str =
    r#"{"name":"fmt","stage":"source","run":["targo","fmt","--check"],"within-secs":120}"#;
const UNIT: &str = r#"{"name":"unit","stage":"source","run":["targo","--unverified","test","--locked"],"within-secs":1800,"min-tests":40}"#;
const BUILD: &str = r#"{"name":"build","stage":"build","within-secs":3600}"#;
const VERSION: &str =
    r#"{"name":"version","stage":"packed","run":["{bin}/ay","--version"],"within-secs":30}"#;

/// `targo metadata`'s JSON for a one-package workspace `program` whose recipe carries `steps`.
fn recipe(program: &str, steps: Option<&[&str]>) -> String {
    let qualify = steps
        .map(|s| format!(r#","qualify":[{}]"#, s.join(",")))
        .unwrap_or_default();
    format!(
        r#"{{"packages":[{{"name":"{program}","version":"1.0.0","manifest_path":"/w/Cargo.toml"}}],
            "metadata":{{"atpkg":{{"expose":["{program}"]{qualify}}}}}}}"#
    )
}

fn summary(passed: u64) -> String {
    format!(
        "running {passed} tests\n\ntest result: ok. {passed} passed; 0 failed; 0 ignored; 0 \
         measured; 0 filtered out; finished in 0.01s\n"
    )
}

fn exit(code: i32, stdout: &str) -> std::result::Result<Ran, RunError> {
    Ok(Ran {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    })
}

type Hook = Box<dyn Fn(&Run) + Send>;

/// The scripted backend: recipes by repository, answers by a word of the command, and a record
/// of everything it was asked, in order.
#[derive(Default)]
struct Scripted {
    events: Arc<Mutex<Vec<String>>>,
    recipes: Mutex<BTreeMap<String, String>>,
    answers: Mutex<Vec<(String, std::result::Result<Ran, RunError>)>>,
    hooks: Mutex<Vec<(String, Hook)>>,
    asked: Mutex<Vec<Run>>,
    refuse_stage: AtomicBool,
    /// The staging owner does not answer: a could-not-run, nothing about the candidate.
    staging_down: AtomicBool,
}

impl Scripted {
    fn recipe(&self, repo: &str, json: String) {
        self.recipes
            .lock()
            .expect("recipes")
            .insert(repo.to_string(), json);
    }

    /// Answer the command containing `word` with `answer` (the newest answer for a word wins).
    fn on(&self, word: &str, answer: std::result::Result<Ran, RunError>) {
        self.answers
            .lock()
            .expect("answers")
            .insert(0, (word.to_string(), answer));
    }

    /// Run `hook` when the command containing `word` is asked for.
    fn hook(&self, word: &str, hook: impl Fn(&Run) + Send + 'static) {
        self.hooks
            .lock()
            .expect("hooks")
            .push((word.to_string(), Box::new(hook)));
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events").clone()
    }

    fn asked(&self) -> Vec<Run> {
        self.asked.lock().expect("asked").clone()
    }

    fn event(&self, what: String) {
        self.events.lock().expect("events").push(what);
    }
}

impl Runner for Scripted {
    fn run(&self, run: &Run) -> std::result::Result<Ran, RunError> {
        self.asked.lock().expect("asked").push(run.clone());
        let line = format!("{} {}", run.program.display(), run.argv().join(" "));
        for (word, hook) in self.hooks.lock().expect("hooks").iter() {
            if line.contains(word.as_str()) {
                hook(run);
            }
        }
        let (word, answer) = self
            .answers
            .lock()
            .expect("answers")
            .iter()
            .find(|(word, _)| line.contains(word.as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("the script has no answer for `{line}`"));
        self.event(format!("run {word}"));
        answer
    }
}

impl Backend for Scripted {
    fn metadata(&self, _ctx: &Ctx, checkout: &Path) -> Result<Vec<u8>> {
        let repo = checkout
            .file_name()
            .expect("a checkout directory")
            .to_string_lossy()
            .into_owned();
        self.event(format!("metadata {repo}"));
        self.recipes
            .lock()
            .expect("recipes")
            .get(&repo)
            .map(|json| json.clone().into_bytes())
            .ok_or_else(|| Error::refused(format!("fixture: no recipe for {repo}")))
    }

    fn unpack(&self, asset: &Path, into: &Path) -> Result<()> {
        self.event(format!(
            "unpack {}",
            asset.file_name().expect("an asset").to_string_lossy()
        ));
        std::fs::create_dir_all(into.join("bin")).expect("mkdir the unpacked tree");
        Ok(())
    }

    fn stage(&self, ctx: &Ctx, staged: &Staged<'_>) -> Result<()> {
        if self.refuse_stage.load(Ordering::SeqCst) {
            return Err(Error::refused("fixture: the upload was refused"));
        }
        if self.staging_down.load(Ordering::SeqCst) {
            return Err(Error::cannot_run(
                "fixture: the staging owner did not answer",
            ));
        }
        self.event(format!("stage {}={}", staged.def.name, staged.build));
        let slug = ctx.staging_slug(staged.def.repo);
        let tag = format!("atpkg-{}-{}", staged.def.name, staged.build);
        let mut files = staged.assets.to_vec();
        files.push(staged.manifest.to_path_buf());
        files.push(staged.signature.to_path_buf());
        let host = &ctx.hosts.staging;
        let sent = if host.release(&slug, &tag).is_ok() {
            host.upload(&slug, &tag, &files, Replace::Clobber)
        } else {
            host.create(&slug, &tag, &tag, "", &files)
        };
        sent.map_err(|e| Error::refused(format!("fixture upload: {e}")))
    }
}

/// The fixture packer, with each pack and the commit its checkout was at recorded in order.
struct Recording<'a> {
    inner: &'a fixture::FakePacker,
    events: Arc<Mutex<Vec<String>>>,
    /// A target whose pack fails.
    broken: Option<Triple>,
}

impl Packer for Recording<'_> {
    fn can_pack(&self) -> std::result::Result<(), String> {
        self.inner.can_pack()
    }

    fn pack(&self, ctx: &Ctx, request: &PackRequest) -> Result<PackOutcome> {
        let at = git(&request.checkout, &["rev-parse", "HEAD"]);
        let mut events = self.events.lock().expect("events");
        events.push(format!("pack {} at {at}", request.triple));
        events.push(format!(
            "build bound {:?}",
            request.build_within.map(|within| within.as_secs())
        ));
        // What the build would read besides the commit's own files.
        let stray = git(
            &request.checkout,
            &["status", "--porcelain", "--ignored=matching"],
        );
        if stray.lines().any(|line| line != "!! target/") {
            events.push(format!("build saw {stray}"));
        }
        drop(events);
        assert!(!request.upload, "nothing is uploaded before it qualifies");
        if self.broken == Some(request.triple) {
            return Err(Error::refused("fixture: this target does not build"));
        }
        self.inner.pack(ctx, request)
    }
}

/// The channel's index text: `pin`, and one `pin_by_target` overlay per target.
fn index_text(
    keys: &Keys,
    n: u64,
    pin: &BTreeMap<String, u64>,
    overlays: &BTreeMap<String, BTreeMap<String, u64>>,
) -> String {
    let programs: Vec<(&str, &str)> = pin
        .keys()
        .map(|p| {
            (
                p.as_str(),
                atpkg_publish::programs::get(p).map_or(p.as_str(), |d| d.repo),
            )
        })
        .collect();
    let pins: Vec<(&str, u64)> = pin.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    let mut text = keys.index_toml(n, &programs, &pins);
    for (target, cells) in overlays.iter().filter(|(_, cells)| !cells.is_empty()) {
        text.push_str(&format!("\n[channels.pin_by_target.{target}]\n"));
        for (program, build) in cells {
            text.push_str(&format!("{program} = {build}\n"));
        }
    }
    text
}

/// An indexer that applies a delta as the channel does (`pin` moves and per-target overlays)
/// over the verified baseline it is given, and signs index N+1.
struct Overlaying {
    keys: Arc<Keys>,
    host: Arc<FakeHost>,
    requests: Mutex<Vec<IndexRequest>>,
}

impl Overlaying {
    fn count(&self) -> usize {
        self.requests.lock().expect("requests").len()
    }

    /// Each publish's whole delta: its `pin` moves and its per-target moves.
    #[allow(clippy::type_complexity)]
    fn deltas(
        &self,
    ) -> Vec<(
        BTreeMap<String, u64>,
        BTreeMap<Triple, BTreeMap<String, u64>>,
    )> {
        self.requests
            .lock()
            .expect("requests")
            .iter()
            .map(|r| (r.delta.pins.clone(), r.delta.target_pins.clone()))
            .collect()
    }
}

impl Indexer for Overlaying {
    fn publish(&self, ctx: &Ctx, request: &IndexRequest) -> Result<Published> {
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        let Baseline::Dir(dir) = &request.baseline else {
            return Err(Error::usage(
                "the lanes baseline on the verified head they read",
            ));
        };
        let head = atpkg_publish::quad::verify_dir(ctx, dir)?;
        let n = head.index.index_build + 1;
        let channel = &head.index.channels[0];
        let mut pin = channel.pin.clone();
        pin.extend(request.delta.pins.clone());
        let mut overlays = channel.pin_by_target.clone();
        for (target, cells) in &request.delta.target_pins {
            overlays
                .entry(target.as_str().to_string())
                .or_default()
                .extend(cells.clone());
        }
        let text = index_text(&self.keys, n, &pin, &overlays);
        self.keys
            .publish_quad(&self.host, &ctx.index_slug(), n, &text);
        Ok(Published {
            index_build: n,
            dir: request.out.clone(),
            uploaded: request.upload,
        })
    }
}

/// A world whose head (index 5) pins ay 2, nn 3, ny 2, ty 9, astream 4, claude-code-alab 2,
/// trust-ir 1 and atpkg-keys 9, with Apple silicon alone moved on to ay 3 (a `pin_by_target`
/// overlay, as a target-pin publish leaves the channel), over dev repos ay (5 commits), nn (3),
/// ny (4) and ty (4).
struct World {
    fx: Fx,
    backend: Scripted,
    indexer: Overlaying,
    ay: Vec<String>,
}

impl World {
    fn new(label: &str) -> Self {
        let fx = Fx::new(label);
        // ay's main (build 5) carries a manifest that spells a qualification list: the recipe a
        // pass reads from it is the scripted backend's.
        let (dev, mut ay) = fx.dev_repo("ay", 4);
        std::fs::write(dev.join("Cargo.toml"), MANIFEST).expect("ay's manifest");
        git(&dev, &["add", "Cargo.toml"]);
        ay.push(fixture::commit(
            &dev,
            "a manifest with a qualify list",
            None,
        ));
        let _ = fx.dev_repo("nn", 3);
        let _ = fx.dev_repo("ny", 4);
        let _ = fx.dev_repo("ty", 4);
        let _ = fx.dev_repo("claude-code-alab", 4);
        let indexer = Overlaying {
            keys: fx.keys.clone(),
            host: fx.host.clone(),
            requests: Mutex::new(Vec::new()),
        };
        let world = Self {
            fx,
            backend: Scripted::default(),
            indexer,
            ay,
        };
        world.publish(
            5,
            &[
                ("ay", 2),
                ("nn", 3),
                ("ny", 2),
                ("ty", 9),
                ("astream", 4),
                ("claude-code-alab", 2),
                ("trust-ir", 1),
                ("atpkg-keys", 9),
            ],
            &[("ay", 3)],
        );
        world.registry(REGISTRY);
        world
            .backend
            .recipe("ay", recipe("ay", Some(&[FMT, UNIT, BUILD, VERSION])));
        world.backend.on("fmt --check", exit(0, ""));
        world.backend.on("test --locked", exit(0, &summary(50)));
        world.backend.on("--version", exit(0, "ay 1.0.0\n"));
        world
    }

    /// Publish index `n`: `pin`, and Apple silicon's overlay `arm`.
    fn publish(&self, n: u64, pin: &[(&str, u64)], arm: &[(&str, u64)]) {
        let own = |cells: &[(&str, u64)]| -> BTreeMap<String, u64> {
            cells.iter().map(|(p, b)| ((*p).to_string(), *b)).collect()
        };
        let overlays = BTreeMap::from([(ARM.as_str().to_string(), own(arm))]);
        self.fx
            .publish_index_text(n, &index_text(&self.fx.keys, n, &own(pin), &overlays));
    }

    /// Another publisher's index `n`: the head as it is, with Apple silicon moved to `arm`.
    fn publish_over(&self, n: u64, arm: &[(&str, u64)]) {
        let head = self.head();
        let channel = &head.1.channels[0];
        let mut overlays = channel.pin_by_target.clone();
        for (program, build) in arm {
            overlays
                .entry(ARM.as_str().to_string())
                .or_default()
                .insert((*program).to_string(), *build);
        }
        self.fx
            .publish_index_text(n, &index_text(&self.fx.keys, n, &channel.pin, &overlays));
    }

    /// Another publisher's index `n`: the head as it is, with `entry` (`<program>@<build>`)
    /// yanked.
    fn publish_yanking(&self, n: u64, entry: &str) {
        let head = self.head();
        let channel = &head.1.channels[0];
        let text = index_text(&self.fx.keys, n, &channel.pin, &channel.pin_by_target).replace(
            "min_build = 0\n",
            &format!("min_build = 0\nyanked = [\"{entry}\"]\n"),
        );
        assert!(text.contains("yanked"), "{text}");
        self.fx.publish_index_text(n, &text);
    }

    /// Stage `program` `build` for Apple silicon and queue a record that binds it, as a pass
    /// that qualified it over `pin` leaves them (`like`, another build's record, lends the
    /// steps).
    fn queue(&self, like: &Record, program: &str, build: u64, pin: u64) -> Record {
        self.fx.stage(program, build, &[ARM]);
        let manifest = self
            .fx
            .staging_asset(program, build, &format!("pkg-{program}-{build}.toml"))
            .expect("staged");
        let record = Record {
            program: program.into(),
            repo: program.into(),
            source: fixture::hex40(program),
            build,
            manifest_sha256: atpkg::tree::sha256_bytes(&manifest),
            pin_before: pin,
            ..like.clone()
        };
        ready::enqueue(&self.state(), &record).expect("queued");
        record
    }

    /// The engine's main holds `text` as its registry.
    fn registry(&self, text: &str) {
        let origin = self.fx.engine_origin();
        if !origin.join(".git").exists() {
            std::fs::create_dir_all(&origin).expect("mkdir the engine origin");
            git(&origin, &["init", "-q"]);
            self.fx
                .host
                .add_clone_source(&self.fx.ctx.staging_slug("publication"), &origin);
        }
        std::fs::create_dir_all(origin.join("registry")).expect("mkdir registry");
        std::fs::write(origin.join("registry/repos.toml"), text).expect("write the registry");
        git(&origin, &["add", "registry/repos.toml"]);
        fixture::commit(&origin, "registry", None);
    }

    fn state(&self) -> LaneState {
        LaneState::of(LaneKind::Tip, &self.fx.ctx.state_dir)
    }

    fn pass_with(
        &self,
        dry_run: bool,
        broken: Option<Triple>,
    ) -> (Result<tip::Outcome>, Transcript) {
        self.pass_as(ARM, dry_run, broken)
    }

    /// A pass on a machine that runs `host`.
    fn pass_as(
        &self,
        host: Triple,
        dry_run: bool,
        broken: Option<Triple>,
    ) -> (Result<tip::Outcome>, Transcript) {
        let tx = Transcript::memory(tip::ME);
        let packer = Recording {
            inner: &self.fx.packer,
            events: self.backend.events.clone(),
            broken,
        };
        let deps = tip::Deps {
            packer: &packer,
            backend: &self.backend,
            notifier: &self.fx.notes,
        };
        let opts = tip::Options::new(&self.fx.ctx, dry_run, host);
        (tip::run(&self.fx.ctx, &opts, &deps, &tx), tx)
    }

    fn pass(&self) -> (tip::Outcome, Transcript) {
        match self.pass_with(false, None) {
            (Ok(outcome), tx) => (outcome, tx),
            (Err(e), tx) => panic!("the pass refused: {e}\n{}", tx.text()),
        }
    }

    fn dispatch_with(
        &self,
        indexer: &dyn Indexer,
        dry_run: bool,
        queued: bool,
    ) -> (Result<ready::Outcome>, Transcript) {
        let tx = Transcript::memory(ready::ME);
        let deps = ready::Deps {
            indexer,
            notifier: &self.fx.notes,
        };
        let opts = ready::Options::new(&self.fx.ctx, dry_run, queued, ARM);
        (ready::dispatch(&self.fx.ctx, &opts, &deps, &tx), tx)
    }

    fn dispatch(&self) -> (ready::Outcome, Transcript) {
        match self.dispatch_with(&self.indexer, false, true) {
            (Ok(outcome), tx) => (outcome, tx),
            (Err(e), tx) => panic!("the dispatch refused: {e}\n{}", tx.text()),
        }
    }

    /// The record queued for `program` `build`, if one is.
    fn record(&self, program: &str, build: u64) -> Option<Record> {
        let path = self
            .state()
            .ready_dir()
            .join(format!("{program}-{build}.ready.toml"));
        let text = std::fs::read_to_string(path).ok()?;
        Some(aterm_toml::from_str(&text).expect("a record that parses"))
    }

    fn queued(&self) -> Vec<String> {
        ready::pending(&self.state())
            .expect("a readable queue")
            .into_iter()
            .map(|q| {
                q.path
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// The verified head.
    fn head(&self) -> (u64, atpkg::Index) {
        let scratch = self
            .fx
            .path(&format!("read-head-{}", self.fx.host.calls().len()));
        let (n, head) =
            atpkg_publish::quad::fetch_head(&self.fx.ctx, &self.fx.ctx.index_slug(), &scratch)
                .expect("a head");
        (n, (*head.index).clone())
    }

    /// The head's pins as a client on `target` resolves them.
    fn pins_on(&self, target: Triple) -> (u64, BTreeMap<String, u64>) {
        let (n, index) = self.head();
        (n, index.channels[0].for_target(target.as_str()).pin)
    }

    /// The head's pins as Apple silicon resolves them.
    fn pins(&self) -> (u64, BTreeMap<String, u64>) {
        self.pins_on(ARM)
    }
}

fn row<'a>(outcome: &'a tip::Outcome, program: &str) -> &'a tip::Row {
    outcome
        .rows
        .iter()
        .find(|r| r.program.name == program)
        .unwrap_or_else(|| panic!("no {program} row"))
}

/// Every file under `root` with its length, for "nothing was written".
fn listing(root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).expect("stat");
            if meta.is_dir() {
                out.push((path.clone(), 0));
                walk(&path, out);
            } else {
                out.push((path, meta.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

#[test]
fn the_decision_is_refusal_then_pin_then_the_frozen_build() {
    let frozen = |build| {
        Some(Ok(Frozen {
            sha: "a".repeat(40),
            build,
        }))
    };
    let facts = Facts {
        refusal: None,
        pin: Some(3),
        frozen: frozen(5),
        enqueued: false,
        latched: false,
    };
    assert_eq!(tip::decide(&facts), Decision::Qualify);
    // A refusal decides before anything a repository says.
    let refused = Facts {
        refusal: Some("on hold".into()),
        ..facts.clone()
    };
    assert_eq!(tip::decide(&refused), Decision::Refused("on hold".into()));
    // A program the head does not pin is never first-pinned by a lane.
    assert_eq!(
        tip::decide(&Facts {
            pin: None,
            ..facts.clone()
        }),
        Decision::Unpinned
    );
    assert_eq!(
        tip::decide(&Facts {
            frozen: frozen(3),
            ..facts.clone()
        }),
        Decision::Current
    );
    assert_eq!(
        tip::decide(&Facts {
            frozen: frozen(2),
            ..facts.clone()
        }),
        Decision::Behind
    );
    // A queued build is not rebuilt, and a latch is not retried; the queue is asked first.
    assert_eq!(
        tip::decide(&Facts {
            enqueued: true,
            latched: true,
            ..facts.clone()
        }),
        Decision::Enqueued
    );
    assert_eq!(
        tip::decide(&Facts {
            latched: true,
            ..facts.clone()
        }),
        Decision::Blocked
    );
    // A main that cannot be read is never read as "nothing to build" or as current.
    for unread in [
        Facts {
            frozen: Some(Err("no main".into())),
            ..facts.clone()
        },
        Facts {
            frozen: None,
            ..facts.clone()
        },
    ] {
        assert!(
            matches!(tip::decide(&unread), Decision::Unread(_)),
            "{unread:?}"
        );
    }
}

#[test]
fn the_registry_says_which_repositories_a_lane_may_work_on() {
    let registry = Registry::parse(REGISTRY).expect("a registry");
    assert_eq!(registry.standing("ay"), Standing::Active);
    assert_eq!(registry.standing("muse-alab"), Standing::Active);
    assert_eq!(
        registry.standing("claude-code-alab"),
        Standing::PublicClosed
    );
    assert_eq!(registry.standing("orca"), Standing::Unregistered);
    let Standing::Inactive { state, reason } = registry.standing("ny") else {
        panic!("ny is on hold");
    };
    assert_eq!(
        (state.as_str(), reason.as_deref()),
        ("on-hold", Some("left alone for now"))
    );
    // A lifecycle record decides for a project the registry does not otherwise manage, and an
    // explicit `active` is no hold.
    let more = Registry::parse(
        "[repos.ay]\n[lifecycle.ay]\nstate = \"active\"\n[lifecycle.mly]\nstate = \"retired\"\n",
    )
    .expect("a registry");
    assert_eq!(more.standing("ay"), Standing::Active);
    assert!(matches!(more.standing("mly"), Standing::Inactive { .. }));
    // What cannot be read is never read as open or active.
    for (text, want) in [
        ("order = []\n", "no [repos] table"),
        (
            "[repos.ay]\npublic_release = \"no\"\n",
            "public_release is not a boolean",
        ),
        (
            "[repos.ay]\n[lifecycle.ay]\nreason = \"x\"\n",
            "names no state",
        ),
        ("[repos.ay\n", "not TOML"),
    ] {
        let said = Registry::parse(text).expect_err("undecidable");
        assert!(said.contains(want), "{text:?}: {said}");
    }
    for (repo, want) in [
        (
            "ny",
            "`on-hold` in the lifecycle registry (left alone for now)",
        ),
        ("claude-code-alab", "public tier is closed"),
        ("orca", "not a repository the registry manages"),
    ] {
        let why = registry.standing(repo).refusal(repo).expect("a refusal");
        assert!(why.starts_with(repo) && why.contains(want), "{why}");
    }
    assert_eq!(registry.standing("ay").refusal("ay"), None);
}

#[test]
fn a_pass_freezes_main_qualifies_packs_stages_and_enqueues_and_publishes_nothing() {
    let w = World::new("tip-pass");
    let changes_before = w.fx.host.changes();
    let (outcome, tx) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=5"], "{}", tx.text());
    assert!(outcome.failed.is_empty());
    let ay = row(&outcome, "ay");
    assert_eq!(ay.frozen.as_ref().expect("frozen").sha, w.ay[4]);
    assert!(matches!(ay.fate, Some(Fate::Enqueued(_))));
    // nn's main is its pin: nothing is built for it.
    assert_eq!(row(&outcome, "nn").decision, Decision::Current);
    // ty's pin is ahead of its main: a pack cut from another line, left alone.
    assert_eq!(row(&outcome, "ty").decision, Decision::Behind);
    // astream's main cannot be read: skipped this pass, not treated as current.
    assert!(matches!(
        row(&outcome, "astream").decision,
        Decision::Unread(_)
    ));

    // Cheap source checks, then the pack of this machine's target at the frozen commit, then the
    // control on the unpacked asset, and only then the upload.
    let frozen = &w.ay[4];
    assert_eq!(
        w.backend.events(),
        [
            "metadata ay".to_string(),
            "run fmt --check".into(),
            "run test --locked".into(),
            format!("pack {ARM} at {frozen}"),
            // The build's committed bound is the pack's: its build is ended at it.
            "build bound Some(3600)".into(),
            format!("unpack ay-5-{ARM}.tar.zst"),
            "run --version".into(),
            "stage ay=5".into(),
        ]
    );
    let asked = w.backend.asked();
    let clone = w.state().dir.join("progs/ay");
    assert_eq!(asked[0].cwd.as_deref(), Some(clone.as_path()));
    assert!(
        atpkg_publish::artifact::package::names_repo(
            &git(&clone, &["remote", "get-url", "origin"]),
            &w.fx.ctx.accounts.staging,
            "ay"
        ),
        "the scratch repository says whose source it holds"
    );
    let unpacked = w.state().dir.join("unpacked");
    assert!(
        asked[2].program.starts_with(&unpacked) && asked[2].program.ends_with("bin/ay"),
        "{}",
        asked[2].program.display()
    );
    // The tree the control ran from is stamped before its first exec and stays on disk while
    // macOS may still be checking that exec; a later pass removes it once the hold is over.
    let tree = asked[2]
        .program
        .parent()
        .and_then(Path::parent)
        .expect("the unpacked tree")
        .to_path_buf();
    let held = |dir: &Path| {
        aterm_uds::deadline::copy_wait_left(dir, std::time::SystemTime::now()).is_some()
    };
    assert!(tree.join(aterm_uds::deadline::EXEC_STAMP).is_file() && held(&tree));
    w.pass();
    assert!(tree.is_dir(), "a held tree survives the next pass");
    std::fs::write(
        tree.join(aterm_uds::deadline::EXEC_STAMP),
        u64::MAX.to_string(),
    )
    .expect("spend the stamp");
    assert!(!held(&tree));
    w.pass();
    assert!(!tree.exists(), "its hold over, the tree is removed");
    assert_ne!(
        asked[2].cwd.as_deref(),
        Some(clone.as_path()),
        "a packed control does not lean on the checkout"
    );
    // One row is packed: Apple silicon's. The head pins x86_64 at another build (ay 2 against
    // 3), which is no obstacle to a pass that serves one target.
    let legs = w.fx.packer.legs();
    assert_eq!(legs.len(), 1);
    assert_eq!((legs[0].1, legs[0].2.as_ref()), (ARM, None));
    assert_eq!(w.pins_on(X86).1["ay"], 2);

    // Staged privately; nothing public, and no index: that is the dispatcher's.
    let staged =
        w.fx.staging_asset("ay", 5, "pkg-ay-5.toml")
            .expect("staged");
    assert!(w.fx.staging_asset("ay", 5, "pkg-ay-5.toml.sig").is_some());
    assert!(
        w.fx.staging_asset("ay", 5, &format!("ay-5-{ARM}.tar.zst"))
            .is_some()
    );
    assert_eq!(w.fx.public_asset("ay", 5, "pkg-ay-5.toml"), None);
    assert_eq!(
        w.fx.host.changes(),
        changes_before + 1,
        "one staging release"
    );
    assert_eq!(w.pins().0, 5, "the head is index 5 still");

    // The ready record binds the frozen commit and the exact signed manifest that was staged.
    let record = w.record("ay", 5).expect("queued");
    assert_eq!(record.check(), Ok(()));
    assert_eq!(
        (record.source.as_str(), record.build, record.pin_before),
        (frozen.as_str(), 5, 3)
    );
    assert_eq!(record.manifest_sha256, atpkg::tree::sha256_bytes(&staged));
    assert_eq!(record.target, ARM.as_str());
    assert_eq!(record.targets, [ARM.as_str()]);
    assert_eq!(record.decided_over_index, 5);
    let steps: Vec<(&str, &str, i64)> = record
        .steps
        .iter()
        .map(|s| (s.name.as_str(), s.stage.as_str(), s.exit))
        .collect();
    assert_eq!(
        steps,
        [
            ("fmt", "source", 0),
            ("unit", "source", 0),
            ("build", "build", 0),
            ("version", "packed", 0)
        ]
    );
    assert_eq!(record.steps[1].tests_ran, Some(50));
    assert_eq!(record.steps[3].target.as_deref(), Some(ARM.as_str()));

    // Its qualification receipt and raw step output stay on disk.
    let receipt = std::fs::read_to_string(w.state().dir.join("receipts/ay-5.json")).expect("kept");
    let receipt: aterm_json::Value = aterm_json::from_str(&receipt).expect("JSON");
    assert_eq!(receipt["outcome"].as_str(), Some("enqueued"));
    assert_eq!(receipt["source"].as_str(), Some(frozen.as_str()));
    assert!(
        w.state()
            .dir
            .join("logs/ay-5/attempt-1/unit.stdout")
            .is_file()
    );
}

#[test]
fn main_moving_during_a_pass_never_moves_what_was_frozen() {
    let w = World::new("tip-freeze");
    let dev = w.fx.path("dev/ay");
    // Main advances while the first source check runs: 18 times an hour, it will.
    w.backend.hook("fmt --check", move |_| {
        fixture::commit(&dev, "main moved", None);
    });
    let (outcome, tx) = w.pass();
    let frozen = &w.ay[4];
    assert_eq!(outcome.enqueued, ["ay=5"], "{}", tx.text());
    assert_eq!(w.record("ay", 5).expect("queued").source, *frozen);
    let packs: Vec<String> = w
        .backend
        .events()
        .into_iter()
        .filter(|e| e.starts_with("pack "))
        .collect();
    assert_eq!(
        packs,
        [format!("pack {ARM} at {frozen}")],
        "the moved main was never built"
    );
    // One read of main for the whole pass.
    let slug = w.fx.ctx.staging_slug("ay");
    assert_eq!(w.fx.calls(&format!("fetch-main {slug}")).len(), 1);

    // The moved main is the next pass's candidate, frozen afresh (and it moves again under
    // that pass, which builds what it froze all the same).
    let moved = git(&w.fx.path("dev/ay"), &["rev-parse", "HEAD"]);
    assert_ne!(moved, *frozen);
    let (outcome, tx) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=6"], "{}", tx.text());
    assert_eq!(w.record("ay", 6).expect("queued").source, moved);
    assert_ne!(git(&w.fx.path("dev/ay"), &["rev-parse", "HEAD"]), moved);
    assert_eq!(w.queued(), ["ay-5.ready.toml", "ay-6.ready.toml"]);
}

#[test]
fn a_build_the_index_pins_or_the_queue_holds_is_not_built_again() {
    let w = World::new("tip-skip");
    w.pass();
    let (packs, runs) = (w.fx.packer.legs().len(), w.backend.asked().len());
    let (outcome, tx) = w.pass();
    assert_eq!(
        row(&outcome, "ay").decision,
        Decision::Enqueued,
        "{}",
        tx.text()
    );
    assert!(row(&outcome, "ay").render(5).starts_with("READY:"));
    assert!(outcome.enqueued.is_empty());
    assert_eq!(
        (w.fx.packer.legs().len(), w.backend.asked().len()),
        (packs, runs),
        "nothing was qualified or packed twice"
    );
    assert!(tx.text().contains("clean no-op"), "{}", tx.text());
    // Once an index pins that build for this target, the frozen main is simply current.
    w.publish_over(6, &[("ay", 5)]);
    let (outcome, _) = w.pass();
    assert_eq!(row(&outcome, "ay").decision, Decision::Current);
    assert_eq!(w.fx.packer.legs().len(), packs);
}

#[test]
fn closed_tiers_holds_groups_and_unpinned_programs_are_refused_by_name_and_never_read() {
    let w = World::new("tip-refusals");
    let (outcome, tx) = w.pass();
    for (program, want) in [
        ("ny", "ny is `on-hold` in the lifecycle registry"),
        (
            "claude-code-alab",
            "claude-code-alab's public tier is closed",
        ),
        ("trust-ir", "trust-ir moves with coherence group `rustc`"),
        ("atpkg-keys", "atpkg-keys is built from aterm's repository"),
    ] {
        let r = row(&outcome, program);
        assert!(
            matches!(&r.decision, Decision::Refused(why) if why.contains(want)),
            "{program}: {:?}",
            r.decision
        );
        assert!(r.render(5).starts_with("REFUSED:"));
        assert!(r.frozen.is_none());
        assert!(
            outcome.skipped.iter().any(|s| s.starts_with(program)),
            "{:?}",
            outcome.skipped
        );
    }
    // clean is registered and active, but index 5 does not pin it: never first-pinned by a lane.
    assert_eq!(row(&outcome, "clean").decision, Decision::Unpinned);
    assert!(
        row(&outcome, "clean")
            .render(5)
            .contains("not pinned by index-5")
    );
    // None of their repositories was read, cloned or built.
    for repo in ["ny", "claude-code-alab", "trust-ir", "aterm", "clean"] {
        let slug = w.fx.ctx.staging_slug(repo);
        assert!(
            w.fx.calls("fetch-main")
                .iter()
                .all(|c| !c.ends_with(&slug.to_string())),
            "{repo} was read: {:?}",
            w.fx.calls("fetch-main")
        );
        assert!(!w.state().dir.join("progs").join(repo).exists());
    }
    assert!(
        tx.text()
            .contains("REFUSED: claude-code-alab's public tier is closed")
    );

    // On an Intel Mac, a program built for Apple silicon alone is none of the pass's.
    let (r, _) = w.pass_as(X86, false, None);
    let outcome = r.expect("a pass");
    assert!(
        matches!(&row(&outcome, "muse-alab").decision, Decision::Refused(why)
            if why.contains(&format!("muse-alab ships no {X86} row"))),
        "{:?}",
        row(&outcome, "muse-alab").decision
    );

    // With no registry to read, nothing is built at all.
    let blind = World::new("tip-no-registry");
    std::fs::remove_dir_all(blind.fx.engine_origin().join(".git")).expect("drop the engine");
    let (r, _) = blind.pass_with(false, None);
    let e = r.expect_err("no lifecycle authority");
    assert!(matches!(e, Error::CannotRun(_)), "{e}");
    assert!(
        e.to_string()
            .contains("lifecycle registry could not be read"),
        "{e}"
    );
    assert!(blind.fx.packer.legs().is_empty());
}

#[test]
fn a_run_that_matched_zero_tests_fails_the_candidate_before_anything_is_built() {
    let w = World::new("tip-zero");
    w.backend.on(
        "test --locked",
        exit(0, "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n"),
    );
    let (r, tx) = w.pass_with(false, None);
    let e = r.expect_err("the only candidate failed");
    assert!(
        e.to_string().contains("every candidate failed to qualify"),
        "{e}"
    );
    assert!(e.to_string().contains("matched zero tests"), "{e}");
    assert!(tx.text().contains("attempt 1 of 3"), "{}", tx.text());
    // Nothing was packed, staged or queued; the raw output and the receipt are kept.
    assert!(w.fx.packer.legs().is_empty());
    assert_eq!(w.fx.staging_asset("ay", 5, "pkg-ay-5.toml"), None);
    assert!(w.queued().is_empty());
    let state = w.state();
    assert!(state.dir.join("logs/ay-5/attempt-1/unit.stdout").is_file());
    let receipt = std::fs::read_to_string(state.dir.join("receipts/ay-5.json")).expect("kept");
    let receipt: aterm_json::Value = aterm_json::from_str(&receipt).expect("JSON");
    assert_eq!(receipt["outcome"].as_str(), Some("failed"));
    assert_eq!(receipt["steps"][1]["passed"].as_bool(), Some(false));
    assert_eq!(receipt["steps"][1]["exit"].as_i64(), Some(0));

    // The same build fails twice more and is latched; a latched build runs nothing. Each
    // attempt's raw output is kept beside the last one's.
    let _ = w.pass_with(false, None);
    let (_, tx) = w.pass_with(false, None);
    assert!(state.blocked_file("ay", 5).is_file(), "{}", tx.text());
    for attempt in 1..=3 {
        assert!(
            state
                .dir
                .join(format!("logs/ay-5/attempt-{attempt}/unit.stdout"))
                .is_file(),
            "attempt {attempt}"
        );
    }
    let asked = w.backend.asked().len();
    let (outcome, _) = w.pass();
    assert_eq!(row(&outcome, "ay").decision, Decision::Blocked);
    assert_eq!(w.backend.asked().len(), asked);
    // A newer main is a new build with its own count: it is tried by itself.
    fixture::commit(&w.fx.path("dev/ay"), "a fix", None);
    w.backend.on("test --locked", exit(0, &summary(50)));
    let (outcome, tx) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=6"], "{}", tx.text());
}

#[test]
fn a_failed_packed_control_or_a_refused_upload_leaves_nothing_queued() {
    let w = World::new("tip-packed-red");
    w.backend
        .on("--version", exit(1, "dyld: library not loaded\n"));
    let (r, _) = w.pass_with(false, None);
    let e = r.expect_err("the packed control failed");
    assert!(e.to_string().contains("packed version"), "{e}");
    assert_eq!(w.fx.packer.legs().len(), 1, "it was packed");
    assert_eq!(
        w.fx.staging_asset("ay", 5, "pkg-ay-5.toml"),
        None,
        "and never left this machine"
    );
    assert!(w.queued().is_empty());

    // Qualified, but the staging owner refuses the upload: still nothing is queued.
    w.backend.on("--version", exit(0, "ay 1.0.0\n"));
    w.backend.refuse_stage.store(true, Ordering::SeqCst);
    let (r, _) = w.pass_with(false, None);
    assert!(
        r.expect_err("the upload was refused")
            .to_string()
            .contains("staging refused it")
    );
    assert!(w.queued().is_empty());
    w.backend.refuse_stage.store(false, Ordering::SeqCst);
    let (outcome, _) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=5"]);
}

#[test]
fn a_host_that_does_not_answer_defers_the_candidate_and_counts_nothing_against_it() {
    let w = World::new("tip-deferred");
    let state = w.state();
    w.backend.staging_down.store(true, Ordering::SeqCst);
    // More passes than a failing build gets: none is an attempt, and nothing is latched.
    for _ in 0..=tip::BLOCK_AFTER {
        let (outcome, tx) = w.pass();
        match &row(&outcome, "ay").fate {
            Some(Fate::Deferred(why)) => {
                assert!(why.contains("the staging owner did not answer"), "{why}");
            }
            other => panic!("{other:?}\n{}", tx.text()),
        }
        assert!(outcome.enqueued.is_empty() && outcome.failed.is_empty());
        assert!(
            outcome.skipped.iter().any(|s| s.starts_with("ay=5: ")),
            "{:?}",
            outcome.skipped
        );
        assert!(!state.attempts_file("ay", 5).exists());
        assert!(!state.blocked_file("ay", 5).exists());
        assert!(w.queued().is_empty());
    }
    let receipt = std::fs::read_to_string(state.dir.join("receipts/ay-5.json")).expect("kept");
    let receipt: aterm_json::Value = aterm_json::from_str(&receipt).expect("JSON");
    assert_eq!(receipt["outcome"].as_str(), Some("deferred"));
    // The owner answers again: the same build is staged and queued.
    w.backend.staging_down.store(false, Ordering::SeqCst);
    assert_eq!(w.pass().0.enqueued, ["ay=5"]);
}

#[test]
fn what_a_source_step_leaves_in_the_tree_is_gone_before_the_build_reads_it() {
    let w = World::new("tip-stray");
    // A check that writes files no commit holds: a build would read each as its own.
    w.backend.hook("fmt --check", |run| {
        let cwd = run.cwd.as_ref().expect("the checkout");
        std::fs::create_dir_all(cwd.join(".cargo")).expect("mkdir");
        std::fs::write(cwd.join(".cargo/config.toml"), "[build]\n").expect("a config");
        std::fs::write(cwd.join("Cargo.lock"), "# generated\n").expect("a lock");
    });
    let (outcome, tx) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=5"], "{}", tx.text());
    let events = w.backend.events();
    assert!(events.iter().any(|e| e.starts_with("pack ")), "{events:?}");
    assert!(
        !events.iter().any(|e| e.starts_with("build saw")),
        "the pack built a tree that was not the frozen commit's alone: {events:?}"
    );
}

#[test]
fn a_repository_with_no_qualification_list_ships_nothing_and_is_not_a_failure() {
    let w = World::new("tip-unqualified");
    w.backend.recipe("ay", recipe("ay", None));
    let (outcome, tx) = w.pass();
    assert_eq!(row(&outcome, "ay").fate, Some(Fate::Unqualified));
    assert!(outcome.enqueued.is_empty() && outcome.failed.is_empty());
    assert!(
        outcome
            .skipped
            .iter()
            .any(|s| s.contains("declares no [workspace.metadata.atpkg] qualify")),
        "{:?}",
        outcome.skipped
    );
    assert!(tx.text().contains("nothing ships unqualified"));
    assert!(w.fx.packer.legs().is_empty() && w.backend.asked().is_empty());
    assert!(w.queued().is_empty());
    assert!(
        !w.state().attempts_file("ay", 5).exists(),
        "a missing list is the repository's standing, not a failed attempt"
    );
    // A malformed list is a defect of that commit: it fails, by what is wrong with it.
    w.backend.recipe("ay", recipe("ay", Some(&[FMT, BUILD])));
    let (r, _) = w.pass_with(false, None);
    let e = r.expect_err("a malformed list");
    assert!(e.to_string().contains(r#"carries no "packed" step"#), "{e}");

    // A commit none of whose manifests spells a list has not joined the lane: the pass learns
    // that from the commit's objects, and neither checks it out nor asks for its recipe.
    let dev = w.fx.path("dev/ay");
    std::fs::write(dev.join("Cargo.toml"), "[workspace]\n").expect("a plain manifest");
    git(&dev, &["add", "Cargo.toml"]);
    let plain = fixture::commit(&dev, "no list", None);
    let recipes = |w: &World| {
        w.backend
            .events()
            .iter()
            .filter(|e| *e == "metadata ay")
            .count()
    };
    let asked = recipes(&w);
    let (outcome, _) = w.pass();
    let ay = row(&outcome, "ay");
    assert_eq!(ay.frozen.as_ref().expect("frozen").sha, plain);
    assert_eq!(ay.fate, Some(Fate::Unqualified));
    assert_eq!(recipes(&w), asked, "no recipe was read");
    assert_ne!(
        git(&w.state().dir.join("progs/ay"), &["rev-parse", "HEAD"]),
        plain,
        "and the commit was never checked out"
    );
}

#[test]
fn a_pack_that_fails_or_source_a_step_changed_is_refused() {
    // The target does not build: the build step fails, and nothing is staged or queued.
    let w = World::new("tip-unbuilt");
    let (r, tx) = w.pass_with(false, Some(ARM));
    let e = r.expect_err("the pack failed");
    assert!(
        e.to_string()
            .contains(&format!("the pack failed for {ARM}")),
        "{e}\n{}",
        tx.text()
    );
    assert!(w.queued().is_empty());
    assert_eq!(w.fx.staging_asset("ay", 5, "pkg-ay-5.toml"), None);
    let receipt = std::fs::read_to_string(w.state().dir.join("receipts/ay-5.json")).expect("kept");
    let receipt: aterm_json::Value = aterm_json::from_str(&receipt).expect("JSON");
    assert_eq!(receipt["steps"][2]["stage"].as_str(), Some("build"));
    assert_eq!(receipt["steps"][2]["passed"].as_bool(), Some(false));

    // A step that rewrites tracked source: what was checked is not what would be built.
    let w = World::new("tip-changed");
    w.backend.hook("fmt --check", |run| {
        let cwd = run.cwd.as_ref().expect("the checkout");
        std::fs::write(cwd.join("Cargo.toml"), format!("{MANIFEST}# reformatted\n"))
            .expect("rewrite");
    });
    let (r, _) = w.pass_with(false, None);
    let e = r.expect_err("the tree changed");
    assert!(
        e.to_string().contains("a step changed tracked source"),
        "{e}"
    );
    assert!(
        w.fx.packer.legs().is_empty(),
        "the changed tree was never built"
    );
}

#[test]
fn a_dry_run_prints_each_programs_plan_and_writes_nothing() {
    let w = World::new("tip-dry");
    let home = w.fx.path("home");
    // The operator's own checkout of ay: its objects may be borrowed, and it is never written.
    git(
        &w.fx.path("dev"),
        &["clone", "-q", "ay", &home.join("ay").display().to_string()],
    );
    let before = (listing(&home), w.fx.host.changes());
    let (r, tx) = w.pass_with(true, None);
    let outcome = r.expect("a plan");
    assert_eq!(outcome.enqueued, ["ay=5"], "what a real pass would qualify");
    let text = tx.text();
    let frozen = &w.ay[4];
    for want in [
        format!(
            "[ay] PLAN — freeze {} main at {frozen} (build 5 > pin 3)",
            w.fx.ctx.staging_slug("ay")
        ),
        "[ay]   source fmt (within 120 s): targo fmt --check".to_string(),
        "[ay]   source unit (within 1800 s, at least 40 tests): targo --unverified test --locked"
            .into(),
        "[ay]   build  build (within 3600 s): the pack's build".into(),
        "[ay]   packed version (within 30 s): {bin}/ay --version".into(),
        format!(
            "[ay]   pack {ARM} signed with this machine's key, and run its packed controls here"
        ),
        format!("then enqueue ay-5.ready.toml for the {ARM} pin"),
        "REFUSED: ny is `on-hold`".into(),
        "DRY_RUN — would qualify, pack, stage and enqueue: ay=5".into(),
    ] {
        assert!(text.contains(&want), "{want:?} missing from\n{text}");
    }
    assert_eq!(row(&outcome, "ay").plan.len(), 7);
    // No step ran, nothing was packed, staged or queued, and no file under the lane's home or
    // on a host changed: the reads went to a scratch directory that is gone.
    assert!(w.backend.asked().is_empty() && w.fx.packer.legs().is_empty());
    assert_eq!((listing(&home), w.fx.host.changes()), before);
    assert!(!w.state().dir.exists());

    // After a real pass too: the dry run borrows the lane's objects and still writes nothing.
    w.backend.recipe("ay", recipe("ay", None));
    w.pass();
    let before = listing(&home);
    let (r, tx) = w.pass_with(true, None);
    r.expect("a plan");
    assert!(
        tx.text()
            .contains("NOT QUALIFIED — its repository declares no"),
        "{}",
        tx.text()
    );
    assert!(
        tx.text().contains(
            "would qualify, pack, stage and enqueue: <none>; ahead of their pin with no \
             qualification list to run: ay=5"
        ),
        "{}",
        tx.text()
    );
    assert_eq!(listing(&home), before);
}

#[test]
fn the_dispatcher_publishes_the_queue_in_one_index_and_moves_only_that_targets_pin() {
    let w = World::new("ready-publish");
    w.pass();
    let before = w.head().1.channels[0].clone();
    let (packs, runs) = (w.fx.packer.legs().len(), w.backend.asked().len());
    let (outcome, tx) = w.dispatch();
    assert_eq!(outcome.published, Some(6), "{}", tx.text());
    assert_eq!(outcome.ready, ["ay=5"]);
    assert!(!outcome.idle && outcome.held.is_empty() && outcome.deferred.is_empty());
    // The delta is one target pin and nothing else: no `pin` move at all.
    assert_eq!(
        w.indexer.deltas(),
        [(
            BTreeMap::new(),
            BTreeMap::from([(ARM, BTreeMap::from([("ay".to_string(), 5)]))])
        )]
    );
    // Apple silicon moved to ay 5. The channel's `pin` (what x86_64 and every other target
    // resolve) is byte for byte what it was: no other platform's selection moved.
    let (n, index) = w.head();
    let after = &index.channels[0];
    assert_eq!(n, 6);
    assert_eq!(after.pin, before.pin);
    assert_eq!(w.pins().1["ay"], 5);
    assert_eq!(w.pins_on(X86).1, before.for_target(X86.as_str()).pin);
    let mut expected = before.for_target(ARM.as_str()).pin;
    expected.insert("ay".into(), 5);
    assert_eq!(w.pins().1, expected);
    // It started no build and ran no step.
    assert_eq!(
        (w.fx.packer.legs().len(), w.backend.asked().len()),
        (packs, runs)
    );
    // The record left the queue and is kept as what became of it.
    assert!(w.queued().is_empty());
    assert!(
        w.state()
            .dir
            .join("retired/ay-5.ready.toml.published")
            .is_file()
    );
    // The next wake finds an empty queue: idle, with no host contact at all.
    let calls = w.fx.host.calls().len();
    let (outcome, _) = w.dispatch();
    assert!(outcome.idle && outcome.published.is_none());
    assert_eq!(w.fx.host.calls().len(), calls);
    assert_eq!(w.indexer.count(), 1);
}

#[test]
fn a_record_whose_staged_bytes_changed_is_set_aside_and_its_build_is_qualified_again() {
    let w = World::new("ready-restaged");
    w.pass();
    // Someone re-staged another signed manifest under the same build.
    let restage = |w: &World| {
        w.fx.stage_text(
            "ay",
            5,
            &format!(
                "{}\n# restaged by hand\n",
                fixture::manifest("ay", 5, &[ARM])
            ),
        );
    };
    restage(&w);
    let (outcome, tx) = w.dispatch();
    // Nothing is published, and the record does not stay to refuse every later wake: it is
    // kept as what became of it, and the queue is empty.
    assert_eq!((outcome.published, w.indexer.count()), (None, 0));
    assert!(outcome.idle, "{}", tx.text());
    assert!(
        outcome
            .retired
            .iter()
            .any(|r| r.contains("is not the manifest that was qualified")),
        "{:?}",
        outcome.retired
    );
    assert!(w.queued().is_empty());
    let state = w.state();
    assert!(state.dir.join("retired/ay-5.ready.toml.restaged").is_file());
    // With no record and no latch the tip lane qualifies the frozen build again, and what it
    // stages is what its new record binds: that one publishes.
    let (outcome, tx) = w.pass();
    assert_eq!(outcome.enqueued, ["ay=5"], "{}", tx.text());
    let (outcome, tx) = w.dispatch();
    assert_eq!(outcome.published, Some(6), "{}", tx.text());
    assert_eq!(w.pins().1["ay"], 5);

    // A build re-staged under the lane again and again is not rebuilt for ever: at the bound
    // it is latched, and the tip lane leaves it alone.
    let w = World::new("ready-restaged-latch");
    let state = w.state();
    for round in 1..=ready::SET_ASIDE_AFTER {
        assert!(!state.blocked_file("ay", 5).exists(), "round {round}");
        let (outcome, tx) = w.pass();
        assert_eq!(outcome.enqueued, ["ay=5"], "round {round}: {}", tx.text());
        restage(&w);
        w.dispatch();
        assert!(w.queued().is_empty(), "round {round}");
    }
    let latch = std::fs::read_to_string(state.blocked_file("ay", 5)).expect("latched");
    assert!(
        latch.contains("is not the manifest that was qualified"),
        "{latch}"
    );
    let packs = w.fx.packer.legs().len();
    let (outcome, _) = w.pass();
    assert_eq!(row(&outcome, "ay").decision, Decision::Blocked);
    assert_eq!(w.fx.packer.legs().len(), packs, "nothing was built again");
    assert_eq!(w.indexer.count(), 0);
}

/// An indexer the public owner refuses one program's release to: any index that pins
/// `refuses` is refused with the head where it was.
struct Picky<'a> {
    world: &'a World,
    refuses: &'static str,
    calls: AtomicUsize,
}

impl Indexer for Picky<'_> {
    fn publish(&self, ctx: &Ctx, request: &IndexRequest) -> Result<Published> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request
            .delta
            .target_pins
            .values()
            .any(|cells| cells.contains_key(self.refuses))
        {
            return Err(Error::refused(format!(
                "the public owner refuses {}'s release",
                self.refuses
            )));
        }
        self.world.indexer.publish(ctx, request)
    }
}

#[test]
fn a_record_the_publisher_refuses_does_not_hold_the_others_and_is_set_aside_at_the_bound() {
    let w = World::new("ready-isolate");
    let state = w.state();
    w.pass();
    let like = w.record("ay", 5).expect("ay's record");
    w.queue(&like, "nn", 4, 3);
    let picky = Picky {
        world: &w,
        refuses: "nn",
        calls: AtomicUsize::new(0),
    };
    let calls = || picky.calls.load(Ordering::SeqCst);
    // One index for both is refused. Each record is then published alone: ay is pinned, and
    // the refusal is counted against nn, which stays queued.
    let (r, tx) = w.dispatch_with(&picky, false, true);
    let outcome = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert_eq!(outcome.published, Some(6), "{}", tx.text());
    assert_eq!(outcome.ready, ["ay=5"]);
    assert!(
        outcome
            .held
            .iter()
            .any(|h| h.starts_with("nn=4: the publisher refused it")
                && h.contains("the public owner refuses nn's release")
                && h.contains("refusal 1 of 3")),
        "{:?}",
        outcome.held
    );
    let (n, pins) = w.pins();
    assert_eq!((n, pins["ay"], pins["nn"]), (6, 5, 3));
    assert_eq!(w.queued(), ["nn-4.ready.toml"]);
    assert_eq!(calls(), 3, "both, then ay alone, then nn alone");
    assert_eq!(state.publish_attempts("nn", 4), 1);
    assert_eq!(state.publish_attempts("ay", 5), 0);

    // From then on the refused record is published alone: a build queued later never rides
    // with it, and is pinned by the first publish of its pass.
    w.queue(&like, "ty", 10, 9);
    let (r, tx) = w.dispatch_with(&picky, false, true);
    let outcome = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert_eq!(outcome.published, Some(7), "{}", tx.text());
    assert_eq!(outcome.ready, ["ty=10"]);
    assert_eq!(calls(), 5, "ty alone, then nn alone");
    assert_eq!(w.queued(), ["nn-4.ready.toml"]);

    // The third refusal sets the record aside and latches its build: the queue is empty, the
    // latch says what was refused, and the next wake is idle.
    let (r, tx) = w.dispatch_with(&picky, false, true);
    let e = r.expect_err("nothing was published");
    assert!(
        e.to_string().contains("did not publish") && e.to_string().contains("set aside"),
        "{e}\n{}",
        tx.text()
    );
    assert!(w.queued().is_empty());
    assert!(state.dir.join("retired/nn-4.ready.toml.refused").is_file());
    let latch = std::fs::read_to_string(state.blocked_file("nn", 4)).expect("latched");
    assert!(
        latch.contains("the public owner refuses nn's release") && latch.contains("delete"),
        "{latch}"
    );
    let before = calls();
    let (r, _) = w.dispatch_with(&picky, false, true);
    assert!(r.expect("idle").idle);
    assert_eq!(calls(), before);

    // The tip lane does not qualify the latched build again when nn's main reaches it.
    fixture::commit(&w.fx.path("dev/nn"), "nn moves on", None);
    let (outcome, tx) = w.pass();
    assert_eq!(
        row(&outcome, "nn").decision,
        Decision::Blocked,
        "{}",
        tx.text()
    );
}

/// An indexer whose public owner does not answer.
struct Down;

impl Indexer for Down {
    fn publish(&self, _ctx: &Ctx, _request: &IndexRequest) -> Result<Published> {
        Err(Error::cannot_run("the public owner did not answer"))
    }
}

#[test]
fn a_publish_that_could_not_run_counts_nothing_against_its_records() {
    let w = World::new("ready-down");
    w.pass();
    w.queue(&w.record("ay", 5).expect("ay's record"), "nn", 4, 3);
    for _ in 0..=ready::SET_ASIDE_AFTER {
        let (r, _) = w.dispatch_with(&Down, false, true);
        let e = r.expect_err("the publish could not run");
        assert!(matches!(e, Error::CannotRun(_)), "{e:?}");
        assert!(e.to_string().contains("stay queued"), "{e}");
    }
    let state = w.state();
    assert_eq!(w.queued(), ["ay-5.ready.toml", "nn-4.ready.toml"]);
    assert_eq!(state.publish_attempts("ay", 5), 0);
    assert_eq!(state.publish_attempts("nn", 4), 0);
    assert!(!state.blocked_file("ay", 5).exists() && !state.blocked_file("nn", 4).exists());
    // The owner answers again: both are pinned by one index.
    let (outcome, tx) = w.dispatch();
    assert_eq!(outcome.published, Some(6), "{}", tx.text());
    assert_eq!(outcome.ready, ["ay=5", "nn=4"]);
    assert_eq!(w.indexer.count(), 1);
}

#[test]
fn a_build_the_head_withdrew_is_neither_pinned_nor_qualified_again() {
    let w = World::new("ready-withdrawn");
    w.pass();
    // An operator yanks the queued build before it is pinned.
    w.publish_yanking(6, "ay@5");
    let (outcome, tx) = w.dispatch();
    assert_eq!((outcome.published, w.indexer.count()), (None, 0));
    assert!(outcome.idle, "{}", tx.text());
    assert!(w.queued().is_empty());
    assert!(
        w.state()
            .dir
            .join("retired/ay-5.ready.toml.withdrawn")
            .is_file()
    );
    // The tip lane leaves the withdrawn build alone by name, and runs nothing for it.
    let (asked, packs) = (w.backend.asked().len(), w.fx.packer.legs().len());
    let (outcome, _) = w.pass();
    match &row(&outcome, "ay").decision {
        Decision::Refused(why) => assert!(why.contains("yanks ay@5"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        (w.backend.asked().len(), w.fx.packer.legs().len()),
        (asked, packs)
    );
    // A newer main is another build, and is qualified.
    fixture::commit(&w.fx.path("dev/ay"), "the fix", None);
    assert_eq!(w.pass().0.enqueued, ["ay=6"]);
}

#[test]
fn a_record_is_held_by_the_lifecycle_registry_as_it_stands_at_dispatch() {
    let w = World::new("ready-lifecycle");
    w.pass();
    // The owner puts ay on hold after its build was queued.
    w.registry(&format!(
        "{REGISTRY}\n[lifecycle.ay]\nstate = \"on-hold\"\n"
    ));
    let (r, _) = w.dispatch_with(&w.indexer, false, false);
    let outcome = r.expect("reported");
    assert!(
        outcome.held[0].contains("ay is `on-hold`"),
        "{:?}",
        outcome.held
    );
    assert_eq!((outcome.published, w.indexer.count()), (None, 0));
    // Lifted, it publishes.
    w.registry(REGISTRY);
    assert_eq!(w.dispatch().0.published, Some(6));
}

#[test]
fn a_record_for_another_target_is_held_on_this_machine() {
    let w = World::new("ready-other-target");
    w.pass();
    // The same qualified build, recorded as an x86_64 delivery: not this machine's to publish.
    let mine = w.record("ay", 5).expect("queued");
    std::fs::remove_file(w.state().ready_dir().join(mine.file_name())).expect("unqueue");
    let theirs = Record {
        target: X86.as_str().into(),
        targets: vec![X86.as_str().into()],
        ..mine
    };
    ready::enqueue(&w.state(), &theirs).expect("a record");
    let (r, _) = w.dispatch_with(&w.indexer, false, false);
    let outcome = r.expect("reported");
    assert!(
        outcome.held[0].contains(&format!(
            "the record is for {X86}, and this machine publishes {ARM} pins"
        )),
        "{:?}",
        outcome.held
    );
    assert_eq!((outcome.published, w.indexer.count()), (None, 0));
    assert_eq!(w.queued(), ["ay-5.ready.toml"]);
}

#[test]
fn a_record_the_head_already_covers_is_retired_and_nothing_is_published() {
    let w = World::new("ready-covered");
    w.pass();
    // Another publisher pinned a newer ay for this target meanwhile.
    w.publish_over(6, &[("ay", 6)]);
    let (outcome, tx) = w.dispatch();
    assert!(outcome.idle, "{}", tx.text());
    assert_eq!((outcome.published, w.indexer.count()), (None, 0));
    assert!(w.queued().is_empty());
    assert!(
        w.state()
            .dir
            .join("retired/ay-5.ready.toml.covered")
            .is_file()
    );
}

/// An indexer that loses its first race: another publisher's index lands first.
struct Raced<'a> {
    world: &'a World,
    calls: AtomicUsize,
    /// Whether the other publisher's index really landed (the head moved).
    lands: bool,
}

impl Indexer for Raced<'_> {
    fn publish(&self, ctx: &Ctx, request: &IndexRequest) -> Result<Published> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            if self.lands {
                self.world.publish_over(6, &[("nn", 4)]);
            }
            return Err(Error::refused("another publisher took index 6"));
        }
        self.world.indexer.publish(ctx, request)
    }
}

#[test]
fn a_lost_race_is_replanned_over_the_fresh_head_and_keeps_the_winners_pins() {
    let w = World::new("ready-race");
    w.pass();
    let raced = Raced {
        world: &w,
        calls: AtomicUsize::new(0),
        lands: true,
    };
    let (r, tx) = w.dispatch_with(&raced, false, true);
    let outcome = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert_eq!(outcome.published, Some(7), "{}", tx.text());
    assert_eq!(raced.calls.load(Ordering::SeqCst), 2);
    let (n, pins) = w.pins();
    assert_eq!(n, 7);
    assert_eq!(
        (pins["ay"], pins["nn"]),
        (5, 4),
        "the other publisher's pin is carried"
    );
    assert!(w.queued().is_empty());

    // A refusal with the head where it was is not a race: one attempt, and the record waits.
    let w = World::new("ready-refused");
    w.pass();
    let stuck = Raced {
        world: &w,
        calls: AtomicUsize::new(0),
        lands: false,
    };
    let (r, _) = w.dispatch_with(&stuck, false, true);
    let e = r.expect_err("the publish was refused");
    assert!(e.to_string().contains("did not publish"), "{e}");
    assert_eq!(stuck.calls.load(Ordering::SeqCst), 1);
    assert_eq!(w.queued(), ["ay-5.ready.toml"]);
    assert_eq!(w.pins().0, 5);
    assert_eq!(w.state().publish_attempts("ay", 5), 1, "and it is counted");
}

#[test]
fn a_ready_record_is_written_once_and_says_what_qualified_it() {
    let w = World::new("ready-immutable");
    w.pass();
    let state = w.state();
    let record = w.record("ay", 5).expect("queued");
    // The same record again is the same file; another qualification of that build is refused.
    let path = ready::enqueue(&state, &record).expect("idempotent");
    assert_eq!(path, state.ready_dir().join("ay-5.ready.toml"));
    let other = Record {
        manifest_sha256: "0".repeat(64),
        ..record.clone()
    };
    let e = ready::enqueue(&state, &other).expect_err("immutable");
    assert!(e.to_string().contains("a ready record is immutable"), "{e}");
    assert_eq!(w.record("ay", 5), Some(record.clone()));
    // What a record must say to be one.
    for (broken, want) in [
        (
            Record {
                source: "abc".into(),
                ..record.clone()
            },
            "not a full lowercase commit",
        ),
        (
            Record {
                build: 3,
                ..record.clone()
            },
            "not above the pin",
        ),
        (
            Record {
                steps: Vec::new(),
                ..record.clone()
            },
            "carries no build step",
        ),
        (
            Record {
                schema: "v0".into(),
                ..record.clone()
            },
            "schema",
        ),
        (
            Record {
                targets: vec!["mips".into()],
                ..record.clone()
            },
            "names no client target",
        ),
        (
            Record {
                target: X86.as_str().into(),
                ..record.clone()
            },
            "carries no x86_64-apple-darwin row",
        ),
    ] {
        let e = ready::enqueue(&state, &broken).expect_err("not a record");
        assert!(e.to_string().contains(want), "{e}");
    }
    // A file that is not a record is listed as what it is, never read as one.
    std::fs::write(
        state.ready_dir().join("nn-9.ready.toml"),
        "program = \"nn\"\n",
    )
    .expect("stray");
    let queue = ready::pending(&state).expect("a readable queue");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0].record.as_ref().ok(), Some(&record));
    let why = queue[1].record.as_ref().expect_err("not a record");
    assert!(why.contains("does not parse"), "{why}");
}

#[test]
fn a_file_that_is_not_a_record_is_set_aside_and_publishes_nothing() {
    let w = World::new("ready-stray");
    w.pass();
    let state = w.state();
    std::fs::write(
        state.ready_dir().join("nn-9.ready.toml"),
        "program = \"nn\"\n",
    )
    .expect("stray");
    let (outcome, tx) = w.dispatch();
    assert_eq!(outcome.ready, ["ay=5"], "{}", tx.text());
    assert_eq!(w.indexer.count(), 1);
    assert_eq!(w.pins().1["ay"], 5);
    assert!(state.dir.join("retired/nn-9.ready.toml.invalid").is_file());
    assert!(w.queued().is_empty());
}

#[test]
fn a_dispatch_dry_run_says_what_it_would_pin_and_changes_nothing() {
    let w = World::new("ready-dry");
    w.pass();
    let home = w.fx.path("home");
    let before = (listing(&home), w.fx.host.changes());
    let (r, tx) = w.dispatch_with(&w.indexer, true, false);
    let outcome = r.expect("a plan");
    assert_eq!(
        (outcome.ready.as_slice(), outcome.published),
        (&["ay=5".to_string()][..], None)
    );
    assert!(
        tx.text().contains(&format!(
            "DRY_RUN — would publish atpkg-index-6 pinning for {ARM}: ay=5"
        )),
        "{}",
        tx.text()
    );
    assert_eq!(w.indexer.count(), 0);
    assert_eq!((listing(&home), w.fx.host.changes()), before);
}

#[test]
fn the_staged_only_flags_belong_to_the_linux_and_tip_lanes() {
    assert_eq!(LaneKind::parse("tip").expect("a lane"), LaneKind::Tip);
    assert_eq!(LaneKind::Tip.as_str(), "tip");
    let flags = RunFlags::parse(
        LaneKind::Tip,
        &["--staged-only".into(), "--queued".into(), "--json".into()],
    )
    .expect("the dispatcher's words");
    assert!(flags.staged_only && flags.queued && flags.json && !flags.dry_run);
    assert!(
        RunFlags::parse(LaneKind::Tip, &["--dry-run".into()])
            .expect("a plan")
            .dry_run
    );
    assert!(RunFlags::parse(LaneKind::Tip, &["--queued".into()]).is_err());
    assert!(RunFlags::parse(LaneKind::Tip, &["--triple".into(), ARM.as_str().into()]).is_err());
}

fn spec(root: &Path, staged_only: bool) -> UnitSpec {
    UnitSpec {
        kind: LaneKind::Tip,
        binary: root.join("state/aterm-auto/lane.20261112T000000Z/atpkg-publish"),
        sha256: "cd".repeat(32),
        home: root.join("home"),
        state_dir: root.join("state/aterm-auto"),
        xdg_config_home: root.join("home/.config"),
        shim_bin: root.join("home/Library/Application Support/aterm/pkg/bin"),
        host: atpkg_publish::ctx::HostChoice::Gh,
        staged_only,
    }
}

#[test]
fn the_tip_agent_is_a_short_interval_pass_of_the_snapshot() {
    let root = Path::new("/Users//o");
    let alone = units::render(&spec(root, false));
    assert_eq!(alone.len(), 1, "no dispatcher unless asked for");
    assert_eq!(alone[0].name, "systems.alab.aterm-auto-tip.plist");
    let pass = &alone[0].text;
    for want in [
        "<string>/Users//o/state/aterm-auto/lane.20261112T000000Z/atpkg-publish</string>",
        "<string>run</string>\n\t\t<string>tip</string>\n\t\t<string>--sha256</string>",
        &format!("<string>{}</string>", "cd".repeat(32)),
        "<string>/Users//o/state/aterm-auto/tip.log</string>",
        "<key>StartInterval</key>\n\t<integer>900</integer>",
        "<key>RunAtLoad</key>\n\t<true/>",
        "<string>/Users//o/state/aterm-auto/tip.launchd.log</string>",
    ] {
        assert!(pass.contains(want), "{want:?} missing from\n{pass}");
    }
    assert!(
        !pass.contains("--staged-only") && !pass.contains("WatchPaths"),
        "{pass}"
    );
    // Its steps are held to bounds the product repositories committed: it is not run in the
    // class macOS throttles.
    assert!(!pass.contains("ProcessType"), "{pass}");
}

#[test]
fn the_ready_dispatcher_agent_is_woken_by_the_ready_directory() {
    let root = Path::new("/Users//o");
    let both = units::render(&spec(root, true));
    let names: Vec<&str> = both.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "systems.alab.aterm-auto-tip.plist",
            "systems.alab.aterm-auto-tip-ready.plist"
        ]
    );
    // The pass is the same job with or without its dispatcher.
    assert_eq!(both[0].text, units::render(&spec(root, false))[0].text);
    let ready = &both[1].text;
    for want in [
        "<string>run</string>\n\t\t<string>tip</string>\n\t\t<string>--sha256</string>",
        "<string>--staged-only</string>\n\t\t<string>--queued</string>\n\t\t<string>--json</string>",
        "<key>WatchPaths</key>\n\t<array>\n\t\t<string>/Users//o/state/aterm-auto/publish-ready</string>\n\t</array>",
        "<key>StartInterval</key>\n\t<integer>3600</integer>",
        "<string>/Users//o/state/aterm-auto/tip-ready.log</string>",
        "<string>/Users//o/state/aterm-auto/tip-ready.launchd.log</string>",
        &format!("<string>{}</string>", "cd".repeat(32)),
    ] {
        assert!(ready.contains(want), "{want:?} missing from\n{ready}");
    }
    assert!(
        ready.contains("<key>ProcessType</key>\n\t<string>Background</string>"),
        "{ready}"
    );
    // The dispatcher watches the directory the lane's records are written to.
    assert_eq!(
        LaneState::of(LaneKind::Tip, Path::new("/Users//o/state/aterm-auto")).ready_dir(),
        Path::new("/Users//o/state/aterm-auto/publish-ready")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_rendered_tip_agents_lint_with_plutil() {
    use atpkg_publish::lane::install::{self, Lint};
    let fx = Fx::new("tip-plutil");
    let paths: Vec<PathBuf> = units::render(&spec(&fx.path("r&d <x>"), true))
        .into_iter()
        .map(|unit| {
            let p = fx.path(&unit.name);
            std::fs::write(&p, &unit.text).expect("write");
            p
        })
        .collect();
    assert!(!paths.is_empty());
    assert_eq!(install::lint_plists(&paths).expect("lints"), Lint::Passed);
}

#[test]
fn installing_the_tip_lane_arms_its_pass_from_the_one_snapshot() {
    use atpkg_publish::triple::Os;
    let b = super::install::machine("tip-install-pass", Os::Darwin);
    let (r, tx) = super::install::install(&b, LaneKind::Tip, InstallFlags::default());
    let done = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    assert_eq!(done.armed.len(), 1);
    assert_eq!(
        done.armed[0].file_name().expect("a name").to_string_lossy(),
        format!("{}.plist", units::TIP_LABEL)
    );
    let text = std::fs::read_to_string(&done.armed[0]).expect("an armed unit");
    assert!(text.contains(&done.binary.display().to_string()), "{text}");
    assert!(text.contains(&done.sha256), "{text}");
    assert!(
        b.rec.calls().iter().any(|c| c.starts_with("arm tip ")),
        "{:?}",
        b.rec.calls()
    );
    assert!(tx.text().contains("a pass every 15 min"), "{}", tx.text());

    // The tip lane packs Apple rows: it does not install on Linux.
    let linux = super::install::machine("tip-install-linux", Os::Linux);
    let (r, _) = super::install::install(&linux, LaneKind::Tip, InstallFlags::default());
    assert!(
        r.expect_err("not macOS")
            .to_string()
            .contains("the tip lane runs on macOS")
    );
}

#[test]
fn installing_with_staged_only_also_arms_the_ready_dispatcher() {
    use atpkg_publish::triple::Os;
    let b = super::install::machine("tip-install", Os::Darwin);
    let staged = InstallFlags {
        staged_only: true,
        ..InstallFlags::default()
    };
    let (r, tx) = super::install::install(&b, LaneKind::Tip, staged);
    let done = r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    let names: Vec<String> = done
        .armed
        .iter()
        .map(|p| {
            p.file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        [
            format!("{}.plist", units::TIP_LABEL),
            format!("{}.plist", units::TIP_READY_LABEL)
        ]
    );
    // Both jobs run the one snapshot, by its hash.
    for unit in &done.armed {
        let text = std::fs::read_to_string(unit).expect("an armed unit");
        assert!(text.contains(&done.binary.display().to_string()), "{text}");
        assert!(text.contains(&done.sha256), "{text}");
    }
    // The directory the dispatcher watches exists before anything is queued.
    assert!(
        LaneState::of(LaneKind::Tip, &b.fx.ctx.state_dir)
            .ready_dir()
            .is_dir()
    );
    assert!(tx.text().contains(units::TIP_READY_LABEL), "{}", tx.text());

    // Without --staged-only the install says the queue waits for a pass someone starts.
    let alone = super::install::machine("tip-install-alone", Os::Darwin);
    let (r, tx) = super::install::install(&alone, LaneKind::Tip, InstallFlags::default());
    assert_eq!(r.expect("installed").armed.len(), 1);
    assert!(
        tx.text().contains("no ready dispatcher armed"),
        "{}",
        tx.text()
    );

    // The Apple source-release lane has no staged-only mode.
    let (r, _) = super::install::install(&alone, LaneKind::Alab, staged);
    assert!(matches!(r, Err(Error::Usage(_))));
}

#[test]
fn arming_one_apple_lane_waits_for_the_other_to_be_idle() {
    use atpkg_publish::lane::state::Lock;
    use atpkg_publish::triple::Os;
    let b = super::install::machine("tip-install-busy", Os::Darwin);
    let root = &b.fx.ctx.state_dir;
    // Arming tip retires the alab job: a running alab pass would be killed where it stands.
    let alab = LaneState::of(LaneKind::Alab, root);
    let Lock::Held(pass) = alab.try_lock().expect("the alab lock") else {
        panic!("the alab lock is free");
    };
    let (r, _) = super::install::install(&b, LaneKind::Tip, InstallFlags::default());
    let e = r.expect_err("an alab run is in flight");
    assert!(e.to_string().contains("a alab run is in flight"), "{e}");
    assert!(
        !b.rec.calls().iter().any(|c| c.starts_with("arm ")),
        "{:?}",
        b.rec.calls()
    );
    drop(pass);
    // And arming alab would kill a tip dispatch in the middle of its index.
    let tip = LaneState::of(LaneKind::Tip, root);
    let Lock::Held(dispatch) = tip.try_publish_lock().expect("the publish lock") else {
        panic!("the publish lock is free");
    };
    let (r, _) = super::install::install(&b, LaneKind::Alab, InstallFlags::default());
    let e = r.expect_err("a tip dispatch is in flight");
    assert!(e.to_string().contains("a tip run is in flight"), "{e}");
    drop(dispatch);
    // --no-arm retires nothing, so it waits for no one.
    let Lock::Held(pass) = alab.try_lock().expect("the alab lock") else {
        panic!("the alab lock is free");
    };
    let unarmed = InstallFlags {
        no_arm: true,
        ..InstallFlags::default()
    };
    let (r, tx) = super::install::install(&b, LaneKind::Tip, unarmed);
    r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
    drop(pass);
    let (r, tx) = super::install::install(&b, LaneKind::Tip, InstallFlags::default());
    r.unwrap_or_else(|e| panic!("{e}\n{}", tx.text()));
}
