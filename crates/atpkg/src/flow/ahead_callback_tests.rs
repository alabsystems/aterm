// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

struct NoFetch;

impl Fetcher for NoFetch {
    fn index_candidates(&self) -> Result<Vec<crate::select::Candidate>, String> {
        panic!("the prepared lane never resolves an index");
    }

    fn pkg_manifest(
        &self,
        _repo: &str,
        _program: &str,
        _build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        panic!("the prepared lane never resolves a package");
    }

    fn download(&self, _repo: &str, _asset: &str, _dest: &Path) -> Result<(), String> {
        panic!("these completed or inline jobs never download on the worker");
    }
}

fn lane(slots: Vec<Slot>, sizes: &[u64], budget: u64) -> Ahead {
    assert_eq!(slots.len(), sizes.len());
    let jobs = sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| Job {
            unit: i,
            program: format!("p{i}"),
            pinned: 1,
            repo: "model/repository".to_string(),
            artifact: crate::manifest::Artifact {
                target: "model-target".to_string(),
                kind: "binary".to_string(),
                protocol: "github-release".to_string(),
                asset: format!("p{i}.tar.zst"),
                sha256: String::new(),
                sha512: String::new(),
                tree_root: String::new(),
                size,
                reloc: "self-contained".to_string(),
                cost: crate::manifest::Cost::default(),
                url: String::new(),
                payload: String::new(),
                entry: String::new(),
                strip_components: 0,
                links: Default::default(),
                vendor: String::new(),
                provenance: None,
            },
            dl: PathBuf::from(format!("/model/p{i}/p{i}.tar.zst")),
        })
        .collect();
    let n = sizes.len();
    Ahead {
        jobs,
        budget,
        state: Mutex::new(State {
            slots,
            claimed: vec![false; n],
            current: None,
            passed: BTreeSet::new(),
            fetched: vec![false; n],
            halted: false,
        }),
        changed: Condvar::new(),
    }
}

fn notices(lane: &Ahead, read_bump: impl Fn() -> Vec<String>) -> usize {
    let count = Cell::new(0);
    lane.work_with_bump(
        &NoFetch,
        &|seen| {
            assert!(seen.all_fetched(&[0]));
            count.set(count.get() + 1);
        },
        read_bump,
    );
    count.get()
}

#[test]
fn a_last_inline_archive_is_announced_before_the_worker_leaves() {
    let already_in = lane(vec![Slot::Failed], &[7], 7);
    let dl = already_in.jobs[0].dl.clone();
    assert!(already_in.claim("p0", &dl).is_none());
    already_in.fetched_inline("p0", &dl);
    assert_eq!(notices(&already_in, Vec::new), 1);

    let refused = lane(vec![Slot::Failed], &[7], 7);
    assert_eq!(
        notices(&refused, || {
            assert!(refused.claim("p0", &refused.jobs[0].dl).is_none());
            refused.halt();
            Vec::new()
        }),
        0
    );

    let wrong_path = lane(vec![Slot::Failed], &[7], 7);
    assert_eq!(
        notices(&wrong_path, || {
            assert!(wrong_path.claim("p0", &wrong_path.jobs[0].dl).is_none());
            wrong_path.fetched_inline("p0", Path::new("/model/another.tar.zst"));
            wrong_path.halt();
            Vec::new()
        }),
        0
    );

    let last_inline = lane(vec![Slot::Failed], &[7], 7);
    let bump_reads = Cell::new(0);
    let count = notices(&last_inline, || {
        bump_reads.set(bump_reads.get() + 1);
        assert!(last_inline.claim("p0", &last_inline.jobs[0].dl).is_none());
        last_inline.fetched_inline("p0", &last_inline.jobs[0].dl);
        Vec::new()
    });
    assert_eq!(bump_reads.get(), 1);
    assert!(last_inline.all_fetched(&[0]));
    assert!(last_inline.state.lock().unwrap().halted);
    assert_eq!(
        count, 1,
        "LAST_INLINE_ARCHIVE_MUST_BE_ANNOUNCED_BEFORE_EXIT"
    );
}
