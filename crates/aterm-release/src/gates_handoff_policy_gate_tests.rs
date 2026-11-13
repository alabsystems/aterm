// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update_core::handoff_policy::SOURCE_PATH;

fn handoff_policy_gate(
    git: &dyn GitRunner,
    repo: &Path,
    predecessor: Option<&str>,
) -> Result<CutHandoffPolicy> {
    let (text, policy) = crate::bundle::handoff_policy_for_cut(repo)
        .map_err(|why| Error::new(format!("handoff-policy gate: {why}")))?;
    sealed_policy_kept(
        git,
        &fixture_origin::FixtureOrigin(git),
        &GateTree::Dir(repo),
        predecessor,
        policy,
        &text,
    )
}

struct NoGit;

impl GitRunner for NoGit {
    fn git(&self, args: &[&str]) -> Result<crate::ledger::RunOut> {
        panic!("the policy gate ran git with no predecessor: {args:?}")
    }
}

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn with_policy(label: &str, text: Option<&str>) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-release-policy-{label}-"))
            .tempdir()
            .expect("scratch repo");
        fs::create_dir_all(dir.path().join("publish")).expect("scratch repo");
        if let Some(text) = text {
            fs::write(dir.path().join(SOURCE_PATH), text).expect("write the policy");
        }
        Scratch(dir)
    }
}

#[test]
fn the_checked_in_policy_passes_the_gate() {
    let package = aterm_tempfile::manifest_dir();
    let root = package
        .parent()
        .and_then(Path::parent)
        .expect("crates/aterm-release sits two levels under the root");
    crate::bundle::handoff_policy_for_cut(root)
        .expect("the checked-in policy is one producers can follow");
}

#[test]
fn a_policy_producers_could_not_follow_refuses_the_cut() {
    for (label, text, why) in [
        ("missing", None, "handoff-policy.toml"),
        (
            "typo",
            Some("schema = 1\ncary = \"repaint\"\n"),
            "unknown key `cary`",
        ),
        (
            "reserved",
            Some("schema = 1\nseamless = false\n"),
            "`seamless` is reserved",
        ),
        ("value", Some("schema = 1\ncarry = \"blank\"\n"), "`carry`"),
        ("schema", Some("carry = \"repaint\"\n"), "schema"),
        (
            "range",
            Some("schema = 1\napplies_to_producers = [9, 2]\n"),
            "empty range",
        ),
        (
            "unranged",
            Some("schema = 1\ncarry = \"repaint\"\n"),
            "a knob outside `[[ranges]]`",
        ),
        (
            "the one-cut retire",
            Some("schema = 1\nretire = { producers = [1, 2], reason = \"gone\" }\n"),
            "`retire` is now a `[[retired]]` table",
        ),
        (
            "a rescue with no reason",
            Some(
                "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"repaint\"\n\n\
                 [[ranges]]\nproducers = [1, 2]\ncarry = \"repaint\"\n",
            ),
            "no `reason`",
        ),
        (
            "not the hull",
            Some(
                "schema = 1\napplies_to_producers = [1, 2]\ncarry = \"visible\"\n\n\
                 [[ranges]]\nproducers = [1, 2]\ncarry = \"repaint\"\nreason = \"x\"\n",
            ),
            "not the hull of `[[ranges]]`",
        ),
        (
            "a knob with nothing before it",
            Some(
                "schema = 1\napplies_to_producers = [1790120000, 1790129999]\ncarry = \
                 \"repaint\"\n\n[[ranges]]\nproducers = [1790120000, 1790129999]\ncarry = \
                 \"repaint\"\nreason = \"x\"\n",
            ),
            "no build can hand over",
        ),
    ] {
        let scratch = Scratch::with_policy(label, text);
        let refused = handoff_policy_gate(&NoGit, scratch.0.path(), None)
            .err()
            .unwrap_or_else(|| panic!("{label}: the gate passed"))
            .to_string();
        assert!(
            refused.contains("handoff-policy gate") && refused.contains(why),
            "{label}: {refused}"
        );
    }
}
