// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the checks that read a selection (lint, test, doc) build and
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

    /// Would this selection compile the bare package `name`? A supported
    /// `name@version` spec identifies the same package; its original text is
    /// retained by the driver arguments.
    #[must_use]
    pub fn includes_crate(&self, name: &str) -> bool {
        match self {
            Self::Workspace => true,
            _ => self.crates().iter().any(|spec| {
                spec == name
                    || spec
                        .split_once('@')
                        .is_some_and(|(package, version)| package == name && !version.is_empty())
            }),
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
        assert!(Scope::crate_only("aterm-lab").has_lib_target());
        assert!(!Scope::changed("main", vec!["aterm-lab".into()], false).has_lib_target());
    }
}
