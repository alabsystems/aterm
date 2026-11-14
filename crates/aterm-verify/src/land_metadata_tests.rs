// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presentation data uses the ordinary file rule, never a Rust normalization waiver.

use super::*;

const POLICY: &str = "crates/aterm-verify/src/stages.rs";
const WORDING: &str = "crates/aterm-verify/src/objc-pass.tsv";
const SOURCE: &str = "pub const COMMAND: &str = \"window_drive\";\n\
    pub const STATUS: i32 = 0;\n\
    pub const SEVERITY: &str = \"GateFailed\";\n\
    pub const REFUSE: bool = true;\n\
    pub fn accepted(code: i32) -> bool { code == STATUS }\n\
    \n\n\n\n\n\n\n\n\
    pub const JOBS: usize = 1;\n";
const TABLE: &str = "window_drive\tthe window answered\n";

fn prepared() -> World {
    let world = world();
    write(&world.rival, POLICY, SOURCE);
    write(&world.rival, WORDING, TABLE);
    git(&world.rival, &["add", "-A"]);
    git(&world.rival, &["commit", "-qm", "separate wording data"]);
    git(&world.rival, &["push", "-q", "origin", "main"]);
    git(&world.work, &["fetch", "-q", "origin"]);
    git(&world.work, &["merge", "-q", "--ff-only", "origin/main"]);
    commit(
        &world.work,
        POLICY,
        &SOURCE.replace("JOBS: usize = 1", "JOBS: usize = 2"),
    );
    world
}

#[test]
fn objc_wording_data_composes_without_relabeling_the_checked_tree() {
    let world = prepared();
    let checked = git(&world.work, &["rev-parse", "HEAD"]);
    let checked_tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
    let moved = RefCell::new(None);
    let (code, said, bases) = land_with(&world, crates(&["aterm-verify"]), Lane::Pass, |_, n| {
        if n == 0 {
            *moved.borrow_mut() = Some(rival_moves(
                &world,
                WORDING,
                "window_drive\tthe window answered; its callback waited\n",
            ));
        }
        green()
    });
    assert_eq!(code, exit::PASS, "{said:#?}");
    assert_eq!(bases.len(), 1, "only disjoint data arrived: {said:#?}");
    let moved = moved.into_inner().unwrap();
    let landing = origin_main(&world);
    let receipt = stated(&world, &landing).unwrap();
    assert_eq!(receipt.composed_onto.as_deref(), Some(moved.as_str()));
    assert_eq!(receipt.checked.len(), 1);
    assert_eq!(receipt.checked[0].tree, checked_tree);
    let merged_tree = git(&world.work, &["rev-parse", &format!("{landing}^{{tree}}")]);
    assert_ne!(
        merged_tree, checked_tree,
        "composition is not a new execution"
    );
    assert!(
        receipt_composition(
            &world.work,
            &std::env::var_os("PATH").unwrap_or_default(),
            &checked,
            &moved,
            &merged_tree,
        )
        .unwrap(),
        "the historical reader accepts the same ordinary disjoint composition"
    );
    assert_eq!(
        git(&world.work, &["show", &format!("{landing}:{WORDING}")]),
        "window_drive\tthe window answered; its callback waited"
    );
}

#[test]
fn shared_objc_policy_and_malformed_rust_still_require_a_new_judgment() {
    // The injected judge is deliberately red on the new tree. This verifies dispatch
    // and refusal to publish; actual Rust syntax and semantics remain the compiler's job.
    for changed in [
        SOURCE.replace("window_drive", "other_command"),
        SOURCE.replace("STATUS: i32 = 0", "STATUS: i32 = 2"),
        SOURCE.replace("GateFailed", "CouldNotRun"),
        SOURCE.replace("REFUSE: bool = true", "REFUSE: bool = false"),
        SOURCE.replace("code == STATUS", "code != STATUS"),
        SOURCE.replace(
            "pub const COMMAND",
            "unknown_field: true,\npub const COMMAND",
        ),
        SOURCE.replace("pub const COMMAND", "fn malformed( {\npub const COMMAND"),
    ] {
        let world = prepared();
        let checked = git(&world.work, &["rev-parse", "HEAD"]);
        let moved = RefCell::new(None);
        let (code, said, bases) =
            land_with(&world, crates(&["aterm-verify"]), Lane::Pass, |base, n| {
                if n == 0 {
                    *moved.borrow_mut() = Some(rival_moves(&world, POLICY, &changed));
                    return green();
                }
                let tree = git(&world.work, &["rev-parse", "HEAD^{tree}"]);
                assert!(
                    !receipt_composition(
                        &world.work,
                        &std::env::var_os("PATH").unwrap_or_default(),
                        &checked,
                        base,
                        &tree,
                    )
                    .unwrap()
                );
                ran(
                    exit::FAILED,
                    vec![red("test", &["-p aterm-verify --lib"])],
                    1,
                )
            });
        assert_eq!(bases.len(), 2, "{changed}: {said:#?}");
        assert_eq!(code, exit::FAILED, "{changed}: {said:#?}");
        assert_eq!(origin_main(&world), moved.into_inner().unwrap());
    }
}

#[test]
fn the_wording_data_filename_does_not_waive_shared_changes() {
    let world = prepared();
    commit(&world.work, WORDING, "window_drive\tbranch wording\n");
    let (code, said, bases) = land_with(&world, crates(&["aterm-verify"]), Lane::Pass, |_, n| {
        if n == 0 {
            rival_moves(&world, WORDING, "window_drive\tmain wording\n");
        }
        green()
    });
    assert_eq!(
        code,
        exit::CONFLICT,
        "conflicting data is not waived: {said:#?}"
    );
    assert_eq!(
        bases.len(),
        1,
        "a conflicted merge starts no second judgment"
    );
}
