// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn changed(crates: &[&str]) -> Scope {
    Scope::changed(
        "origin/main",
        crates.iter().map(|s| (*s).to_string()).collect(),
        true,
    )
}

/// A selection hands cargo exactly its crates; the empty cone selects
/// nothing yet falls back WIDER (`--workspace`), never to an empty argv;
/// and only the cone can say nothing has a library.
#[test]
fn a_selection_hands_cargo_its_crates_and_an_empty_cone_falls_back_wider() {
    assert_eq!(
        Scope::packages(&["a".into(), "b".into()]).args(),
        ["-p", "a", "-p", "b"]
    );
    assert_eq!(Scope::packages(&[]), Scope::Workspace);
    assert_eq!(changed(&["a", "b"]).args(), ["-p", "a", "-p", "b"]);
    let empty = changed(&[]);
    assert!(empty.selects_nothing() && !empty.is_workspace());
    assert_eq!(empty.args(), ["--workspace"]);
    assert!(!empty.includes_crate("aterm-link"));
    assert!(Scope::workspace().includes_crate("aterm-link"));
    assert!(!Scope::crate_only("aterm-grid").includes_crate("aterm-link"));
    // A member the cone names by its spec, because the lock holds another
    // package of its name, is that package — and no other one.
    let shared = changed(&["getrandom@0.2.17", "path+file:///r/crates/aterm-x#x@1.0.0"]);
    assert_eq!(
        shared.spec_of("getrandom").as_deref(),
        Some("getrandom@0.2.17")
    );
    assert_eq!(
        shared.spec_of("x").as_deref(),
        Some("path+file:///r/crates/aterm-x#x@1.0.0")
    );
    assert!(!shared.includes_crate("getrandom@0.2.17"));
    assert!(!shared.includes_crate("aterm-x"));
    assert_eq!(
        Scope::workspace().spec_of("getrandom").as_deref(),
        Some("getrandom")
    );
    // A cone says what the change touched by package name, whichever spec names it, and a
    // crate only there for its dependency is not touched; a cone built from its crates alone
    // counts each one.
    let seeded = changed(&["aterm-gui", "aterm-uds"])
        .with_seeds(vec!["aterm-uds".into(), "ring@0.17.14".into()]);
    assert!(seeded.touches("aterm-uds") && seeded.touches("ring"));
    assert!(!seeded.touches("aterm-gui") && seeded.includes_crate("aterm-gui"));
    assert!(changed(&["aterm-gui"]).touches("aterm-gui"));
    // A seed whose change only its own tests compile is touched but reaches no dependent; a
    // scope that is no cone has none alone. CONTROL: another seed of the cone reaches them.
    let alone = seeded.clone().with_alone(vec!["aterm-uds".into()]);
    assert!(alone.touches("aterm-uds") && !alone.reaches_dependents("aterm-uds"));
    assert!(alone.reaches_dependents("ring") && seeded.reaches_dependents("aterm-uds"));
    assert!(!alone.reaches_dependents("aterm-gui"));
    assert!(
        Scope::crate_only("aterm-uds")
            .with_alone(vec!["aterm-uds".into()])
            .reaches_dependents("aterm-uds")
    );
    assert!(Scope::crate_only("aterm-gui").touches("aterm-gui"));
    assert!(Scope::workspace().touches("aterm-gui"));
    assert!(Scope::crate_only("aterm-lab").has_lib_target());
    assert!(!Scope::changed("main", vec!["aterm-lab".into()], false).has_lib_target());
    // What a cone's lint leaves out goes from the lint's label alone, by package name whichever
    // spec names it. CONTROL: with nothing left out, it reads as every other check's.
    let pair = changed(&["aterm-gui", "aterm-uds", "getrandom@0.2.17"]);
    assert_eq!(pair.lint_label(), pair.label());
    let some = pair.with_unlinted(vec!["getrandom".into(), "aterm-gui".into()]);
    assert_eq!(some.lint_label(), "-p aterm-uds");
    assert_eq!(
        some.label(),
        "-p aterm-gui -p aterm-uds -p getrandom@0.2.17"
    );
    assert_eq!(
        Scope::workspace()
            .with_unlinted(vec!["aterm-gui".into()])
            .lint_label(),
        "--workspace"
    );
}
