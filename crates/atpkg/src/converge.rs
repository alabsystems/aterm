// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! atpkg's converge items (docs/DESIGN-init-system-2026-10-20.md §4.2, S4): `aterm pkg
//! doctor`'s checks (0)–(11) as [`aterm_converge::Item`]s.
//!
//! The doctor reads the machine once ([`crate::doctor::read`]): every check leaves one
//! [`Finding`] per thing it found, under a stable id (`base.pkg.path`,
//! `base.pkg.stubs@cargo`). [`items`] makes each an item whose probe is that reading, and
//! [`report`] runs them through the engine in [`aterm_converge::Mode::Plan`] on a
//! [`aterm_converge::ReadOnly`] journal, so the doctor stays a no-mutation verb. The rows
//! it returns are the one schema `--json` prints ([`aterm_converge::Report`]), and the
//! doctor's text is rendered from them ([`crate::doctor::render`]); nothing re-reads the
//! text.
//!
//! An item is essential when, left wrong, it fails `aterm pkg doctor` (exit 1): a broken
//! shim, a store tree gone, a fish-breaking hook, a world-writable login-sourced
//! directory, a workspace the installed targo refuses, a diverged live build, a dead dev
//! link, the C toolchain, and the toolset itself. Everything else is advisory. A read that
//! could not be made is never essential: the doctor never fails because a probe failed.

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Mode, Outcome, Probe, Remedy};

pub mod repair;

/// (0) Which atpkg is speaking.
pub const ATPKG: &str = "base.pkg.atpkg";
/// (0) The aterm app's own update posture.
pub const APP: &str = "base.pkg.aterm-app";
/// (1) The index source.
pub const INDEX_SOURCE: &str = "base.pkg.index-source";
/// (1) The trust root compiled in.
pub const TRUST_ROOT: &str = "base.pkg.trust-root";
/// (1) The `[packages]` table's own spelling.
pub const CONFIG: &str = "base.pkg.config";
/// (1) A retired opt-out still exported, `@<variable>`.
pub const RETIRED_ENV: &str = "base.pkg.env";
/// (2) The prefix.
pub const PREFIX: &str = "base.pkg.prefix";
/// (3) The managed `bin/` on PATH.
pub const PATH: &str = "base.pkg.path";
/// (3b) The reroute stubs, abrew's `brew` included, `@<name>`.
pub const STUBS: &str = "base.pkg.stubs";
/// (3b) The reroute directory's place on PATH.
pub const REROUTE: &str = "base.pkg.reroute";
/// (4) A bin/ shim, `@<name>`: essential when its target is gone.
pub const SHIMS: &str = "base.pkg.shims";
/// (4b) The exposed names an active build was refused a shim (design S13): one row, or
/// `@<program>` for each program the record names.
pub const REFUSED: &str = "base.pkg.refused";
/// (4) A shim an older atpkg laid under a name no shim may carry now, `@<name>`.
pub const REFUSED_SHIM: &str = "base.pkg.refused.shim";
/// (5) The active builds' store trees; `@<program>` for one that is missing or incomplete.
pub const STORE: &str = "base.pkg.store";
/// (5c) A solver the trust bundle pins privately, `@<program>`.
pub const SOLVER: &str = "base.pkg.solver";
/// (5f)(a) The trust bundle's compiler as tippy needs it.
pub const TRUST_COMPILER: &str = "base.pkg.trust.compiler";
/// (5f)(b) A stock name in the rustup view, `@<seam>.<name>`.
pub const RUSTUP_VIEW: &str = "base.pkg.rustup.view";
/// (5f)(g) A directory on the trust build's chain `targo trust` refuses, `@<dir>`.
pub const TRUST_CHAIN: &str = "base.pkg.trust.chain";
/// (5f)(c)(f) The trust build's exec root.
pub const EXEC_ROOTS: &str = "base.pkg.exec-roots";
/// (5f)(d) An exec root no build needs, `@<path>`.
pub const EXEC_ROOT_STRAY: &str = "base.pkg.exec-roots.stray";
/// (5d) The workspace's `[trust]` policy against the installed targo: essential.
pub const WORKSPACE: &str = "base.pkg.workspace";
/// (5e) A local toolchain behind rustup's `trust` channel.
pub const RUSTUP_LOCAL: &str = "base.pkg.rustup.local";
/// The held toolchain update.
pub const HELD_UPDATE: &str = "base.pkg.held-update";
/// The build leases.
pub const LEASES: &str = "base.pkg.leases";
/// (5b) The live builds; `@<program>` for one whose witness disagrees.
pub const LIVE: &str = "base.pkg.live";
/// (6) The shell.d hooks.
pub const HOOKS: &str = "base.shell.hooks";
/// (6) A POSIX `.sh` in shell.d, which breaks fish, `@<file>`: essential.
pub const FISH_SAFE: &str = "base.shell.fish-safe";
/// (6b) The rc files' wiring; `@<file>` for one file's own state.
pub const RC: &str = "base.shell.rc";
/// (6c) A copied command at a command path, `@<path>`.
pub const COPIED_COMMANDS: &str = "base.shell.copied-commands";
/// (6) A login-sourced directory others can write, `@<dir>`: essential.
pub const PRIVATE: &str = "base.shell.private";
/// (7) Disk headroom.
pub const DISK: &str = "base.pkg.disk";
/// (7b) The Developer Tools list.
pub const DEVTOOLS: &str = "base.privacy.devtools";
/// (7c) Universal Control.
pub const UNIVERSAL_CONTROL: &str = "base.machine.universal-control";
/// (7d) Rosetta 2, on Apple silicon.
pub const ROSETTA: &str = "base.machine.rosetta";
/// (8) The last completed update pass.
pub const INDEX_AGE: &str = "base.pkg.index-age";
/// (8) Whether the signed index has been reached lately.
pub const INDEX_REACHED: &str = "base.pkg.index-reached";
/// (8) Whether publishing looks frozen upstream.
pub const INDEX_FROZEN: &str = "base.pkg.index-frozen";
/// (8) The accepted index build and roster generation.
pub const INDEX_FLOOR: &str = "base.pkg.index-floor";
/// (8) The roster generation, when the floor row does not name it.
pub const ROSTER_FLOOR: &str = "base.pkg.roster-floor";
/// (8) Whether the accepted index is the newest.
pub const INDEX_HEAD: &str = "base.pkg.index-head";
/// (8c) Who watches the vendor heads.
pub const VENDOR_WATCH: &str = "base.pkg.vendor-watch";
/// (9) rustup.
pub const RUSTUP: &str = "base.pkg.rustup";
/// (9) rustup's `trust` entry against the managed seam.
pub const RUSTUP_SEAM: &str = "base.pkg.rustup.seam";
/// (9) A stale `trust` link the machine replaced.
pub const RUSTUP_REPLACED: &str = "base.pkg.rustup.replaced";
/// (10) A record row that cannot be a program, `@<key>`.
pub const RECORD_STRAY: &str = "base.pkg.record.stray";
/// (10a) A member satisfied by the system, `@<program>`.
pub const SYSTEM: &str = "base.pkg.system";
/// (10g) A member held on its current build, `@<program>`.
pub const HELD: &str = "base.pkg.held";
/// (10d) A managed member and what PATH runs for it, `@<program>`.
pub const SHADOW: &str = "base.pkg.shadow";
/// (10f) A managed member's shim environment, `@<program>`.
pub const SHIM_ENV: &str = "base.pkg.shim-env";
/// (10h) Claude Code's defaults; `@<key>` for one key's own state.
pub const CLAUDE_DEFAULTS: &str = "base.agents.claude-defaults";
/// (11) A dev link: essential when it no longer runs.
pub const DEV_LINK: &str = "base.pkg.dev-link";
/// (11) The toolset against the signed index: essential.
pub const TOOLSET: &str = "base.pkg.toolset";
/// (11a) The toolset on a platform no build is published for.
pub const TOOLSET_PLATFORM: &str = "base.pkg.toolset.platform";
/// (11) A recorded problem the toolset row lists, `@<program>`.
pub const TOOLSET_MEMBER: &str = "base.pkg.toolset.member";
/// (11) A member removed on purpose, `@<program>`.
pub const REMOVED: &str = "base.pkg.removed";
/// (11) The C toolchain: essential when it answers that it cannot build.
pub const CLT: &str = "base.clt";

/// The `detail` of a row whose warn says a managed tool cannot run.
pub const CANNOT_RUN: &str = "cannot-run";
/// The `detail` of a row whose warn says a managed tool may not run, or cannot tell.
pub const UNPROVEN: &str = "unproven";

/// `id` for one subject of a check (`base.pkg.stubs@cargo`).
#[must_use]
pub fn of(id: &str, subject: impl std::fmt::Display) -> String {
    format!("{id}@{subject}")
}

/// Whether `row_id` is the item `id` or one of its subjects (`id@…`).
#[must_use]
pub fn is(row_id: &str, id: &str) -> bool {
    row_id
        .strip_prefix(id)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('@'))
}

/// One check's reading: what the doctor's read pass found, and what the item reports.
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
    /// What a caller acts on: the active count on the toolset row, a tool claim
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

    /// As it should be; `now` is the row's sentence. Every constructor takes the class of
    /// what would fix the item (design §3.2), so no row is `relay` (the one class an
    /// unattended lane applies) unless its check says so.
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

    /// Could not be read: said, never essential.
    pub fn unread(
        id: impl Into<String>,
        class: Class,
        title: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        let why = why.into();
        Self::new(id, class, title, Probe::Unusable { why: why.clone() }, why)
    }

    /// Only a person changes it (System Settings): class `person`.
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

    /// Not as wanted: `why` is the row's sentence, whose first clause is the value now, and
    /// `want` the value wanted.
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

    /// From a typed doctor row ([`crate::doctor::Row`]): `ok`, a `note`, or a `warn` that
    /// plans `want` by `remedy`.
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

/// Where a sentence's first clause ends: at its first ` — `, `; ` or `. ` outside
/// parentheses and backticks (a parenthetical's `;` is not the end of the fact), else the
/// whole sentence. A check's own sentence, read when the finding is made (a row's `from`)
/// and when its short form is cut ([`crate::doctor::render`]).
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

/// The family a doctor id belongs to: its second segment (`base.shell.rc` → `shell`), the
/// Command Line Tools' `developer` as the machines family has it.
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

/// A [`Finding`] as an item: its probe is the reading, its explanation the finding's own.
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
        // A read that could not be made never fails the doctor.
        self.0.essential && !matches!(self.0.probe, Probe::Unusable { .. })
    }

    fn detail(&self) -> Option<String> {
        self.0.detail.clone()
    }
}

/// The findings as items, each id unique: a check that found the same subject twice keeps
/// both, the later one numbered (`base.pkg.config@2`).
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

/// The doctor's rows: the findings through the engine, planned and never applied.
#[must_use]
pub fn report(findings: Vec<Finding>) -> Vec<ItemReport> {
    let items = items(findings);
    match aterm_converge::run(&items, Mode::Plan, &mut aterm_converge::ReadOnly) {
        Ok(rows) => rows,
        // No doctor item names another, so nothing can refuse the order; were one to,
        // the report fails rather than says less.
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
        }],
    }
}

/// Whether the rows pass `aterm pkg doctor`'s rule (exit 0): no row fails it.
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
