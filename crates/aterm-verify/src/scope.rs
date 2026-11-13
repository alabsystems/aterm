// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the checks that read a selection (lint, test, doc; `cells` under `-p`
//! or pulled) build and test: the whole workspace (`--all`), the packages `-p` named, or the change
//! cone ([`crate::changed`]). The driver flags and the label derive from one
//! value, so a check can never print `--workspace` while running `-p`.

/// The change cone: the crates whose files changed since `base`, what builds on them over normal
/// and build edges, and the dev dependents of those. A dev edge is the last hop, so a normal
/// dependent of a crate the cone holds by a dev edge is not in it ([`crate::changed::Reach`]).
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
    /// The packages the change itself touched, by `-p` spec: a member's files, a
    /// `Cargo.lock` or `[workspace.dependencies]` row, a vendored fork; and the members whose
    /// library, binaries, build script or unplaced files spell a changed path of another member
    /// ([`crate::changed::select`]). Not all are in `crates`; the rest of `crates` is
    /// there only because it depends on one, a test of its reads a changed file, or its tests or
    /// doc comments alone spell one.
    pub seeds: Vec<String>,
    /// The packages, by name, whose test, bench and example targets the cone's lint compiles
    /// ([`Selection::Narrowed`](crate::changed::Selection::Narrowed)'s `all_targets`).
    pub all_targets: Vec<String>,
    /// The packages, by name, the cone's lint leaves out
    /// ([`Selection::Narrowed`](crate::changed::Selection::Narrowed)'s `unlinted`).
    pub unlinted: Vec<String>,
    /// The packages, by name, one edge from what the change touched
    /// ([`Selection::Narrowed`](crate::changed::Selection::Narrowed)'s `direct_dependents`). With
    /// [`Changed::all_targets`], they are what the cone's required-features lint compiles
    /// ([`Scope::gated_spec_of`]).
    pub direct_dependents: Vec<String>,
    /// The packages among [`Changed::seeds`], by name, whose change is confined to the package
    /// itself, its own tests and comments no macro reads among them, whose reads of another
    /// package's changed file sit only in its tests, benches and examples, and that observe no
    /// changed path of another member through code a dependent links, or, where they do, whose
    /// paths no member observes and none of which is a reader, so main judged them alone too
    /// ([`Selection::Narrowed`](crate::changed::Selection::Narrowed)'s `alone`): no build of a
    /// dependent reads it ([`Scope::reaches_dependents`]).
    pub alone: Vec<String>,
}

impl Changed {
    /// The selected crates the lint compiles, by `-p` spec: all but [`Changed::unlinted`].
    pub fn linted(&self) -> impl Iterator<Item = &String> {
        self.crates.iter().filter(|spec| {
            spec_name(spec).is_none_or(|name| !self.unlinted.iter().any(|n| n == name))
        })
    }
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
    pub const fn workspace() -> Self {
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

    /// A cone that knows no more than its crates: each counts as touched and has all its targets
    /// linted, what asks the most of the checks that read [`Changed::seeds`],
    /// [`Changed::all_targets`] and [`Changed::unlinted`]. [`crate::changed::cone`] says which they
    /// are.
    #[must_use]
    pub fn changed(base: impl Into<String>, crates: Vec<String>, any_lib: bool) -> Self {
        Self::Changed(Changed {
            base: base.into(),
            seeds: crates.clone(),
            all_targets: crates
                .iter()
                .filter_map(|spec| spec_name(spec).map(str::to_string))
                .collect(),
            unlinted: Vec::new(),
            direct_dependents: Vec::new(),
            alone: Vec::new(),
            crates,
            any_lib,
        })
    }

    /// This cone with the packages the change touched ([`Changed::seeds`]); any other scope as it
    /// is.
    #[must_use]
    pub fn with_seeds(mut self, seeds: Vec<String>) -> Self {
        if let Self::Changed(changed) = &mut self {
            changed.seeds = seeds;
        }
        self
    }

    /// This cone with the packages whose every target the lint compiles
    /// ([`Changed::all_targets`]); any other scope as it is.
    #[must_use]
    pub fn with_all_targets(mut self, all_targets: Vec<String>) -> Self {
        if let Self::Changed(changed) = &mut self {
            changed.all_targets = all_targets;
        }
        self
    }

    /// This cone with the packages its lint leaves out ([`Changed::unlinted`]); any other scope as
    /// it is.
    #[must_use]
    pub fn with_unlinted(mut self, unlinted: Vec<String>) -> Self {
        if let Self::Changed(changed) = &mut self {
            changed.unlinted = unlinted;
        }
        self
    }

    /// This cone with the packages one edge from what the change touched
    /// ([`Changed::direct_dependents`]); any other scope as it is.
    #[must_use]
    pub fn with_direct_dependents(mut self, direct_dependents: Vec<String>) -> Self {
        if let Self::Changed(changed) = &mut self {
            changed.direct_dependents = direct_dependents;
        }
        self
    }

    /// This cone with the seeds whose change no build of a dependent reads ([`Changed::alone`]);
    /// any other scope as it is.
    #[must_use]
    pub fn with_alone(mut self, alone: Vec<String>) -> Self {
        if let Self::Changed(changed) = &mut self {
            changed.alone = alone;
        }
        self
    }

    #[must_use]
    pub const fn is_workspace(&self) -> bool {
        matches!(self, Self::Workspace)
    }

    /// The selection is empty: a cone no crate is in. The checks that compile ask
    /// before they build a command ([`Scope::args`]).
    #[must_use]
    pub const fn selects_nothing(&self) -> bool {
        matches!(self, Self::Changed(c) if c.crates.is_empty())
    }

    /// Does the selection hold a package with a library target? Yes unless the
    /// cone proved otherwise: an unanswerable question leaves doctests running.
    #[must_use]
    pub const fn has_lib_target(&self) -> bool {
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

    /// What the lint compiles: [`Scope::label`], but for the crates a cone's lint leaves out
    /// ([`Changed::linted`]), so its row never names a crate it did not run on.
    #[must_use]
    pub fn lint_label(&self) -> String {
        match self {
            Self::Changed(c) if !c.unlinted.is_empty() => c
                .linted()
                .flat_map(|n| ["-p", n.as_str()])
                .collect::<Vec<_>>()
                .join(" "),
            _ => self.label(),
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

    /// Did the change itself touch the package `name`, rather than a dependency of it? Only a cone
    /// can tell; for any other scope, whether the selection names it ([`Scope::includes_crate`]).
    #[must_use]
    pub fn touches(&self, name: &str) -> bool {
        match self {
            Self::Changed(changed) => changed
                .seeds
                .iter()
                .any(|seed| spec_name(seed) == Some(name)),
            _ => self.includes_crate(name),
        }
    }

    /// Did the change touch the package `name` ([`Scope::touches`]) where a build of a dependent
    /// reads it: not only in what is confined to it ([`Changed::alone`])?
    #[must_use]
    pub fn reaches_dependents(&self, name: &str) -> bool {
        self.touches(name)
            && !matches!(self, Self::Changed(changed) if changed.alone.iter().any(|n| n == name))
    }

    /// Would this selection compile the bare package `name`? A spec the cone
    /// names it by (`name@version`, a path spec) identifies the same package
    /// ([`Scope::spec_of`]); its original text is retained by the driver
    /// arguments.
    #[must_use]
    pub fn includes_crate(&self, name: &str) -> bool {
        self.spec_of(name).is_some()
    }

    /// The `-p` spelling this selection names the package `name` by: the
    /// cone's own spec for it — `name`, `name@<version>`, or the path spec
    /// `path+file://<dir>#name@<version>` for a member whose name the lock
    /// shares ([`crate::changed::Members::spec`]) — or `name` itself for the
    /// workspace. `None` when the selection would not compile it.
    #[must_use]
    pub fn spec_of(&self, name: &str) -> Option<String> {
        match self {
            Self::Workspace => Some(name.to_string()),
            _ => self
                .crates()
                .iter()
                .find(|spec| spec_name(spec) == Some(name))
                .cloned(),
        }
    }

    /// The `-p` spelling ([`Scope::spec_of`]) by which the lint compiles the bare package `name`'s
    /// `required-features` targets, or `None` when it leaves them out. The workspace and `-p`
    /// compile those of every package they select. A cone compiles those of the packages whose
    /// every target it lints ([`Changed::all_targets`]) and those one edge from what the change
    /// touched ([`Changed::direct_dependents`]): a feature-gated target or module names the items
    /// of a direct dependency where no other code of its package may (`native_motion_probe`, under
    /// aterm-gui's `bench-support`, called a method its own crate had narrowed to `cfg(test)`). GAP:
    /// a gated target that reaches the change only through a re-export of a crate it depends on
    /// waits for `-p` or `--all`.
    #[must_use]
    pub fn gated_spec_of(&self, name: &str) -> Option<String> {
        match self {
            Self::Changed(changed)
                if !changed
                    .all_targets
                    .iter()
                    .chain(&changed.direct_dependents)
                    .any(|n| n == name) =>
            {
                None
            }
            _ => self.spec_of(name),
        }
    }
}

/// The package name a `-p` spec names, read as the memory book reads a selection's crates for
/// their units' peaks ([`aterm_capacity::book::Records::unit_floor`]).
pub use aterm_capacity::book::spec_name;

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
