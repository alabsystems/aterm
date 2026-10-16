// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the checks that read a selection (lint, test, doc, trust) build and
//! test: the whole workspace (`--all`), the packages `-p` named, or the change
//! cone ([`crate::changed`]). The driver flags and the label derive from one
//! value, so a check can never print `--workspace` while running `-p`.

/// The change cone: the crates whose files changed since `base`, closed under
/// reverse dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    /// The ref the diff was taken against.
    pub base: String,
    /// The selected crates, sorted; empty when the change touched no workspace
    /// crate.
    pub crates: Vec<String>,
    /// Does any selected crate have a library target? `cargo test --doc` is an
    /// error on an all-binary selection.
    pub any_lib: bool,
}

/// What a selection-reading check runs over.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Workspace,
    /// `-p <crate>`, once or more.
    Packages(Vec<String>),
    Changed(Changed),
}

impl Scope {
    #[must_use]
    pub fn workspace() -> Self {
        Self::Workspace
    }

    /// `-p` for each of `names`; none is the workspace.
    #[must_use]
    pub fn packages(names: &[String]) -> Self {
        if names.is_empty() {
            Self::Workspace
        } else {
            Self::Packages(names.to_vec())
        }
    }

    #[must_use]
    pub fn crate_only(name: impl Into<String>) -> Self {
        Self::Packages(vec![name.into()])
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
    pub fn is_workspace(&self) -> bool {
        matches!(self, Self::Workspace)
    }

    /// The selection is empty: a cone no crate is in. The checks that compile ask
    /// before they build a command ([`Scope::args`]).
    #[must_use]
    pub fn selects_nothing(&self) -> bool {
        matches!(self, Self::Changed(c) if c.crates.is_empty())
    }

    /// Does the selection hold a package with a library target? Yes unless the
    /// cone proved otherwise: an unanswerable question leaves doctests running.
    #[must_use]
    pub fn has_lib_target(&self) -> bool {
        match self {
            Self::Changed(c) => c.any_lib,
            _ => true,
        }
    }

    /// The crates `-p` names, the cone's, or none for the workspace.
    #[must_use]
    pub fn crates(&self) -> &[String] {
        match self {
            Self::Workspace => &[],
            Self::Packages(p) => p,
            Self::Changed(c) => &c.crates,
        }
    }

    /// `--workspace`, `-p a -p b`, or `<no crates selected>` for the empty cone.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Changed(c) if c.crates.is_empty() => "<no crates selected>".to_string(),
            _ => self.args().join(" "),
        }
    }

    /// The driver flags. The empty cone answers `--workspace`, never an empty
    /// list: a dropped [`Scope::selects_nothing`] guard then builds too much and
    /// is noticed, rather than building everything under a narrow label.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        if matches!(self, Self::Workspace) || self.selects_nothing() {
            return vec!["--workspace".to_string()];
        }
        self.crates()
            .iter()
            .flat_map(|n| ["-p".to_string(), n.clone()])
            .collect()
    }

    /// Would this selection compile `name`?
    #[must_use]
    pub fn includes_crate(&self, name: &str) -> bool {
        match self {
            Self::Workspace => true,
            _ => self.crates().iter().any(|n| n == name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(crates: &[&str]) -> Scope {
        Scope::changed(
            "origin/main",
            crates.iter().map(|s| (*s).to_string()).collect(),
            true,
        )
    }

    #[test]
    fn the_label_and_the_flags_can_never_disagree() {
        for s in [
            Scope::workspace(),
            Scope::crate_only("aterm-pty"),
            Scope::packages(&["aterm-grid".into(), "aterm-gui".into()]),
            changed(&["aterm-pty"]),
            changed(&["aterm-grid", "aterm-gui", "aterm-cli"]),
        ] {
            assert_eq!(s.label(), s.args().join(" "));
        }
        assert_eq!(
            Scope::packages(&["a".into(), "b".into()]).args(),
            ["-p", "a", "-p", "b"]
        );
        assert_eq!(Scope::packages(&[]), Scope::Workspace);
    }

    #[test]
    fn the_empty_cone_says_so_and_falls_back_wider_never_narrower() {
        let s = changed(&[]);
        assert!(s.selects_nothing());
        assert_eq!(s.label(), "<no crates selected>");
        assert_eq!(s.args(), ["--workspace"]);
        assert!(!s.is_workspace());
    }

    #[test]
    fn a_selection_includes_exactly_what_it_names() {
        assert!(Scope::workspace().includes_crate("aterm-link"));
        assert!(Scope::crate_only("aterm-link").includes_crate("aterm-link"));
        assert!(!Scope::crate_only("aterm-grid").includes_crate("aterm-link"));
        assert!(changed(&["aterm-grid", "aterm-link"]).includes_crate("aterm-link"));
        assert!(!changed(&[]).includes_crate("aterm-link"));
    }

    #[test]
    fn only_the_cone_can_report_that_nothing_has_a_library() {
        assert!(Scope::workspace().has_lib_target());
        assert!(Scope::crate_only("aterm-lab").has_lib_target());
        assert!(!Scope::changed("main", vec!["aterm-lab".into()], false).has_lib_target());
    }
}
