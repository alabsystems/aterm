// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of [`GithubFetcher::walk_candidates`] to the `AtpkgIndexPublishWalk` model.

use super::GithubFetcher;
use aterm_models_atpkg::atpkg_index_publish_walk_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_update_core::{HeadAnswer, HttpError};
use std::collections::{BTreeSet, VecDeque};

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

fn channel(state: &State) -> [i64; 3] {
    [state["t2"], state["t3"], state["t4"]]
}

fn published(tags: [i64; 3], build: u64) -> bool {
    match build {
        1 => true,
        2..=4 => matches!(tags[build as usize - 2], 2 | 4),
        _ => false,
    }
}

fn newest(tags: [i64; 3]) -> u64 {
    (1..=4).rev().find(|b| published(tags, *b)).unwrap_or(1)
}

fn real_walk(floor: u64, tags: [i64; 3]) -> u64 {
    const PREFIX: &str = "https://github.com/alabsystems/aterm/releases/download/atpkg-index-";
    let fetcher = GithubFetcher::new("alabsystems".into()).with_index_floor(floor);
    let head = |url: &str| -> Result<HeadAnswer, HttpError> {
        let build = url
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.strip_suffix("/index.toml"))
            .and_then(|b| b.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("the walk HEADs only index tags: {url}"));
        Ok(if published(tags, build) {
            HeadAnswer {
                code: 302,
                location: Some("https://release-assets.githubusercontent.com/object".into()),
            }
        } else {
            HeadAnswer {
                code: 404,
                location: None,
            }
        })
    };
    let fetch = |url: &str, _cap: u64| -> Result<Vec<u8>, String> { Ok(url.as_bytes().to_vec()) };
    let candidates = fetcher
        .walk_candidates(&fetcher.index_slug(), &head, &fetch)
        .expect("a walk over answered HEADs is never an error");
    candidates.first().map_or(floor, |newest| {
        newest
            .label
            .strip_prefix("atpkg-index-")
            .and_then(|b| b.parse().ok())
            .expect("a walk candidate is an index tag")
    })
}

#[test]
fn the_real_walk_lands_the_newest_index_on_every_channel_the_model_reaches() {
    let model = atpkg_index_publish_walk_model();
    let seen: BTreeSet<(u64, [i64; 3])> = reachable(&model)
        .iter()
        .map(|s| (s["floor"] as u64, channel(s)))
        .collect();
    let mut moved = 0;
    for &(floor, tags) in &seen {
        let want = newest(tags);
        assert_eq!(
            real_walk(floor, tags),
            want,
            "from floor {floor} over channel {tags:?} the walk must land atpkg-index-{want}"
        );
        moved += usize::from(want > floor);
    }
    assert!(
        moved >= 6 && seen.len() >= 20,
        "the bind must cover walks that move ({moved} of {} channels)",
        seen.len()
    );

    let buggy = interp::with_buggy(&model, 1);
    let hole = reachable(&buggy)
        .into_iter()
        .find(|s| s["floor"] == 1 && channel(s) == [0, 1, 4])
        .expect("the Buggy = 1 model reaches the draft-baseline hole");
    assert!(!buggy.check_invariant("WalkLandsNewest", &hole));
    assert_ne!(
        real_walk(1, [0, 1, 4]),
        4,
        "two missing numbers directly above the floor are a gap no walk crosses"
    );
}
