// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! AghGitGrant's conformance: the helper's decision (`answer`) and the grant files, bound to
//! the model step by step.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};

use aterm_models_agh::agh_git_grant_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::{self, StepEvidence};

use super::GRANT_MAX;
use super::credential::{Asked, CredentialAnswer, CredentialRequest, answer, project_answer};
use super::grant::{self, Grant, MapGrants, project_grant_file};
use crate::config::{self, Dirs};
use crate::identity::{self, MapEnv};
use crate::policy::{Presence, Router};
use crate::slug::Slug;
use crate::test_fixtures::{canonical_text, setup_file, write_config};

const MACHINE: &str = "AghGitGrant";

const LAWS: [&str; 9] = [
    "PathlessGetsNothing",
    "GithubOnly",
    "NoTerminalDoor",
    "StoreEraseInert",
    "ReleaseOwnerOnlyByGrant",
    "ReleaseStaysHome",
    "GrantLive",
    "GrantPidBound",
    "GrantSlugBound",
];

const MUTANTS: [(&str, &str); 9] = [
    ("BuggyPathless", "PathlessGetsNothing"),
    ("BuggyForeignHost", "GithubOnly"),
    ("BuggyTtyAnswers", "NoTerminalDoor"),
    ("BuggyEraseAnswers", "StoreEraseInert"),
    ("BuggyReleaseByRead", "ReleaseOwnerOnlyByGrant"),
    ("BuggyStaleGrant", "GrantLive"),
    ("BuggyOtherPid", "GrantPidBound"),
    ("BuggyWrongSlug", "GrantSlugBound"),
    ("BuggyReleaseForDev", "ReleaseStaysHome"),
];

const WORLD: [&str; 10] = [
    "NextTier",
    "TogglePath",
    "ToggleHost",
    "ToggleTty",
    "NextOp",
    "ToggleSlug",
    "ToggleAsker",
    "ChildExits",
    "SpawnerKilled",
    "Reset",
];

/// The walk's clock: the helper's `now`, and the clock every grant file is written at.
const NOW: u64 = 1_760_000_000;

fn var(state: &State, name: &str) -> i64 {
    state[name]
}

/// The nonce the model's one grant has: 32 lowercase hex, built at runtime.
fn nonce() -> String {
    "a5".repeat(16)
}

fn slug(text: &str) -> Slug {
    Slug::parse(text).expect("a slug")
}

/// The request's repository: the model's `tier` 0 is the dev owner, 1 the release owner.
fn slug_text(tier: i64) -> &'static str {
    if tier == 0 {
        "fixture-dev/app"
    } else {
        "alabsystems/aterm"
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("agh-git-grant-")
        .tempdir()
        .expect("a scratch root")
}

/// The canonical agh.toml (an in-crate copy of the fixture's text), with both token files
/// present, so presence is never the variable.
struct World {
    _root: aterm_tempfile::TempDir,
    router: Router,
}

fn world() -> World {
    let root = scratch();
    let base = root.path();
    let dirs = Dirs::under(base);
    write_config(&dirs, &canonical_text(base));
    for (dir, name, seed) in [
        ("tokens", "dev.token", "dev"),
        ("secrets", "release.token", "release"),
    ] {
        let dir = base.join(dir);
        aterm_uds::private_file::create_private_dir(&dir).expect("a token directory");
        setup_file(
            &dir.join(name),
            format!("{}\n", agh_fake::token(seed)).as_bytes(),
        );
    }
    let validated = config::load(&dirs)
        .expect("the canonical agh.toml loads")
        .expect("the canonical agh.toml is there");
    World {
        _root: root,
        router: Router::from_validated(validated),
    }
}

/// What git sent in `state`.
fn request(state: &State) -> CredentialRequest {
    let op = match var(state, "op") {
        0 => "get",
        1 => "store",
        _ => "erase",
    };
    let host = if var(state, "host") == 1 {
        "github.com"
    } else {
        "example.com"
    };
    CredentialRequest {
        op: op.to_string(),
        protocol: Some("https".to_string()),
        host: Some(host.to_string()),
        path: (var(state, "path") == 1).then(|| format!("{}.git", slug_text(var(state, "tier")))),
    }
}

/// The model's grant as a `MapGrants` (plan §11, the conformance's step 3).
fn grants(state: &State) -> MapGrants {
    let model = var(state, "grant");
    if model == 0 {
        return MapGrants::new();
    }
    let slug = if var(state, "slugm") == 1 {
        slug(slug_text(var(state, "tier")))
    } else {
        slug("alabsystems/other")
    };
    let live = model == 1;
    let in_tree = live && var(state, "child") == 1 && var(state, "asker") == 0;
    let grant = Grant {
        nonce: nonce(),
        slug,
        profile: "release".to_string(),
        pid: std::process::id(),
        start: 1,
        expires_at: if live { NOW + 600 } else { NOW - 1 },
    };
    MapGrants::new().with(grant, in_tree)
}

fn successor(model: &Model, state: &State, action: &str) -> Option<State> {
    let mut next = model.successors(action, state);
    assert!(next.len() <= 1, "{action} from {state:?}: {next:?}");
    next.pop()
}

fn with_answer(state: &State, (phase, used): (i64, i64)) -> State {
    let mut next = state.clone();
    next.insert("phase", phase);
    next.insert("used", used);
    next
}

/// A fresh state root whose state directory, and the directory above it, do not exist yet.
fn fresh_dirs() -> (aterm_tempfile::TempDir, Dirs) {
    let root = scratch();
    let dirs = Dirs::under(&root.path().join("fresh"));
    (root, dirs)
}

#[test]
fn the_helper_and_the_grant_files_conform_to_the_git_grant_model() {
    std::hint::black_box(super::grant::git_grant_environment as fn());
    let model = agh_git_grant_model();
    let buggy = interp::with_buggy(&model, 1);
    let states = interp::reachable_states(&model);
    let world = world();
    let env = MapEnv::new();
    let presence = |name: &str| {
        world
            .router
            .profile(name)
            .map_or(Presence::Absent, |row| identity::presence(row, &env))
    };
    let mut ev = StepEvidence::new(MACHINE);
    let mut real: BTreeMap<State, (i64, i64)> = BTreeMap::new();

    for state in states.iter().filter(|s| var(s, "phase") == 0) {
        let expected = successor(&model, state, "Answer").expect("Answer fires in phase 0");
        let req = request(state);
        let nonce = nonce();
        let asked = Asked {
            stdin_is_terminal: var(state, "tty") == 1,
            nonce: (var(state, "grant") > 0).then_some(nonce.as_str()),
            loopback: None,
            now: NOW,
        };
        let grants = grants(state);
        let answered = ev.step("Answer", || {
            answer(&req, &asked, &world.router, &grants, &presence)
        });
        let projection = project_answer(&answered);
        assert_eq!(
            (expected["phase"], expected["used"]),
            projection,
            "{state:?}: {answered:?}"
        );
        if let CredentialAnswer::Credential { profile, tier } = &answered {
            let row = world.router.profile(profile).expect("a profile it holds");
            assert_eq!(*tier, row.tier, "{state:?}: {answered:?}");
            if var(state, "tier") == 0 {
                assert_eq!(profile, "dev", "{state:?}: {answered:?}");
            }
        }
        real.insert(state.clone(), projection);
    }

    let mut pairs = BTreeSet::new();
    for state in states.iter().filter(|s| var(s, "phase") == 0) {
        for (mutant, law) in MUTANTS {
            for wrong in buggy.successors(mutant, state) {
                let lawful = with_answer(state, real[state]);
                assert_ne!(wrong, lawful, "{mutant} at {state:?}");
                assert!(
                    model.check_invariant(law, &lawful),
                    "{law} fails on the real step at {state:?}"
                );
                assert!(
                    !buggy.check_invariant(law, &wrong),
                    "{mutant} keeps {law} at {state:?}"
                );
                pairs.insert(mutant);
            }
        }
    }
    assert_eq!(
        pairs,
        MUTANTS.iter().map(|(mutant, _)| *mutant).collect(),
        "a mutant the conformance never replays"
    );

    // The file actions take the slug and the clock alone: the world's other toggles never reach
    // them, so each runs once per (grant, child, tier) the model reaches.
    let later = NOW + GRANT_MAX.as_secs() + 1;
    let mut seen = BTreeSet::new();
    for state in &states {
        let key = (var(state, "grant"), var(state, "child"), var(state, "tier"));
        let repo = slug(slug_text(var(state, "tier")));
        for action in ["Spawn", "Consume", "Expire"] {
            let Some(next) = successor(&model, state, action) else {
                continue;
            };
            if !seen.insert((action, key)) {
                continue;
            }
            let (_root, dirs) = fresh_dirs();
            match action {
                "Spawn" => {
                    let made = ev
                        .step("Spawn", || {
                            grant::write(&dirs, &repo, "release", NOW, GRANT_MAX)
                        })
                        .expect("a grant is written");
                    assert_eq!(
                        project_grant_file(&dirs, &made.nonce, NOW),
                        next["grant"],
                        "Spawn at {state:?}"
                    );
                }
                "Consume" => {
                    let made = grant::write(&dirs, &repo, "release", NOW, GRANT_MAX)
                        .expect("a grant is written");
                    assert_eq!(project_grant_file(&dirs, &made.nonce, NOW), 1);
                    ev.step("Consume", || grant::remove(&dirs, &made));
                    assert_eq!(
                        project_grant_file(&dirs, &made.nonce, NOW),
                        next["grant"],
                        "Consume at {state:?}"
                    );
                }
                _ => {
                    let made = grant::write(&dirs, &repo, "release", NOW, GRANT_MAX)
                        .expect("a grant is written");
                    // NEGATIVE CONTROL: a sweep at the walk's clock leaves the live grant alone.
                    grant::sweep(&dirs, NOW);
                    assert_eq!(project_grant_file(&dirs, &made.nonce, NOW), 1);
                    assert_eq!(
                        project_grant_file(&dirs, &made.nonce, later),
                        var(state, "grant"),
                        "SpawnerKilled moved the clock past GRANT_MAX at {state:?}"
                    );
                    ev.step("Expire", || grant::sweep(&dirs, later));
                    assert_eq!(
                        project_grant_file(&dirs, &made.nonce, later),
                        next["grant"],
                        "Expire at {state:?}"
                    );
                }
            }
        }
    }
    assert_eq!(
        seen.len(),
        6,
        "Spawn, Consume and Expire on both tiers: {seen:?}"
    );

    assert_eq!((LAWS.len(), MUTANTS.len(), WORLD.len()), (9, 9, 10));
    let audit = ev.audit();
    assert!(audit.is_complete(), "{audit:?}");
    let anchored: BTreeSet<&str> = ["Answer", "Consume", "Expire", "Spawn"].into();
    assert_eq!(audit.actions, anchored);
    let bound: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == MACHINE)
        .map(|r| r.action)
        .collect();
    assert_eq!(bound, anchored);
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == MACHINE)
        .map(|w| w.action)
        .collect();
    let expected: BTreeSet<&str> = WORLD
        .into_iter()
        .chain(MUTANTS.iter().map(|(mutant, _)| *mutant))
        .collect();
    assert_eq!(waived, expected);
    assert_eq!(waived.len(), 19);
    let every: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(
        bound.union(&waived).copied().collect::<BTreeSet<_>>(),
        every
    );
    for law in LAWS {
        assert!(
            model.invariants.iter().any(|inv| inv.name == law),
            "{law} is no invariant of the model"
        );
    }
}

/// `grant::write` on a state directory nothing has made yet: its parents are made 0700 and the
/// grant file 0600.
#[test]
fn a_grant_is_written_into_a_fresh_state_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_root, dirs) = fresh_dirs();
    assert!(!dirs.state.exists());
    assert!(dirs.state.parent().is_some_and(|p| !p.exists()));
    let made = grant::write(&dirs, &slug("alabsystems/aterm"), "release", NOW, GRANT_MAX)
        .expect("a grant on a fresh state directory");
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("it exists")
            .permissions()
            .mode()
            & 0o777
    };
    let file = dirs.grants().join(format!("{}.json", made.nonce));
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dirs.state), 0o700);
    assert_eq!(mode(&dirs.grants()), 0o700);
    assert_eq!(made.expires_at, NOW + GRANT_MAX.as_secs());
    assert_eq!(made.nonce.len(), 32);
}

/// A grant is published whole under its name: no staging file is left, and the file has one
/// link. A staging file a concurrent writer has not yet linked (`.{nonce}.tmp`, empty here, as
/// it is between its `create_new` and its write) is no grant to `sweep`, which keeps it while
/// it is young and removes it once its mtime is more than `GRANT_MAX` old.
#[test]
fn a_grant_is_published_whole_and_a_stale_staging_file_is_swept() {
    use std::os::unix::fs::MetadataExt as _;
    let (_root, dirs) = fresh_dirs();
    let made = grant::write(&dirs, &slug("alabsystems/aterm"), "release", NOW, GRANT_MAX)
        .expect("a grant");
    let names = || -> BTreeSet<String> {
        std::fs::read_dir(dirs.grants())
            .expect("the grants directory")
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect()
    };
    let published = format!("{}.json", made.nonce);
    assert_eq!(names(), BTreeSet::from([published.clone()]));
    let file = dirs.grants().join(&published);
    assert_eq!(std::fs::metadata(&file).expect("the grant").nlink(), 1);
    let other = aterm_uds::rand::hex_token::<16>().expect("a nonce");
    let staging = format!(".{other}.tmp");
    std::fs::File::create(dirs.grants().join(&staging)).expect("a staging file");
    let staged_at = std::fs::metadata(dirs.grants().join(&staging))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .expect("its mtime")
        .as_secs();
    grant::sweep(&dirs, staged_at);
    assert!(names().contains(&staging), "a young staging file was swept");
    grant::sweep(&dirs, staged_at + GRANT_MAX.as_secs() + 1);
    assert!(!names().contains(&staging), "a stale staging file was kept");
}
