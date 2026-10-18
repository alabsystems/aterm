// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TIER-1 CONFORMANCE, the WRITER half of the derived model `AtpkgIndexPublishWalk`
//! (crates/aterm-models-atpkg/src/models_atpkg_index_publish.rs). The reader half — the
//! client's tag walk — stays in atpkg (`src/net/index_publish_conformance.rs`).
//!
//! Every decision a publisher holding a baseline faces in the model (its baseline number,
//! the number it aims at, the channel's tags) is laid out on a fake channel, and the REAL
//! indexer (`channel::index::publish_with`) publishes over it. Its outcome must be the
//! model's one enabled decision — `Create`, `Converge` or `Refuse` — and leave the tags as
//! the model does.
//!
//! NEGATIVE CONTROLS: the `Buggy = 1` decisions (a baseline counted off a draft, a number
//! past the baseline's successor, a converge onto another publisher's bytes) are driven
//! too, and the real indexer refuses each one the mutant takes. The writer cannot be TOLD
//! a number past the baseline's successor (it derives N+1); the only way to name one is a
//! channel's first publish, which a live channel refuses.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use aterm_models_atpkg::atpkg_index_publish_walk_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use atpkg_publish::api::{Baseline, IndexRequest};
use atpkg_publish::channel::index::{parse_delta, publish_with};
use atpkg_publish::host::fake::{FakeHost, FakeRelease};
use atpkg_publish::host::{Hosts, Slug};
use atpkg_publish::quad::{QUAD_FILES, index_tag};

use super::Rig;

const OWN_VALID_UNTIL: &str = "2098-01-01T00:00:00Z";
const FOREIGN_VALID_UNTIL: &str = "2098-01-02T00:00:00Z";

/// Every state `model` reaches, breadth-first over its actions.
fn reachable(model: &Model) -> Vec<State> {
    let key = |s: &State| s.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>();
    let init = model.init_state();
    let mut seen = BTreeSet::from([key(&init)]);
    let mut queue = VecDeque::from([init]);
    let mut out = Vec::new();
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if seen.insert(key(&next)) {
                    queue.push_back(next);
                }
            }
        }
        out.push(state);
    }
    out
}

/// The codes of atpkg-index-2..=4: 0 absent, 1/3 a draft, 2/4 published; 1/2 publisher
/// A's bytes, 3/4 another publisher's.
fn channel(state: &State) -> [i64; 3] {
    [state["t2"], state["t3"], state["t4"]]
}

fn published(tags: [i64; 3], build: u64) -> bool {
    match build {
        1 => true,
        2..=4 => matches!(tags[usize::try_from(build).expect("small") - 2], 2 | 4),
        _ => false,
    }
}

struct Fixture {
    rig: Rig,
    /// Publisher A's quad for each index number 1..=4, each built on A's previous one.
    own: BTreeMap<u64, PathBuf>,
    /// Another publisher's quad for 2..=4, built on the same baselines (other bytes).
    foreign: BTreeMap<u64, PathBuf>,
}

fn valid_until(v: &str) -> atpkg_publish::api::Delta {
    parse_delta(&["--valid-until".to_string(), v.to_string()]).expect("delta")
}

impl Fixture {
    fn new() -> Self {
        let rig = Rig::new("index-conformance");
        // The genesis has no head to ship a roster beside: the machine's installed pair.
        let installed = &rig.ctx.identity.roster;
        std::fs::create_dir_all(installed.parent().expect("dir")).expect("mkdir");
        std::fs::write(installed, &rig.keys.roster_bytes).expect("roster");
        std::fs::write(format!("{}.sig", installed.display()), &rig.keys.roster_sig).expect("sig");
        let dry = |name: &str, baseline: Baseline, until: &str| {
            let out = rig.path(name);
            let req = IndexRequest {
                baseline,
                delta: valid_until(until),
                out: out.clone(),
                upload: false,
                pack_dir: None,
            };
            let mut log = Rig::log();
            publish_with(&rig.ctx, &req, &mut log)
                .unwrap_or_else(|e| panic!("fixture {name}: {e}\n{}", log.text()));
            out
        };
        let mut own = BTreeMap::new();
        let mut foreign = BTreeMap::new();
        own.insert(
            1,
            dry(
                "own-1",
                Baseline::First {
                    channel: "stable".into(),
                    index_build: 1,
                },
                OWN_VALID_UNTIL,
            ),
        );
        for n in 2..=4u64 {
            let base = own[&(n - 1)].clone();
            foreign.insert(
                n,
                dry(
                    &format!("foreign-{n}"),
                    Baseline::Dir(base.clone()),
                    FOREIGN_VALID_UNTIL,
                ),
            );
            own.insert(
                n,
                dry(&format!("own-{n}"), Baseline::Dir(base), OWN_VALID_UNTIL),
            );
        }
        Self { rig, own, foreign }
    }

    fn slug(&self) -> Slug {
        self.rig.ctx.index_slug()
    }

    /// A fresh channel: atpkg-index-1 published, 2..=4 as `tags` codes.
    fn lay(&self, tags: [i64; 3]) -> Arc<FakeHost> {
        let host = Arc::new(FakeHost::new());
        for build in 1..=4u64 {
            let code = if build == 1 {
                2
            } else {
                tags[usize::try_from(build).expect("small") - 2]
            };
            let source = match code {
                0 => continue,
                1 | 2 => &self.own[&build],
                _ => &self.foreign[&build],
            };
            let assets = QUAD_FILES
                .iter()
                .map(|f| {
                    (
                        (*f).to_string(),
                        std::fs::read(source.join(f)).expect("quad"),
                    )
                })
                .collect();
            host.put(
                &self.slug(),
                &index_tag(build),
                FakeRelease {
                    draft: matches!(code, 1 | 3),
                    prerelease: true,
                    assets,
                },
            );
        }
        host
    }

    fn code_at(&self, host: &FakeHost, build: u64) -> i64 {
        let Some(rel) = host.release_of(&self.slug(), &index_tag(build)) else {
            return 0;
        };
        let Some(index) = rel.assets.get(QUAD_FILES[0]) else {
            return 0;
        };
        let mine = *index == std::fs::read(self.own[&build].join(QUAD_FILES[0])).expect("own");
        match (mine, rel.draft) {
            (true, true) => 1,
            (true, false) => 2,
            (false, true) => 3,
            (false, false) => 4,
        }
    }

    /// Publisher A, holding baseline `ba`, aims at `na` over the channel `tags`.
    fn publish(&self, ba: i64, na: i64, tags: [i64; 3]) -> (bool, [i64; 3], String) {
        let host = self.lay(tags);
        let mut ctx = self.rig.ctx.clone();
        ctx.hosts = Hosts::single(host.clone());
        let ba = u64::try_from(ba).expect("positive");
        let na = u64::try_from(na).expect("positive");
        let baseline = if na == ba + 1 {
            Baseline::Dir(self.own[&ba].clone())
        } else {
            Baseline::First {
                channel: "stable".into(),
                index_build: na,
            }
        };
        let req = IndexRequest {
            baseline,
            delta: valid_until(OWN_VALID_UNTIL),
            out: self
                .rig
                .path(&format!("run-{ba}-{na}-{tags:?}").replace([' ', ',', '[', ']'], "")),
            upload: true,
            pack_dir: None,
        };
        let mut log = Rig::log();
        let ok = publish_with(&ctx, &req, &mut log).is_ok();
        let after = [
            self.code_at(&host, 2),
            self.code_at(&host, 3),
            self.code_at(&host, 4),
        ];
        (ok, after, log.text())
    }
}

#[test]
fn the_real_indexer_takes_the_models_decision_on_every_channel_it_reaches() {
    let model = atpkg_index_publish_walk_model();
    let buggy = interp::with_buggy(&model, 1);
    // Every decision publisher A faces while it holds a baseline, and whether the
    // committed model reaches it (else only the mutant does).
    let mut cases: BTreeMap<(i64, i64, [i64; 3]), bool> = BTreeMap::new();
    for (m, committed) in [(&model, true), (&buggy, false)] {
        for s in reachable(m) {
            if s["pa"] == 1 {
                cases
                    .entry((s["ba"], s["na"], channel(&s)))
                    .or_insert(committed);
            }
        }
    }
    let fx = Fixture::new();
    let mut controls: BTreeSet<&str> = BTreeSet::new();
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for &(ba, na, tags) in cases.keys() {
        let (ok, after, log) = fx.publish(ba, na, tags);
        let mut state = model.init_state();
        for (var, value) in [
            ("pa", 1),
            ("ba", ba),
            ("na", na),
            ("t2", tags[0]),
            ("t3", tags[1]),
            ("t4", tags[2]),
        ] {
            state.insert(var, value);
        }
        let enabled: Vec<&str> = ["CreateA", "ConvergeA", "RefuseA"]
            .into_iter()
            .filter(|a| model.action_enabled(a, &state))
            .collect();
        assert_eq!(enabled.len(), 1, "one decision at {state:?}: {enabled:?}");
        taken.insert(enabled[0]);
        let label = format!("baseline {ba}, number {na}, channel {tags:?}");
        if enabled[0] == "RefuseA" {
            assert!(
                !ok,
                "{label}: the model refuses, the indexer published:\n{log}"
            );
            assert_eq!(after, tags, "{label}: a refusal writes nothing:\n{log}");
            if buggy.action_enabled("CreateA", &state) || buggy.action_enabled("ConvergeA", &state)
            {
                controls.insert(if na != ba + 1 {
                    "a number past the baseline's successor"
                } else if !published(tags, u64::try_from(ba).expect("positive")) {
                    "a baseline counted off a draft"
                } else {
                    "a converge onto another publisher's bytes"
                });
            }
        } else {
            let decision = enabled[0];
            assert!(
                ok,
                "{label}: the model takes {decision}, the indexer refused:\n{log}"
            );
            assert!(model.fire(decision, &mut state));
            if decision == "CreateA" {
                // The fake's create publishes in one step: the model's Create, Finish.
                assert!(model.fire("FinishA", &mut state));
            }
            assert_eq!(
                after,
                channel(&state),
                "{label}: {decision} leaves the channel the model says:\n{log}"
            );
        }
    }
    assert_eq!(
        controls,
        BTreeSet::from([
            "a baseline counted off a draft",
            "a converge onto another publisher's bytes",
            "a number past the baseline's successor",
        ]),
        "every mutant decision is driven, and refused, at least once"
    );
    assert_eq!(
        taken,
        BTreeSet::from(["ConvergeA", "CreateA", "RefuseA"]),
        "every decision is driven through the real indexer"
    );
    let committed = cases.values().filter(|c| **c).count();
    assert!(
        committed >= 10 && cases.len() > committed,
        "{committed} of {}",
        cases.len()
    );
}
