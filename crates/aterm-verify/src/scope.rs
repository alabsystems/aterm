// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scoping: what this run actually built and tested.
//!
//! Three kinds, and the whole workspace is only one of them: `--scope <crate>`
//! narrows to `-p <crate>`, `--changed` narrows to the diff's crates closed under
//! reverse dependency ([`crate::changed`]), and everything else is whole-tree.
//!
//! Three consequences, and all three are decisions, not formatting:
//!  * the driver flags and the ladder label are derived from ONE value, so a
//!    stage can never claim `--workspace` in its label while running `-p`;
//!  * the regex search lane is the one stage a scope can switch OFF (it is a
//!    second, feature-enabled run of `aterm-search`), so scoping away from that
//!    crate removes the stage entirely rather than skipping it — matching the
//!    script, where the stage's header was never even printed;
//!  * EVERY narrowing forfeits the merge contract. [`Scope::narrowing`] returns
//!    the sentence the verdict prints instead of the claim, and it is `Some` for
//!    exactly the variants [`Scope::is_workspace`] rejects — a new narrowing that
//!    forgot to forfeit would have to break that pairing to exist, and the test
//!    below is what refuses it.

/// A change-scoped selection: the crates `--changed` chose, and what it needs to
/// remember about them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    /// The ref the diff was taken against (`--base`, default `main`).
    pub base: String,
    /// The selected crates, sorted. Legitimately EMPTY when the diff touched no
    /// workspace crate at all.
    pub crates: Vec<String>,
    /// Does any selected crate have a library target? `cargo test --doc` is a
    /// hard error on an all-binary selection, which is an ordinary outcome here.
    pub any_lib: bool,
}

/// What the run is narrowed to.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Workspace,
    Crate(String),
    Changed(Changed),
}

impl Scope {
    #[must_use]
    pub fn workspace() -> Self {
        Self::Workspace
    }

    #[must_use]
    pub fn crate_only(name: impl Into<String>) -> Self {
        Self::Crate(name.into())
    }

    #[must_use]
    pub fn changed(base: impl Into<String>, crates: Vec<String>, any_lib: bool) -> Self {
        Self::Changed(Changed {
            base: base.into(),
            crates,
            any_lib,
        })
    }

    #[must_use]
    pub fn from_option(name: Option<String>) -> Self {
        name.map_or(Self::Workspace, Self::Crate)
    }

    /// `Some(crate)` when narrowed to exactly one crate by `--scope`.
    #[must_use]
    pub fn crate_name(&self) -> Option<&str> {
        match self {
            Self::Crate(c) => Some(c),
            _ => None,
        }
    }

    /// The ONLY shape that can discharge the merge contract. The verdict reads
    /// this; see [`crate::verdict::discharges_merge_contract`].
    #[must_use]
    pub fn is_workspace(&self) -> bool {
        matches!(self, Self::Workspace)
    }

    /// The selection is legitimately EMPTY: compile nothing.
    ///
    /// Only `--changed` can produce it (a docs-only branch), and the stages that
    /// compile must ask BEFORE they build a command — see [`Scope::args`].
    #[must_use]
    pub fn selects_nothing(&self) -> bool {
        matches!(self, Self::Changed(c) if c.crates.is_empty())
    }

    /// Does the selection contain a package with a library target? Always yes
    /// unless `--changed` proved otherwise, because an unanswerable question
    /// must leave the doctest stage RUNNING.
    #[must_use]
    pub fn has_lib_target(&self) -> bool {
        match self {
            Self::Changed(c) => c.any_lib,
            _ => true,
        }
    }

    /// The ladder label, which is also the script's `$SCOPE_LABEL`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Workspace => "--workspace".to_string(),
            Self::Crate(c) => format!("-p {c}"),
            Self::Changed(c) if c.crates.is_empty() => "<no crates selected>".to_string(),
            Self::Changed(c) => c
                .crates
                .iter()
                .map(|n| format!("-p {n}"))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// The driver flags, which are the script's `${CARGO_SCOPE[@]}`.
    ///
    /// The empty selection answers `--workspace` and NOT an empty argument list,
    /// for the script's reason: a bare `targo build` builds the whole default
    /// workspace, so if the [`Scope::selects_nothing`] guard were ever dropped the
    /// fallback would compile MORE than the selection rather than compile the
    /// whole tree under a narrow label. Wrong, and loudly wrong, beats wrong and
    /// silent.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Workspace => vec!["--workspace".to_string()],
            Self::Crate(c) => vec!["-p".to_string(), c.clone()],
            Self::Changed(c) if c.crates.is_empty() => vec!["--workspace".to_string()],
            Self::Changed(c) => c
                .crates
                .iter()
                .flat_map(|n| ["-p".to_string(), n.clone()])
                .collect(),
        }
    }

    /// The word the verdict prints for `scope=…` — the script's `$SCOPE_DESC`.
    #[must_use]
    pub fn desc(&self) -> String {
        match self {
            Self::Workspace => "workspace".to_string(),
            Self::Crate(c) => c.clone(),
            Self::Changed(c) => format!("changed:{}", c.crates.len()),
        }
    }

    /// Why this run proved less than the merge contract, in the verdict's words.
    /// `None` for — and only for — the whole-tree run.
    #[must_use]
    pub fn narrowing(&self) -> Option<String> {
        match self {
            Self::Workspace => None,
            Self::Crate(c) => Some(format!(
                "scoped to -p {c}: the rest of the workspace was not built or tested"
            )),
            Self::Changed(c) if c.crates.is_empty() => Some(format!(
                "change-scoped against {} and NO workspace crate changed: nothing was built or tested",
                c.base
            )),
            Self::Changed(c) => Some(format!(
                "change-scoped against {} to {} crate(s) ({}): every other workspace crate was not built or tested",
                c.base,
                c.crates.len(),
                c.crates.join(" ")
            )),
        }
    }

    /// Would this selection compile `name`? `--workspace` selects everything;
    /// the narrowings select exactly what they name.
    ///
    /// The whole-tree answer is TRUE rather than "unanswerable" on purpose: a
    /// pass that keys off this decides whether it has anything to run, and a
    /// whole-tree run always does.
    #[must_use]
    pub fn includes_crate(&self, name: &str) -> bool {
        match self {
            Self::Workspace => true,
            Self::Crate(c) => c == name,
            Self::Changed(c) => c.crates.iter().any(|n| n == name),
        }
    }

    /// The regex search lane runs whole-tree or when the selection CONTAINS
    /// `aterm-search`.
    ///
    /// Without it the whole regex battery compiles out to zero cases and the
    /// suite stays green with no regex coverage, so the lane exists; but under a
    /// scope that excludes `aterm-search` there is nothing for it to run.
    #[must_use]
    pub fn includes_regex_lane(&self) -> bool {
        self.includes_crate("aterm-search")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(crates: &[&str]) -> Scope {
        Scope::changed(
            "main",
            crates.iter().map(|s| (*s).to_string()).collect(),
            true,
        )
    }

    #[test]
    fn whole_tree_scoping_is_the_workspace() {
        let s = Scope::workspace();
        assert_eq!(s.label(), "--workspace");
        assert_eq!(s.args(), ["--workspace"]);
        assert_eq!(s.crate_name(), None);
        assert_eq!(s.desc(), "workspace");
        assert!(s.is_workspace());
        assert!(!s.selects_nothing());
    }

    #[test]
    fn a_narrowed_scope_becomes_dash_p_everywhere_at_once() {
        let s = Scope::crate_only("aterm-grid");
        assert_eq!(s.label(), "-p aterm-grid");
        assert_eq!(s.args(), ["-p", "aterm-grid"]);
        assert_eq!(s.crate_name(), Some("aterm-grid"));
        assert_eq!(s.desc(), "aterm-grid");
        assert!(!s.is_workspace());
    }

    #[test]
    fn a_change_scope_becomes_one_dash_p_per_selected_crate() {
        let s = changed(&["aterm-grid", "aterm-gui"]);
        assert_eq!(s.label(), "-p aterm-grid -p aterm-gui");
        assert_eq!(s.args(), ["-p", "aterm-grid", "-p", "aterm-gui"]);
        assert_eq!(s.desc(), "changed:2");
        assert_eq!(s.crate_name(), None, "it is not ONE crate");
        assert!(!s.is_workspace());
        assert!(!s.selects_nothing());
    }

    #[test]
    fn the_label_and_the_flags_can_never_disagree() {
        // Both derive from the same value: a stage cannot print `--workspace`
        // while running `-p aterm-grid`, which is how a narrowed run would look
        // like a whole-tree one in a scrollback.
        for s in [
            Scope::workspace(),
            Scope::crate_only("aterm-pty"),
            changed(&["aterm-pty"]),
            changed(&["aterm-grid", "aterm-gui", "aterm-cli"]),
        ] {
            assert_eq!(s.label(), s.args().join(" "));
        }
    }

    #[test]
    fn the_empty_selection_says_so_and_falls_back_wider_never_narrower() {
        // The one case where the label and the flags differ, deliberately: the
        // label states the truth ("nothing"), and the flags are the WIDEST
        // fallback, so a dropped guard would build too much and be noticed rather
        // than build everything while printing a narrow label.
        let s = changed(&[]);
        assert!(s.selects_nothing());
        assert_eq!(s.label(), "<no crates selected>");
        assert_eq!(s.args(), ["--workspace"]);
        assert_eq!(s.desc(), "changed:0");
        assert!(!s.is_workspace(), "empty is not whole-tree");
    }

    #[test]
    fn every_narrowing_forfeits_the_contract_and_says_why() {
        // THE pairing: `is_workspace()` decides the claim, `narrowing()` decides
        // the words, and a scope kind that answered one without the other would
        // either claim the contract silently or refuse it mutely.
        for s in [
            Scope::crate_only("aterm-grid"),
            changed(&[]),
            changed(&["aterm-grid"]),
        ] {
            assert!(!s.is_workspace());
            assert!(s.narrowing().is_some(), "{s:?} narrows but does not say so");
        }
        assert_eq!(Scope::workspace().narrowing(), None);
    }

    #[test]
    fn the_narrowing_sentence_names_the_base_and_the_crates() {
        assert_eq!(
            changed(&["aterm-grid", "aterm-gui"]).narrowing().unwrap(),
            "change-scoped against main to 2 crate(s) (aterm-grid aterm-gui): \
             every other workspace crate was not built or tested"
        );
        assert_eq!(
            Scope::changed("origin/main", vec![], true)
                .narrowing()
                .unwrap(),
            "change-scoped against origin/main and NO workspace crate changed: \
             nothing was built or tested"
        );
        assert_eq!(
            Scope::crate_only("aterm-grid").narrowing().unwrap(),
            "scoped to -p aterm-grid: the rest of the workspace was not built or tested"
        );
    }

    #[test]
    fn the_regex_lane_follows_aterm_search_only() {
        assert!(Scope::workspace().includes_regex_lane());
        assert!(Scope::crate_only("aterm-search").includes_regex_lane());
        assert!(!Scope::crate_only("aterm-grid").includes_regex_lane());
        assert!(!Scope::crate_only("aterm-gui").includes_regex_lane());
        // …and a change scope narrows it the same way, which is the point of
        // asking the SCOPE rather than asking `--scope`.
        assert!(changed(&["aterm-grid", "aterm-search"]).includes_regex_lane());
        assert!(!changed(&["aterm-grid", "aterm-gui"]).includes_regex_lane());
        assert!(!changed(&[]).includes_regex_lane());
    }

    #[test]
    fn only_a_change_scope_can_report_that_nothing_has_a_library() {
        assert!(Scope::workspace().has_lib_target());
        assert!(Scope::crate_only("xtask").has_lib_target());
        assert!(changed(&["aterm-grid"]).has_lib_target());
        assert!(!Scope::changed("main", vec!["xtask".into()], false).has_lib_target());
    }

    #[test]
    fn from_option_round_trips_the_cli_value() {
        assert_eq!(Scope::from_option(None), Scope::workspace());
        assert_eq!(Scope::from_option(Some("x".into())), Scope::crate_only("x"));
    }
}
