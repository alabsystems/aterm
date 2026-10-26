// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Converge items for `aterm pkg doctor` checks ([`aterm_converge::Item`]).

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy};

pub mod repair;

/// Each check id `aterm pkg doctor` reports, as a constant, and every one of them in [`IDS`],
/// so a word is checked against them without reading the machine ([`names`]).
macro_rules! ids {
    ($($(#[$doc:meta])* $name:ident = $id:literal;)*) => {
        $($(#[$doc])* pub const $name: &str = $id;)*
        /// Every check id above, in order.
        pub const IDS: &[&str] = &[$($name),*];
    };
}

ids! {
    /// Which atpkg is speaking.
    ATPKG = "base.pkg.atpkg";
    /// The aterm app's own update posture.
    APP = "base.pkg.aterm-app";
    /// The index source.
    INDEX_SOURCE = "base.pkg.index-source";
    /// The trust root compiled in.
    TRUST_ROOT = "base.pkg.trust-root";
    /// The `[packages]` table's own spelling.
    CONFIG = "base.pkg.config";
    /// A retired opt-out still exported, `@<variable>`.
    RETIRED_ENV = "base.pkg.env";
    /// The prefix.
    PREFIX = "base.pkg.prefix";
    /// The managed `bin/` on PATH.
    PATH = "base.pkg.path";
    /// The reroute stubs, abrew's `brew` included, `@<name>`.
    STUBS = "base.pkg.stubs";
    /// The reroute directory's place on PATH.
    REROUTE = "base.pkg.reroute";
    /// A bin/ shim, `@<name>`.
    SHIMS = "base.pkg.shims";
    /// The exposed names an active build was refused a shim.
    REFUSED = "base.pkg.refused";
    /// A shim an older atpkg laid under a name no shim may carry now, `@<name>`.
    REFUSED_SHIM = "base.pkg.refused.shim";
    /// The active builds' store trees.
    STORE = "base.pkg.store";
    /// A solver the trust bundle pins privately, `@<program>`.
    SOLVER = "base.pkg.solver";
    /// The trust bundle's compiler as tippy needs it.
    TRUST_COMPILER = "base.pkg.trust.compiler";
    /// A stock name in the rustup view, `@<seam>.<name>`.
    RUSTUP_VIEW = "base.pkg.rustup.view";
    /// A directory on the trust build's chain `targo trust` refuses, `@<dir>`.
    TRUST_CHAIN = "base.pkg.trust.chain";
    /// The trust build's exec root.
    EXEC_ROOTS = "base.pkg.exec-roots";
    /// An exec root no build needs, `@<path>`.
    EXEC_ROOT_STRAY = "base.pkg.exec-roots.stray";
    /// The workspace's `[trust]` policy against the installed targo.
    WORKSPACE = "base.pkg.workspace";
    /// A local toolchain behind rustup's `trust` channel.
    RUSTUP_LOCAL = "base.pkg.rustup.local";
    /// The held toolchain update.
    HELD_UPDATE = "base.pkg.held-update";
    /// The build leases.
    LEASES = "base.pkg.leases";
    /// The live builds.
    LIVE = "base.pkg.live";
    /// The shell.d hooks.
    HOOKS = "base.shell.hooks";
    /// A POSIX `.sh` in shell.d, which breaks fish, `@<file>`.
    FISH_SAFE = "base.shell.fish-safe";
    /// The rc files' wiring.
    RC = "base.shell.rc";
    /// A copied command at a command path, `@<path>`.
    COPIED_COMMANDS = "base.shell.copied-commands";
    /// A login-sourced directory others can write, `@<dir>`.
    PRIVATE = "base.shell.private";
    /// Disk headroom.
    DISK = "base.pkg.disk";
    /// The Developer Tools list.
    DEVTOOLS = "base.privacy.devtools";
    /// Universal Control.
    UNIVERSAL_CONTROL = "base.machine.universal-control";
    /// Rosetta 2, on Apple silicon.
    ROSETTA = "base.machine.rosetta";
    /// The last completed update pass.
    INDEX_AGE = "base.pkg.index-age";
    /// Whether the signed index has been reached lately.
    INDEX_REACHED = "base.pkg.index-reached";
    /// Whether publishing looks frozen upstream.
    INDEX_FROZEN = "base.pkg.index-frozen";
    /// The accepted index build and roster generation.
    INDEX_FLOOR = "base.pkg.index-floor";
    /// The roster generation, when the floor row does not name it.
    ROSTER_FLOOR = "base.pkg.roster-floor";
    /// Whether the accepted index is the newest.
    INDEX_HEAD = "base.pkg.index-head";
    /// Who watches the vendor heads.
    VENDOR_WATCH = "base.pkg.vendor-watch";
    /// rustup.
    RUSTUP = "base.pkg.rustup";
    /// rustup's `trust` entry against the managed seam.
    RUSTUP_SEAM = "base.pkg.rustup.seam";
    /// A stale `trust` link the machine replaced.
    RUSTUP_REPLACED = "base.pkg.rustup.replaced";
    /// A record row that cannot be a program, `@<key>`.
    RECORD_STRAY = "base.pkg.record.stray";
    /// A member satisfied by the system, `@<program>`.
    SYSTEM = "base.pkg.system";
    /// A member held on its current build, `@<program>`.
    HELD = "base.pkg.held";
    /// A managed member and what PATH runs for it, `@<program>`.
    SHADOW = "base.pkg.shadow";
    /// A managed member's shim environment, `@<program>`.
    SHIM_ENV = "base.pkg.shim-env";
    /// Claude Code's defaults.
    CLAUDE_DEFAULTS = "base.agents.claude-defaults";
    /// A dev link.
    DEV_LINK = "base.pkg.dev-link";
    /// The toolset against the signed index.
    TOOLSET = "base.pkg.toolset";
    /// The toolset on a platform no build is published for.
    TOOLSET_PLATFORM = "base.pkg.toolset.platform";
    /// A recorded problem the toolset row lists, `@<program>`.
    TOOLSET_MEMBER = "base.pkg.toolset.member";
    /// A member removed on purpose, `@<program>`.
    REMOVED = "base.pkg.removed";
    /// The C toolchain.
    CLT = "base.clt";
}

/// The `detail` of a row whose warn says a managed tool cannot run.
pub const CANNOT_RUN: &str = "cannot-run";
/// The `detail` of a row whose warn says a managed tool may not run, or cannot tell.
pub const UNPROVEN: &str = "unproven";

/// `id` for one subject of a check (`base.pkg.stubs@cargo`).
#[must_use]
pub fn of(id: &str, subject: impl std::fmt::Display) -> String {
    format!("{id}@{subject}")
}

/// Whether the `--only` word `token` can name one of `aterm pkg doctor`'s rows: one of
/// [`IDS`] or a subject of one (`base.pkg.stubs@cargo`), their family or the base layer.
/// Read from the table alone, so a word is judged without reading the machine.
#[must_use]
pub fn names(token: &str) -> bool {
    let item = token.split_once('@').map_or(token, |(item, _)| item);
    IDS.iter().any(|id| {
        *id == item || aterm_converge::selects(token, id, family_of(id), Some(Layer::Base))
    })
}

/// Whether `row_id` is the item `id` or one of its subjects (`id@…`).
#[must_use]
pub fn is(row_id: &str, id: &str) -> bool {
    row_id
        .strip_prefix(id)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('@'))
}

/// One check's reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable id, unique within a report.
    pub id: String,
    /// One human line naming the setting.
    pub title: String,
    /// What fixes it, and so which surface would.
    pub class: Class,
    /// Whether, left wrong, it fails `aterm pkg doctor`.
    pub essential: bool,
    /// The reading.
    pub probe: Probe,
    /// The row's sentence and its one remedy.
    pub explain: Explain,
    /// What a caller acts on: the active count on the toolset row, or a tool claim
    /// ([`CANNOT_RUN`], [`UNPROVEN`]) on a warn about a managed tool.
    pub detail: Option<String>,
}

impl Finding {
    fn new(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        probe: Probe,
        why: String,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            class,
            essential: false,
            probe,
            explain: Explain {
                why,
                remedy: Remedy::None,
            },
            detail: None,
        }
    }

    /// As it should be; `now` is the row's sentence.
    pub fn ok(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        now: impl Into<String>,
    ) -> Self {
        let now = now.into();
        Self::new(id, class, title, Probe::Ok { now: now.clone() }, now)
    }

    /// A fact: nothing to do, nothing wrong.
    pub fn note(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        let why = why.into();
        Self::new(
            id,
            class,
            title,
            Probe::NotApplicable { why: why.clone() },
            why,
        )
    }

    /// Waits for something that ends by itself (a quiet machine, a window, a run).
    pub fn waits(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        let why = why.into();
        Self::new(id, class, title, Probe::Deferred { why: why.clone() }, why)
    }

    /// Could not be read.
    pub fn unread(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        let why = why.into();
        Self::new(id, class, title, Probe::Unusable { why: why.clone() }, why)
    }

    /// Only a person changes it (System Settings).
    pub fn person(
        id: impl Into<String>,
        title: impl Into<String>,
        why: impl Into<String>,
        remedy: Remedy,
    ) -> Self {
        let id = id.into();
        let why = why.into();
        let probe = Probe::Person {
            step: id.clone(),
            how: why.clone(),
        };
        Self::new(id, Class::Person, title, probe, why).remedy(remedy)
    }

    /// Not as wanted: `why` is the row's sentence.
    pub fn change(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        why: impl Into<String>,
        want: impl Into<String>,
        remedy: Remedy,
    ) -> Self {
        let why = why.into();
        let from = why[..first_clause_end(&why)]
            .trim_end()
            .trim_end_matches('.')
            .to_owned();
        let probe = Probe::Change {
            from,
            to: want.into(),
        };
        Self::new(id, class, title, probe, why).remedy(remedy)
    }

    /// Essential: left wrong, it fails the report.
    #[must_use]
    pub const fn essential(mut self) -> Self {
        self.essential = true;
        self
    }

    /// With this remedy.
    #[must_use]
    pub fn remedy(mut self, remedy: Remedy) -> Self {
        self.explain.remedy = remedy;
        self
    }

    /// With this detail.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// From a typed doctor row ([`crate::doctor::Row`]).
    pub fn of_row(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        row: crate::doctor::Row,
        want: impl Into<String>,
        remedy: Remedy,
    ) -> Self {
        match row.level {
            crate::doctor::Level::Ok => Self::ok(id, class, title, row.text),
            crate::doctor::Level::Note => Self::note(id, class, title, row.text),
            crate::doctor::Level::Warn => Self::change(id, class, title, row.text, want, remedy),
        }
    }
}

/// Where a sentence's first clause ends ([`crate::doctor::render`]).
#[must_use]
pub fn first_clause_end(text: &str) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '`' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0
            && !quoted
            && [" \u{2014} ", "; ", ". "]
                .iter()
                .any(|sep| text[at..].starts_with(sep))
        {
            return at;
        }
    }
    text.len()
}

fn family_of(id: &str) -> &'static str {
    match id
        .split('.')
        .nth(1)
        .map(|s| s.split('@').next().unwrap_or(s))
    {
        Some("shell") => "shell",
        Some("agents") => "agents",
        Some("privacy") => "privacy",
        Some("machine") => "machine",
        Some("clt") => "developer",
        _ => "pkg",
    }
}

/// A [`Finding`] as an item.
#[derive(Debug)]
pub struct Check(Finding);

impl Item for Check {
    fn id(&self) -> String {
        self.0.id.clone()
    }

    fn family(&self) -> &'static str {
        family_of(&self.0.id)
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> Class {
        self.0.class
    }

    fn title(&self) -> String {
        self.0.title.clone()
    }

    fn probe(&self) -> Probe {
        self.0.probe.clone()
    }

    fn apply(&self) -> Result<(), String> {
        Err(format!(
            "`aterm pkg doctor` only reads; {} is fixed by its remedy",
            self.0.id
        ))
    }

    fn explain(&self, _: &Probe) -> Explain {
        self.0.explain.clone()
    }

    fn essential(&self) -> bool {
        self.0.essential && !matches!(self.0.probe, Probe::Unusable { .. })
    }

    fn detail(&self) -> Option<String> {
        self.0.detail.clone()
    }
}

/// The findings as items, each id unique.
#[must_use]
pub fn items(findings: Vec<Finding>) -> Vec<Box<dyn Item>> {
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    findings
        .into_iter()
        .map(|mut finding| {
            let n = seen.entry(finding.id.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                finding.id = if finding.id.contains('@') {
                    format!("{}.{n}", finding.id)
                } else {
                    of(&finding.id, n)
                };
            }
            Box::new(Check(finding)) as Box<dyn Item>
        })
        .collect()
}

/// The doctor's rows.
#[must_use]
pub fn report(findings: Vec<Finding>) -> Vec<ItemReport> {
    let items = items(findings);
    match aterm_converge::run(&items, Mode::Plan, &mut aterm_converge::ReadOnly) {
        Ok(rows) => rows,
        Err(refusal) => vec![ItemReport {
            id: ATPKG.to_owned(),
            family: "pkg".to_owned(),
            title: "the doctor's own items".to_owned(),
            outcome: Outcome::Unknown,
            from: None,
            to: None,
            why: Some(refusal.to_string()),
            revert: None,
            effect: aterm_converge::Effect::Now,
            detail: None,
            layer: Some(Layer::Base),
            class: Some(Class::Relay),
            explain: Some(Explain {
                why: format!("the doctor could not order its items: {refusal}"),
                remedy: Remedy::None,
            }),
            stood_aside: None,
            essential: true,
            elapsed_ms: Some(0),
        }],
    }
}

/// Whether the rows pass `aterm pkg doctor`'s rule (exit 0).
#[must_use]
pub fn passes(rows: &[ItemReport]) -> bool {
    !rows.iter().any(ItemReport::fails_surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finding_reports_as_its_item_and_only_an_essential_change_fails() {
        let rows = report(vec![
            Finding::ok(
                PATH,
                Class::Person,
                "the managed bin/ on PATH",
                "managed bin/ is on PATH",
            ),
            Finding::change(
                of(SHIMS, "cargo"),
                Class::Relay,
                "the bin/ shim cargo",
                "broken bin shim /p/bin/cargo — its build is gone",
                "forwarding to a build in the store",
                Remedy::Repair,
            )
            .essential(),
            Finding::change(
                of(STUBS, "cargo"),
                Class::Relay,
                "the reroute stub cargo",
                "reroute stub cargo missing; the next aterm session lays it",
                "laid",
                Remedy::Repair,
            ),
            Finding::unread(
                DISK,
                Class::Person,
                "disk headroom",
                "could not query free space",
            )
            .essential(),
            Finding::note(RUSTUP, Class::Person, "rustup", "no rustup on PATH"),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                PATH,
                "base.pkg.shims@cargo",
                "base.pkg.stubs@cargo",
                DISK,
                RUSTUP
            ]
        );
        let failing: Vec<&str> = rows
            .iter()
            .filter(|r| r.fails_surface())
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(
            failing,
            ["base.pkg.shims@cargo"],
            "the advisory and the unread row do not fail"
        );
        assert!(!passes(&rows));
        assert_eq!(
            rows[1].from.as_deref(),
            Some("broken bin shim /p/bin/cargo")
        );
        assert_eq!(
            rows[1].explain.as_ref().map(|e| &e.remedy),
            Some(&Remedy::Repair)
        );
        assert_eq!(rows[0].to.as_deref(), Some("managed bin/ is on PATH"));
        assert_eq!(rows[0].family, "pkg");
        assert!(rows.iter().all(|r| r.layer == Some(Layer::Base)));
        assert!(passes(&rows[2..]));
    }

    #[test]
    fn a_subject_found_twice_keeps_both_rows_under_distinct_ids() {
        let rows = report(vec![
            Finding::note(CONFIG, Class::Person, "the [packages] table", "a"),
            Finding::note(CONFIG, Class::Person, "the [packages] table", "b"),
            Finding::note(of(STUBS, "x"), Class::Relay, "the reroute stub x", "c"),
            Finding::note(of(STUBS, "x"), Class::Relay, "the reroute stub x", "d"),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                CONFIG,
                "base.pkg.config@2",
                "base.pkg.stubs@x",
                "base.pkg.stubs@x.2"
            ]
        );
    }

    #[test]
    fn an_id_names_its_item_and_its_subjects_only() {
        assert!(is("base.pkg.store", STORE));
        assert!(is("base.pkg.store@ay", STORE));
        assert!(!is("base.pkg.store.shim@ay", STORE));
        assert!(!is("base.pkg.stores", STORE));
        assert_eq!(family_of("base.shell.rc@.zshrc"), "shell");
        assert_eq!(family_of(CLT), "developer");
        assert_eq!(
            family_of("base.agents.claude-defaults@effortLevel"),
            "agents"
        );
    }
}
