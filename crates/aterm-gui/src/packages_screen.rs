// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Packages read projection — the toolchain-package analogue of
//! [`crate::update_screen`]. ONE structured snapshot drives the native
//! `/packages` route: the host-owned [`PackagesService`] reduces off-thread
//! observations of the CO-LOCATED `atpkg`'s durable state (its `status.toml`,
//! dev-link markers, and inert/enabled posture) plus in-flight verb state, and
//! every Settings view renders the same [`PackagesProjection`] via revision
//! fan-out. Collection ([`collect_packages_status`]) always runs OFF the event
//! loop; the service itself is memory-only, mirroring the updater's
//! "no ledger reads on the UI thread" doctrine.

/// Which co-located `atpkg` verb is currently running. The busy gate
/// serializes UI-INITIATED verbs only (one queue keeps feedback honest); the
/// 6-hour background loop's `atpkg update` is a separate process that can run
/// concurrently — atpkg's own sha256/tree_root verify gates keep an interleaved
/// pass fail-closed (a torn stage fails loudly; nothing corrupt activates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesBusy {
    /// `atpkg update` — check/update every installed managed program.
    Check,
    /// `atpkg install --default-set` — bootstrap-install the signed default set.
    Install,
    /// `atpkg uninstall --all` — remove the whole managed toolset, reclaiming its disk.
    Uninstall,
    /// `atpkg install <name> --elevate=never` — one EXTRA, opted in by the Install
    /// control on its Packages row. atpkg records the opt-in marker itself
    /// (`<prefix>/optin/<name>`) before it installs, so the GUI never writes into the
    /// store; `--elevate=never` keeps a windowed child from ever waiting on a sudo
    /// prompt nobody can see (an extra needs no elevation anyway).
    InstallExtra,
    /// `atpkg install <name> --elevate=osascript`, one process per program in
    /// dependency order — the GUI door for the `needs admin` rows (Apple's Command
    /// Line Tools through `softwareupdate`, Homebrew's signed `.pkg`). macOS shows
    /// its own administrator dialog; the GUI never sees a password.
    InstallAdmin,
    /// `atpkg machine apply` — the `[machine]` host settings (Universal Control off,
    /// build output hidden from Spotlight), applied now from the Security page's
    /// "Apply now". A LOCAL verb: no store lock, no network, and it works with the
    /// package manager switched off. macOS only.
    MachineApply,
}

impl PackagesBusy {
    /// Whether the verb runs atpkg's `apply_machine_settings` at the top of its pass
    /// — mirrors atpkg's static list (`update`, `install --default-set`, `seed`, and
    /// `machine apply` itself; NOT `install <name>` or `uninstall --all`), pinned on
    /// the atpkg side by its source-scan test. The host re-reads the machine record
    /// when such a verb finishes, so the card confirms rather than assumes.
    pub(crate) fn applies_machine_settings(self) -> bool {
        matches!(self, Self::Check | Self::Install | Self::MachineApply)
    }

    fn completed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check completed",
            Self::Install => "ALab toolset install completed",
            Self::Uninstall => "ALab toolset removed",
            Self::InstallExtra => "Extra install completed",
            Self::InstallAdmin => "Admin install completed",
            Self::MachineApply => "Machine settings applied",
        }
    }

    fn failed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check failed",
            Self::Install => "ALab toolset install failed",
            Self::Uninstall => "ALab toolset removal failed",
            Self::InstallExtra => "Extra install failed",
            Self::InstallAdmin => "Admin install failed",
            Self::MachineApply => "Machine settings not applied",
        }
    }
}

/// The final result of the co-located `atpkg` process, distinct from the
/// synchronous UI-thread admission result. Keeping this typed result beside
/// the refreshed status report prevents an old successful `status.toml` from
/// masquerading as the outcome of a process that failed to launch or exited
/// non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackagesCommandOutcome {
    Succeeded {
        operation: PackagesBusy,
    },
    Failed {
        operation: PackagesBusy,
        message: String,
    },
}

impl PackagesCommandOutcome {
    fn operation(&self) -> PackagesBusy {
        match self {
            Self::Succeeded { operation } | Self::Failed { operation, .. } => *operation,
        }
    }

    fn headline(&self) -> &'static str {
        match self {
            Self::Succeeded { operation } => operation.completed_headline(),
            Self::Failed { operation, .. } => operation.failed_headline(),
        }
    }

    fn feedback(&self) -> String {
        match self {
            Self::Succeeded { operation } => operation.completed_headline().to_string(),
            Self::Failed { operation, message } => {
                format!("{}: {message}", operation.failed_headline())
            }
        }
    }
}

/// One off-thread worker completion. A silent refresh carries no command
/// outcome; a user verb must carry one matching the operation reserved by
/// [`PackagesService::begin`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesWorkerCompletion {
    pub(crate) report: PackagesStatusReport,
    pub(crate) command: Option<PackagesCommandOutcome>,
    /// The `atpkg machine: …` verdict sentence a [`PackagesBusy::MachineApply`]
    /// worker read off the child's stdout (`applied — …` / `nothing changed — …`),
    /// so the card and the feedback line can quote what the pass said rather than a
    /// generic headline. `None` for every other worker.
    pub(crate) machine_verdict: Option<String>,
}

impl PackagesWorkerCompletion {
    pub(crate) fn refresh(report: PackagesStatusReport) -> Self {
        Self {
            report,
            command: None,
            machine_verdict: None,
        }
    }

    /// Attach the machine-apply verdict sentence (see `machine_verdict`).
    pub(crate) fn with_machine_verdict(mut self, verdict: Option<String>) -> Self {
        self.machine_verdict = verdict;
        self
    }

    pub(crate) fn command(report: PackagesStatusReport, command: PackagesCommandOutcome) -> Self {
        Self {
            machine_verdict: None,
            report,
            command: Some(command),
        }
    }
}

/// One managed program's last-known state, projected read-only for the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProgramRow {
    pub(crate) name: String,
    /// The active build number, if one is installed.
    pub(crate) installed_build: Option<u64>,
    /// atpkg's state line, VERBATIM. For an index-listed program it is one of the
    /// canonical spellings of `atpkg::state` (`managed <build> — pinned by index <N>`,
    /// `system: <path> — not managed by aterm`, `managed <build> — SHADOWED by <path>`,
    /// `extra — not installed (opt in: …)`, `installed via <protocol>: <path>`,
    /// `needs admin — run: aterm pkg install <name>`, `unavailable on <target>: <hint>`,
    /// `blocked by <dep>: <dep state>`); faults keep their prefixed free text. The row
    /// paints this string as-is — the Packages page says exactly what the pass log,
    /// `doctor` and `which` say (`docs/TOOLCHAIN-PACKAGE-MANAGER.md` §17.2).
    pub(crate) state: String,
    /// The same line, parsed through `atpkg::state`'s own readers — never a second
    /// spelling. Drives the grouping and the controls; the text above drives the paint.
    pub(crate) kind: ProgramStateKind,
    /// Which Packages group the row belongs to.
    pub(crate) group: RowGroup,
    /// Vendor / license / size for an extra, from atpkg's authored roster line
    /// (`atpkg::stub::describe`). `None` for a default-set member or an extra this
    /// binary was published before.
    pub(crate) facts: Option<ExtraFacts>,
    /// Source annotation: `Some("dev-link → /path")` for a dev-linked checkout.
    pub(crate) annotation: Option<String>,
}

impl PackagesProgramRow {
    fn from_status(name: &str, program: &atpkg::ProgramStatus) -> Self {
        let kind = ProgramStateKind::parse(&program.state);
        let group = RowGroup::of(name, &kind);
        Self {
            name: name.to_string(),
            installed_build: program.installed_build,
            state: program.state.clone(),
            // The authored vendor · license · size line rides on every extra AND on the
            // agent programs (default-set, but still a vendor's proprietary tool).
            facts: (group == RowGroup::Extras || atpkg::stub::is_agent_program(name))
                .then(|| atpkg::stub::describe(name).map(ExtraFacts::parse))
                .flatten(),
            kind,
            group,
            annotation: None,
        }
    }

    /// The extras Install control belongs on an extra that is waiting for consent
    /// (`extra — not installed (opt in: aterm pkg install <name>)`) — never on one
    /// already installed, system-satisfied or unavailable here.
    pub(crate) fn offers_extra_install(&self) -> bool {
        self.group == RowGroup::Extras && matches!(self.kind, ProgramStateKind::ExtraNotInstalled)
    }
}

/// The three groups the Packages page renders (`docs/DESIGN-which-copy-runs-2026-08-27.md`
/// S9: "Packages lists extras separately with vendor, license, size, and an Install
/// control"; §17.8: the `needs admin` rows and the GUI door).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowGroup {
    /// A default-set member (or any row the index does not mark as an extra).
    Default,
    /// An opt-in extra: listed and pinned, installed only after consent.
    Extras,
    /// A row waiting on an administrator: its own `needs admin` state, or `blocked by`
    /// a dependency whose chain ends in one (Homebrew behind the Command Line Tools).
    NeedsAdmin,
}

impl RowGroup {
    fn of(name: &str, kind: &ProgramStateKind) -> Self {
        // The agent programs (`atpkg::stub::AGENT_PROGRAMS`: claude, codex) are
        // default-set members since the 2026-09-10 owner decision — installed by the
        // pass unasked, never behind an Install control — whatever a stale
        // `extra — not installed` row from an older atpkg still says.
        let agent = atpkg::stub::is_agent_program(name);
        if !agent
            && (matches!(kind, ProgramStateKind::ExtraNotInstalled)
                || atpkg::stub::compiled_extra(name))
        {
            // Needs-admin membership is decided over the WHOLE row set (a blocked
            // chain reaches other rows), in `PackagesStatusReport::from_parts`.
            Self::Extras
        } else if matches!(kind, ProgramStateKind::NeedsAdmin) {
            Self::NeedsAdmin
        } else {
            Self::Default
        }
    }

    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Default => "DEFAULT SET",
            Self::Extras => "EXTRAS",
            Self::NeedsAdmin => "NEEDS ADMIN",
        }
    }
}

/// A `status.toml` state line read through `atpkg::state`'s parsers — the SAME
/// readers `doctor` and `which` use, so this page cannot drift from what the pass
/// wrote. `Other` keeps every fault/legacy line (`active`, `error: …`, `linked`) as
/// a plain default-set row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStateKind {
    Managed {
        build: u64,
        index: u64,
    },
    Shadowed {
        build: u64,
        path: String,
    },
    System {
        path: String,
        retired: Option<String>,
    },
    ExtraNotInstalled,
    /// An agent program the pass has not installed YET (`agent program — installing`,
    /// `atpkg::state::AGENT_INSTALLING`): a default-set row on its way, never an
    /// extra waiting for consent.
    AgentInstalling,
    InstalledVia {
        protocol: String,
        path: String,
    },
    NeedsAdmin,
    Unavailable,
    BlockedBy {
        dep: String,
        dep_state: String,
    },
    Other,
}

impl ProgramStateKind {
    pub(crate) fn parse(state: &str) -> Self {
        if let Some((build, index)) = atpkg::state::managed_pin(state) {
            return Self::Managed { build, index };
        }
        if let Some((build, path)) = atpkg::state::shadowed_by(state) {
            return Self::Shadowed {
                build,
                path: path.to_string(),
            };
        }
        if let Some(path) = atpkg::state::system_path(state) {
            return Self::System {
                path: path.to_string(),
                retired: atpkg::state::system_retired(state).map(str::to_string),
            };
        }
        if state.starts_with(atpkg::state::AGENT_INSTALLING) {
            return Self::AgentInstalling;
        }
        if state.starts_with(atpkg::state::EXTRA_PREFIX) {
            return Self::ExtraNotInstalled;
        }
        if let Some((protocol, path)) = atpkg::state::installed_via_path(state) {
            return Self::InstalledVia {
                protocol: protocol.to_string(),
                path: path.to_string(),
            };
        }
        if state.starts_with(atpkg::state::NEEDS_ADMIN_PREFIX) {
            return Self::NeedsAdmin;
        }
        if state.starts_with(atpkg::state::UNAVAILABLE_PREFIX) {
            return Self::Unavailable;
        }
        if let Some((dep, dep_state)) = atpkg::state::blocked_by(state) {
            return Self::BlockedBy {
                dep: dep.to_string(),
                dep_state: dep_state.to_string(),
            };
        }
        Self::Other
    }
}

/// What an extra's row says beside its state: the vendor, the license and the size,
/// parsed from atpkg's one authored line per extra (`atpkg::stub::EXTRAS_STUB_NAMES`:
/// `"<vendor product> — <license>, <size>, downloaded from <host>"`). A line that does
/// not follow that grammar is kept whole as the vendor so nothing authored is lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtraFacts {
    pub(crate) vendor: String,
    pub(crate) license: Option<String>,
    pub(crate) size: Option<String>,
}

impl ExtraFacts {
    pub(crate) fn parse(line: &str) -> Self {
        let line = line.trim();
        let Some((vendor, rest)) = line.split_once(" \u{2014} ") else {
            return Self {
                vendor: line.to_string(),
                license: None,
                size: None,
            };
        };
        let mut parts = rest.split(',').map(str::trim).filter(|p| !p.is_empty());
        let license = parts.next().map(str::to_string);
        let size = parts
            .next()
            .filter(|p| p.contains("MB") || p.contains("GB") || p.contains("KB"))
            .map(str::to_string);
        Self {
            vendor: vendor.trim().to_string(),
            license,
            size,
        }
    }

    /// `"<vendor>  ·  <license>  ·  <size>"`, whichever facts the line carried.
    pub(crate) fn line(&self) -> String {
        let mut out = self.vendor.clone();
        for extra in [self.license.as_deref(), self.size.as_deref()]
            .into_iter()
            .flatten()
        {
            out.push_str("  \u{b7}  ");
            out.push_str(extra);
        }
        out
    }
}

/// The programs waiting on an administrator, in DEPENDENCY ORDER — the order the GUI
/// door installs them in (§17.10: `clt` before `brew`, which `requires` it).
///
/// A program waits on admin when its own row is `needs admin — run: aterm pkg install
/// <name>`, or when it is `blocked by <dep>: <dep state>` and following the `blocked by`
/// chain (each tail is the dependency's own row, quoted verbatim) ends at a `needs
/// admin` row. Dependencies come first (chain depth, then name); a cycle or a dangling
/// chain is simply not admin-waiting. Pure: the notice thread, the Packages page and
/// the tests all read the same rule.
pub(crate) fn needs_admin_order(rows: &[(String, String)]) -> Vec<String> {
    let states: std::collections::BTreeMap<&str, &str> = rows
        .iter()
        .map(|(name, state)| (name.as_str(), state.as_str()))
        .collect();
    let mut waiting: Vec<(usize, &str)> = Vec::new();
    for (name, state) in &states {
        // Walk the chain by NAME through the recorded rows, so the depth counts the
        // programs the door has to install first; the quoted tail is the fallback
        // when the dependency has no row of its own.
        let mut depth = 0usize;
        let mut current: String = (*state).to_string();
        let mut seen: Vec<String> = vec![(*name).to_string()];
        let admin = loop {
            if current.starts_with(atpkg::state::NEEDS_ADMIN_PREFIX) {
                break true;
            }
            let Some((dep, dep_state)) = atpkg::state::blocked_by(&current) else {
                break false;
            };
            if seen.iter().any(|s| s == dep) || depth >= rows.len() {
                break false;
            }
            seen.push(dep.to_string());
            depth += 1;
            current = states
                .get(dep)
                .map_or_else(|| dep_state.to_string(), |s| (*s).to_string());
        };
        if admin {
            waiting.push((depth, name));
        }
    }
    waiting.sort();
    waiting
        .into_iter()
        .map(|(_, name)| name.to_string())
        .collect()
}

/// The first-launch admin card's dismissal rule: "Not now" records the set of names it
/// was shown for, and the card is not raised again until that set CHANGES. `marker` is
/// the recorded text (`None` = never dismissed); the card shows for a non-empty set
/// whose recorded form differs from the marker.
pub(crate) fn admin_step_should_show(marker: Option<&str>, names: &[String]) -> bool {
    !names.is_empty() && marker.map(str::trim) != Some(admin_step_marker_text(names).as_str())
}

/// The recorded form of a needs-admin set: one name per line, dependency order kept
/// (the door's order is part of what the user declined).
pub(crate) fn admin_step_marker_text(names: &[String]) -> String {
    names.join("\n")
}

/// The dismissal marker beside `aterm.toml` (the config-dir latch idiom of
/// `connections::first_use_notice_should_show`; never inside atpkg's store, which is
/// atpkg's to write).
pub(crate) const ADMIN_STEP_MARKER: &str = "packages-admin-step-dismissed";

/// The most a marker may hold and still be a record: one program name per line, and
/// atpkg refuses names past 64 bytes, so a legitimate set is a few hundred bytes. Same
/// ceiling as atpkg's own prefix markers (`store::Layout::retired_date`); a bigger file
/// is something else wearing the name and reads as "never dismissed" — the card shows,
/// which errs toward disclosure.
pub(crate) const MAX_ADMIN_STEP_MARKER_BYTES: usize = 4096;

pub(crate) fn admin_step_marker_path(
    config_path: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    config_path
        .and_then(std::path::Path::parent)
        .map(|dir| dir.join(ADMIN_STEP_MARKER))
}

/// The marker's text, when a REGULAR, non-symlink file of at most
/// [`MAX_ADMIN_STEP_MARKER_BYTES`] sits at `path` — the symlink-refusing, size-capped
/// rule every other prefix marker follows (`atpkg::store::Layout::retired_date`,
/// `optin_exists`), spelled once in [`crate::config_marker`]. A planted link, a
/// directory, an oversized or non-UTF-8 file is not a dismissal this machine recorded:
/// `None`, and the card shows.
pub(crate) fn read_admin_step_marker(path: &std::path::Path) -> Option<String> {
    crate::config_marker::read_marker(path, MAX_ADMIN_STEP_MARKER_BYTES)
}

/// Record "Not now" for `names`. Best-effort: an unwritable config dir means the card
/// comes back next pass, which errs toward disclosure.
///
/// Never writes THROUGH anything already at the marker path — the exclusive-sibling
/// plus rename rule and the occupied-path refusal are [`crate::config_marker`]'s,
/// shared with the macOS access card's answer marker; a set too large for a marker
/// is refused there, never truncated.
pub(crate) fn record_admin_step_dismissal(
    config_path: Option<&std::path::Path>,
    names: &[String],
) -> std::io::Result<()> {
    let Some(path) = admin_step_marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the dismissal in",
        ));
    };
    crate::config_marker::write_marker(
        &path,
        &admin_step_marker_text(names),
        MAX_ADMIN_STEP_MARKER_BYTES,
    )
}

/// Whether the admin card should be raised NOW for the rows `status.toml` carries —
/// the off-thread half of the first-launch admin step. Reads the marker file (never on
/// the UI thread: callers are the atpkg-update worker). Returns the names, in door
/// order, when the card is due.
pub(crate) fn admin_step_due(
    status: &atpkg::Status,
    config_path: Option<&std::path::Path>,
) -> Option<Vec<String>> {
    let rows: Vec<(String, String)> = status
        .programs
        .iter()
        .map(|(name, program)| (name.clone(), program.state.clone()))
        .collect();
    let names = needs_admin_order(&rows);
    let marker = admin_step_marker_path(config_path).and_then(|p| read_admin_step_marker(&p));
    admin_step_should_show(marker.as_deref(), &names).then_some(names)
}

/// Who installs `name` and how — the honest sentence the admin card and the Needs-admin
/// rows use. Spelled from the §17.8 protocols for the two shipped rows; anything newer
/// says the generic truth (its own installer, through atpkg's door).
pub(crate) fn admin_vendor_line(name: &str) -> String {
    match name {
        "clt" => "Apple Command Line Tools (Apple's installer, via softwareupdate)".to_string(),
        "brew" => "Homebrew (its signed installer package)".to_string(),
        other => format!("{other} (its own installer)"),
    }
}

/// The admin card's caption, in the notice grammar `"<marker> <title> — <detail>"`:
/// what is waiting, who installs it, and that macOS will ask for the password.
pub(crate) fn admin_step_caption(names: &[String]) -> String {
    let what: Vec<String> = names.iter().map(|n| admin_vendor_line(n)).collect();
    let verb = if names.len() == 1 { "needs" } else { "need" };
    format!(
        "\u{2699} Admin step waiting \u{2014} {} {verb} an administrator; Install opens macOS's own password dialog",
        what.join(", then ")
    )
}

/// Facts about the co-located package manager, collected entirely OFF the
/// event loop by one worker pass. Bounded and owned, suitable for a typed
/// event-loop wake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesStatusReport {
    /// The co-located `atpkg` binary exists beside our executable. Release
    /// bundles ship it in Contents/MacOS; a WORKSPACE dev build has
    /// `target/debug/atpkg` beside the dev binary too (atpkg is a workspace
    /// bin), so dev runs exercise the same co-location seam — inert without a
    /// pinned root key like any unpinned build. Only a truly solitary binary
    /// (e.g. a copied-out executable) has none.
    pub(crate) available: bool,
    /// The co-located CLI's OWN posture, mirrored: the compiled root anchor is
    /// present and `ATPKG_DISABLE` is unset. False ⇒ inert by construction.
    pub(crate) manager_enabled: bool,
    /// `ATPKG_DISABLE` was set at collection time — the inert cause is the
    /// user's opt-out, NOT a missing key; the page must say the true reason.
    pub(crate) disabled_by_env: bool,
    /// The pinned root key's operator-facing fingerprint (the doctor line).
    pub(crate) root_fingerprint: String,
    /// `status.toml` existed and parsed (atpkg has run at least once).
    pub(crate) recorded: bool,
    /// `status.toml` fields (empty when `recorded` is false).
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    /// Bounded, operator-readable diagnostic when some private package
    /// metadata could not be admitted or parsed. A worker still publishes the
    /// rest of the snapshot, so Settings never remains permanently “Reading”.
    pub(crate) collection_error: Option<String>,
}

impl PackagesStatusReport {
    /// The honest zero state before any worker observation: nothing is claimed
    /// available or enabled until a collection pass has actually looked.
    fn unobserved() -> Self {
        Self {
            available: false,
            manager_enabled: false,
            disabled_by_env: false,
            root_fingerprint: String::new(),
            recorded: false,
            updated_at: String::new(),
            outcome: String::new(),
            index_source: String::new(),
            programs: Vec::new(),
            collection_error: None,
        }
    }

    /// Pure assembly from atpkg's own record types — the testable core of
    /// [`collect_packages_status`]. `links` maps program → dev-link target.
    pub(crate) fn from_parts(
        available: bool,
        manager_enabled: bool,
        root_fingerprint: String,
        status: Option<&atpkg::Status>,
        links: &[(String, Option<std::path::PathBuf>)],
    ) -> Self {
        let mut programs: Vec<PackagesProgramRow> = status
            .map(|status| {
                status
                    .programs
                    .iter()
                    .map(|(name, program)| PackagesProgramRow::from_status(name, program))
                    .collect()
            })
            .unwrap_or_default();
        // The Needs-admin group is a property of the WHOLE row set: Homebrew's row
        // reads `blocked by clt: needs admin — …`, and it waits on the same door.
        let admin_rows: Vec<(String, String)> = programs
            .iter()
            .map(|row| (row.name.clone(), row.state.clone()))
            .collect();
        for name in needs_admin_order(&admin_rows) {
            if let Some(row) = programs.iter_mut().find(|row| row.name == name) {
                row.group = RowGroup::NeedsAdmin;
            }
        }
        // Dev-linked programs are managed OUTSIDE the registry (update
        // hard-skips them); surface them in the same list, annotated, so the
        // page never claims a linked tool is registry-current.
        for (name, target) in links {
            let annotation = Some(match target {
                Some(path) => {
                    let mut label = String::from("dev-link → ");
                    label.push_str(&path.display().to_string());
                    label
                }
                None => "dev-link".to_string(),
            });
            if let Some(row) = programs.iter_mut().find(|row| row.name == *name) {
                row.annotation = annotation;
            } else {
                programs.push(PackagesProgramRow {
                    name: name.clone(),
                    installed_build: None,
                    state: "linked".to_string(),
                    kind: ProgramStateKind::Other,
                    group: RowGroup::Default,
                    facts: None,
                    annotation,
                });
            }
        }
        programs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            available,
            manager_enabled,
            disabled_by_env: false,
            root_fingerprint,
            recorded: status.is_some(),
            updated_at: status.map(|s| s.updated_at.clone()).unwrap_or_default(),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            index_source: status.map(|s| s.index_source.clone()).unwrap_or_default(),
            programs,
            collection_error: None,
        }
    }
}

impl PackagesStatusReport {
    /// Regroup rows whose names carry an opt-in marker as extras (an installed extra
    /// otherwise reads exactly like a default-set member). A needs-admin row keeps its
    /// group: the door outranks the roster.
    pub(crate) fn mark_extras<S: AsRef<str>>(&mut self, optins: impl IntoIterator<Item = S>) {
        for name in optins {
            if let Some(row) = self
                .programs
                .iter_mut()
                .find(|row| row.name == name.as_ref() && row.group == RowGroup::Default)
            {
                row.group = RowGroup::Extras;
                if row.facts.is_none() {
                    row.facts = atpkg::stub::describe(&row.name).map(ExtraFacts::parse);
                }
            }
        }
    }

    /// The needs-admin names in door order (see [`needs_admin_order`]).
    pub(crate) fn needs_admin(&self) -> Vec<String> {
        let rows: Vec<(String, String)> = self
            .programs
            .iter()
            .map(|row| (row.name.clone(), row.state.clone()))
            .collect();
        needs_admin_order(&rows)
    }
}

const MAX_PACKAGE_COLLECTION_ERRORS: usize = 4;

fn collection_error_summary(errors: Vec<String>, total: usize) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let mut summary = errors.join(" · ");
    if total > errors.len() {
        summary.push_str(" · … and ");
        summary.push_str(&(total - errors.len()).to_string());
        summary.push_str(" more metadata errors");
    }
    Some(summary)
}

/// Collect the packages report from the real machine — filesystem reads only
/// (no network, no subprocess). MUST run off the event loop (worker threads
/// only): it stats the co-located binary and parses `status.toml`.
pub(crate) fn collect_packages_status(available: bool) -> PackagesStatusReport {
    // The CONFIGURED prefix, not the default: this page is what every seed notice
    // points at, and reading the default store made a relocated lab store report
    // "No package activity yet" forever (2026-08-20 round-8 audit).
    let layout = atpkg::store::resolve_configured();
    collect_packages_status_from_layout(available, layout.as_ref())
}

fn collect_packages_status_from_layout(
    available: bool,
    layout: Option<&atpkg::Layout>,
) -> PackagesStatusReport {
    let mut errors = Vec::new();
    let mut error_count = 0usize;
    let mut record_error = |message: String| {
        error_count = error_count.saturating_add(1);
        if errors.len() < MAX_PACKAGE_COLLECTION_ERRORS {
            errors.push(message);
        }
    };
    let status = layout.and_then(|layout| match atpkg::status::read_checked(layout) {
        Ok(status) => status,
        Err(error) => {
            record_error(format!("Could not read status.toml: {error}"));
            None
        }
    });
    let links: Vec<(String, Option<std::path::PathBuf>)> = layout
        .map(|layout| match atpkg::linked_programs_checked(layout) {
            Ok(names) => names
                .into_iter()
                .filter_map(|name| match atpkg::linked_checkout_checked(layout, &name) {
                    Ok(Some(target)) => Some((name, Some(target))),
                    Ok(None) => {
                        record_error(format!("Link marker {name:?} disappeared while reading"));
                        None
                    }
                    Err(error) => {
                        record_error(format!("Could not read link marker {name:?}: {error}"));
                        None
                    }
                })
                .collect(),
            Err(error) => {
                record_error(format!("Could not enumerate package links: {error}"));
                Vec::new()
            }
        })
        .unwrap_or_default();
    // Mirror the CO-LOCATED CLI's own posture: `ATPKG_DISABLE` inerts a pinned
    // build, and the page must report the cause actually in force. There is no
    // longer any root-key override — the anchor is compiled in, so the pin's
    // fingerprint is always the live one.
    let disabled_by_env = std::env::var_os("ATPKG_DISABLE").is_some();
    let manager_enabled = atpkg::manager_enabled();
    let mut report = PackagesStatusReport::from_parts(
        available,
        manager_enabled,
        atpkg::root_key_fingerprint(),
        status.as_ref(),
        &links,
    );
    // An extra the user opted in to and that is now installed reads `managed …`, the
    // same words as a default-set member; the opt-in marker is what still says it is
    // an extra. Read here (worker thread), applied to the grouping only.
    if let Some(layout) = layout {
        report.mark_extras(layout.optins());
    }
    report.disabled_by_env = disabled_by_env;
    report.collection_error = collection_error_summary(errors, error_count);
    report
}

/// Host-owned reducer for the packages surface: one worker at a time, stale
/// completions inert, every observable change bumps `revision` for fan-out.
/// Construction is filesystem-free (the updater-service doctrine): nothing is
/// claimed until a worker reports.
pub(crate) struct PackagesService {
    revision: u64,
    /// Worker generation: a completion must present the exact sequence its
    /// `begin` minted, so a superseded worker cannot import stale facts.
    sequence: u64,
    inflight: bool,
    busy: Option<PackagesBusy>,
    report: Option<PackagesStatusReport>,
    last_command: Option<PackagesCommandOutcome>,
    /// What bare `atpkg machine` last measured, plus this launch's memory of what
    /// changed. Rides the same revision fan-out as the rest of the surface.
    machine: MachinePosture,
}

/// The `[machine]` host settings as the window CONFIRMED them: the record bare
/// `atpkg machine` printed (`machine-state:`), read by the host's machine worker —
/// never re-derived in-process from `defaults` or a home walk — plus this launch's
/// memory of the last `machine-settings:` change and the last apply verdict.
/// Nothing is durable: a fresh launch starts unobserved and reads again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachinePosture {
    /// A read has completed at least once (a parsed state OR a read error).
    pub(crate) observed: bool,
    /// The parsed record, when the last read produced one.
    pub(crate) state: Option<atpkg::machine::MachineState>,
    /// Why the last read produced no record (the child could not be spawned, or
    /// printed no `machine-state:` line — which is never "nothing to apply").
    pub(crate) read_error: Option<String>,
    /// The last `machine-settings:` body this process saw and when it arrived.
    pub(crate) last_change: Option<(String, std::time::SystemTime)>,
    /// The last `atpkg machine apply` verdict sentence this process saw.
    pub(crate) last_verdict: Option<String>,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// A read was asked for while one was running: the running read may have
    /// started before an apply and would land as a pre-apply record, so it is
    /// followed by another read rather than joined.
    pub(crate) rerun: bool,
}

/// Scalar projection used only by Tier-1 conformance. Every field is read from
/// the genuine reducer; `presented_result` additionally reads the shipping
/// projection headline so the formal trace is bound to what Settings renders.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PackagesModelState {
    pub(crate) sequence: u64,
    pub(crate) inflight: bool,
    pub(crate) operation: u8,
    pub(crate) observed: bool,
    pub(crate) last_operation: u8,
    pub(crate) last_result: u8,
    pub(crate) presented_result: u8,
}

impl PackagesService {
    pub(crate) fn new() -> Self {
        Self {
            revision: 1,
            sequence: 0,
            inflight: false,
            busy: None,
            report: None,
            last_command: None,
            machine: MachinePosture::default(),
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn busy(&self) -> Option<PackagesBusy> {
        self.busy
    }

    /// The machine posture as last reduced. Test-only: the host reaches the
    /// posture through [`Self::request_machine_read`] / [`Self::replace_machine_state`]
    /// and the projection, never by inspecting it.
    #[cfg(test)]
    pub(crate) fn machine(&self) -> &MachinePosture {
        &self.machine
    }

    /// Mark a read as running without asking for one — test-only, to stage the
    /// "read in flight" posture; the host uses [`Self::request_machine_read`].
    #[cfg(test)]
    pub(crate) fn set_machine_refreshing(&mut self, refreshing: bool) {
        if self.machine.refreshing == refreshing {
            return;
        }
        self.machine.refreshing = refreshing;
        self.revision = self.revision.saturating_add(1);
    }

    /// Ask for a machine read. `true` ⇒ the caller spawns one (the posture is now
    /// refreshing); `false` ⇒ one is already running and a rerun is queued behind
    /// it — never joined, because the running read may predate an apply and would
    /// land as the final "observed" record.
    pub(crate) fn request_machine_read(&mut self) -> bool {
        if self.machine.refreshing {
            self.machine.rerun = true;
            return false;
        }
        self.machine.refreshing = true;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Reduce one machine read: a parsed `machine-state:` record, or the reason
    /// there was none. Either way the posture is now OBSERVED — a read error is a
    /// fact the card states, not a reason to keep saying "reading". Returns `true`
    /// when a rerun was queued while this read ran: the posture then STAYS
    /// refreshing (Apply disabled, the card still "Reading…") and the caller must
    /// spawn the already-admitted read directly, without requesting admission again.
    #[must_use = "a queued rerun must be spawned by the caller"]
    pub(crate) fn replace_machine_state(
        &mut self,
        result: Result<atpkg::machine::MachineState, String>,
    ) -> bool {
        self.machine.observed = true;
        let rerun = std::mem::take(&mut self.machine.rerun);
        self.machine.refreshing = rerun;
        match result {
            Ok(state) => {
                self.machine.state = Some(state);
                self.machine.read_error = None;
            }
            Err(error) => {
                // Keep the last good record beside the error: a transient read
                // failure must not erase what was measured a moment ago.
                self.machine.read_error = Some(error);
            }
        }
        self.revision = self.revision.saturating_add(1);
        rerun
    }

    /// Remember a `machine-settings:` change (the pull-down row's body) and when
    /// this process saw it.
    pub(crate) fn note_machine_change(&mut self, body: String, at: std::time::SystemTime) {
        self.machine.last_change = Some((body, at));
        self.revision = self.revision.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn model_state(&self) -> PackagesModelState {
        let operation = match (self.inflight, self.busy) {
            (false, _) => 0,
            (true, None) => 1,
            (true, Some(PackagesBusy::Check)) => 2,
            (true, Some(PackagesBusy::Install)) => 3,
            (true, Some(PackagesBusy::Uninstall)) => 4,
            (true, Some(PackagesBusy::InstallExtra)) => 5,
            (true, Some(PackagesBusy::InstallAdmin)) => 6,
            (true, Some(PackagesBusy::MachineApply)) => 7,
        };
        let (last_operation, last_result) = match self.last_command.as_ref() {
            None => (0, 0),
            Some(PackagesCommandOutcome::Succeeded { operation }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::InstallExtra => 5,
                    PackagesBusy::InstallAdmin => 6,
                    PackagesBusy::MachineApply => 7,
                },
                1,
            ),
            Some(PackagesCommandOutcome::Failed { operation, .. }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::InstallExtra => 5,
                    PackagesBusy::InstallAdmin => 6,
                    PackagesBusy::MachineApply => 7,
                },
                2,
            ),
        };
        let headline = self
            .state(true, true, true, false, false)
            .projection()
            .headline;
        let presented_result = if headline.ends_with("completed") {
            1
        } else if headline.ends_with("failed") {
            2
        } else {
            0
        };
        PackagesModelState {
            sequence: self.sequence,
            inflight: self.inflight,
            operation,
            observed: self.report.is_some(),
            last_operation,
            last_result,
            presented_result,
        }
    }

    /// Begin one worker pass (`busy = None` ⇒ a silent status refresh).
    /// Returns the minted sequence, or `None` while a pass is already running —
    /// callers treat that as "join" (the running pass ends with a fresh
    /// collection anyway).
    pub(crate) fn begin(&mut self, busy: Option<PackagesBusy>) -> Option<u64> {
        if self.inflight {
            return None;
        }
        self.sequence = self.sequence.saturating_add(1);
        self.inflight = true;
        self.busy = busy;
        if busy.is_some() {
            // The in-flight headline is now authoritative. Never leave the
            // previous verb's result attached to a new attempt.
            self.last_command = None;
        }
        self.revision = self.revision.saturating_add(1);
        Some(self.sequence)
    }

    /// Reduce one worker completion. `false` ⇒ stale (reducer-inert means
    /// presentation-inert too: the caller must not publish). A current but
    /// protocol-mismatched result is inert as well: a refresh cannot settle a
    /// verb, and a command result cannot settle a refresh reservation.
    pub(crate) fn finish(&mut self, sequence: u64, completion: PackagesWorkerCompletion) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        let command_operation = completion
            .command
            .as_ref()
            .map(PackagesCommandOutcome::operation);
        if self.busy != command_operation {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        self.report = Some(completion.report);
        if completion.command.is_some() {
            self.last_command = completion.command;
        }
        if completion.machine_verdict.is_some() {
            self.machine.last_verdict = completion.machine_verdict;
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Release a reservation whose worker never started (thread-spawn failure):
    /// clears inflight/busy WITHOUT storing a report, so a previously-observed
    /// snapshot keeps its real facts and a never-observed surface honestly
    /// stays unobserved (no fabricated `manager_enabled`/fingerprint). `false`
    /// ⇒ stale.
    pub(crate) fn abort(&mut self, sequence: u64) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// The state a Settings controller renders, resolved against the CURRENT
    /// config consent flags (the config is host-owned; the service holds only
    /// worker facts).
    pub(crate) fn state(
        &self,
        loop_enabled: bool,
        master_enabled: bool,
        auto_update: bool,
        auto_install: bool,
        loop_running: bool,
    ) -> PackagesState {
        PackagesState {
            observed: self.report.is_some(),
            report: self
                .report
                .clone()
                .unwrap_or_else(PackagesStatusReport::unobserved),
            busy: self.busy,
            inflight: self.inflight,
            last_command: self.last_command.clone(),
            loop_enabled,
            master_enabled,
            auto_update,
            auto_install,
            loop_running,
            machine: self.machine.clone(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_spotlight_noindex: true,
        }
    }
}

/// The snapshot a [`crate::native_settings::SettingsApp`] holds (the
/// [`crate::update_screen::UpdateState`] analogue). Pure data; `projection()`
/// derives the presentation strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesState {
    /// A worker pass has completed at least once (before that the page says
    /// "Reading package status…" instead of claiming anything).
    observed: bool,
    report: PackagesStatusReport,
    busy: Option<PackagesBusy>,
    /// ANY worker is inflight — including the silent status refresh, whose
    /// `busy` label is `None`. Actions gate on THIS (not on `busy`): a click
    /// admitted during a silent refresh would only be dropped by the host's
    /// one-worker rule, so the buttons disable for the (brief) window instead.
    inflight: bool,
    /// Final result of the most recent UI-initiated verb. Silent refreshes keep
    /// it; starting a new verb clears it.
    last_command: Option<PackagesCommandOutcome>,
    /// Resolved `[packages]` flags (`enabled && auto_update`, `auto_update`,
    /// `auto_install`) — display only; the switches edit the config keys.
    loop_enabled: bool,
    master_enabled: bool,
    auto_update: bool,
    auto_install: bool,
    /// Immutable fact: the background updater thread actually started for this
    /// process. Saved config may differ until the next launch.
    loop_running: bool,
    /// The `[machine]` host settings as confirmed by the machine read worker.
    machine: MachinePosture,
    /// The SAVED `[machine]` switches, resolved from the host's config — display
    /// only, so the card can say when a saved switch has not been read yet.
    saved_universal_control: atpkg::config::UniversalControlPolicy,
    saved_spotlight_noindex: bool,
}

impl PackagesState {
    /// Attach the host's saved `[machine]` switches (the config is host-owned; the
    /// service holds only worker facts).
    pub(crate) fn with_machine_config(
        mut self,
        universal_control: atpkg::config::UniversalControlPolicy,
        spotlight_noindex: bool,
    ) -> Self {
        self.saved_universal_control = universal_control;
        self.saved_spotlight_noindex = spotlight_noindex;
        self
    }
    /// The pre-observation default a freshly-created Settings controller holds
    /// until the host publishes a real snapshot.
    pub(crate) fn unobserved() -> Self {
        Self {
            observed: false,
            report: PackagesStatusReport::unobserved(),
            busy: None,
            inflight: false,
            last_command: None,
            loop_enabled: true,
            master_enabled: true,
            auto_update: true,
            auto_install: false,
            loop_running: false,
            machine: MachinePosture::default(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_spotlight_noindex: true,
        }
    }

    /// Snapshot the exact state the Packages page paints.
    pub(crate) fn projection(&self) -> PackagesProjection {
        self.projection_at(std::time::SystemTime::now())
    }

    /// [`Self::projection`] with the clock injected — the "Last change … ago"
    /// line is the only time-dependent word on the surface, and tests pin it.
    pub(crate) fn projection_at(&self, now: std::time::SystemTime) -> PackagesProjection {
        let report = &self.report;
        let actions_enabled = self.observed
            && report.available
            && report.manager_enabled
            && report.collection_error.is_none()
            && !self.inflight;
        let headline = if !self.observed {
            "Reading package status…".to_string()
        } else if let Some(busy) = self.busy {
            match busy {
                PackagesBusy::Check => "Checking toolchain packages…".to_string(),
                PackagesBusy::Install => "Installing the ALab toolset…".to_string(),
                PackagesBusy::Uninstall => "Removing the ALab toolset…".to_string(),
                PackagesBusy::InstallExtra => "Installing the extra…".to_string(),
                PackagesBusy::InstallAdmin => {
                    "Installing through macOS — the administrator dialog is open…".to_string()
                }
                PackagesBusy::MachineApply => "Applying the machine settings…".to_string(),
            }
        } else if let Some(command) = self.last_command.as_ref() {
            command.headline().to_string()
        } else if !report.available {
            "Package manager unavailable".to_string()
        } else if !report.manager_enabled {
            "Package manager inert".to_string()
        } else if report.collection_error.is_some() {
            "Package status incomplete".to_string()
        } else if report.recorded {
            "Toolchain packages managed".to_string()
        } else {
            "No package activity yet".to_string()
        };
        let recorded_detail = || {
            let mut detail = String::new();
            if !report.outcome.is_empty() {
                detail.push_str(&report.outcome);
            }
            if !report.updated_at.is_empty() {
                if !detail.is_empty() {
                    detail.push_str("  ·  ");
                }
                detail.push_str(&report.updated_at);
            }
            (!detail.is_empty()).then_some(detail)
        };
        // A machine apply that succeeded quotes the pass's own verdict sentence
        // (`applied — …` / `nothing changed — …`) instead of a bare headline.
        let command_feedback = match (
            self.last_command.as_ref(),
            self.machine.last_verdict.as_deref(),
        ) {
            (
                Some(PackagesCommandOutcome::Succeeded {
                    operation: PackagesBusy::MachineApply,
                }),
                Some(verdict),
            ) => Some(format!("Machine settings: {verdict}")),
            (Some(command), _) => Some(command.feedback()),
            (None, _) => None,
        };
        let mut detail = if !self.observed {
            None
        } else if let Some(command) = self.last_command.as_ref() {
            let mut detail = command_feedback
                .clone()
                .unwrap_or_else(|| command.feedback());
            if let Some(recorded) = recorded_detail() {
                match command {
                    PackagesCommandOutcome::Succeeded { .. } => {
                        detail.push_str("  ·  Recorded status: ");
                    }
                    PackagesCommandOutcome::Failed { .. } => {
                        detail.push_str("  ·  Earlier recorded status (not this attempt): ");
                    }
                }
                detail.push_str(&recorded);
            }
            Some(detail)
        } else if !report.available {
            Some("No co-located atpkg binary beside this executable.".to_string())
        } else if !report.manager_enabled {
            // The doctor line: name the cause that is actually in force —
            // the user's opt-out beats "no key" (a pinned build with
            // ATPKG_DISABLE set is switched off, not unpinned).
            Some(if report.disabled_by_env {
                "ATPKG_DISABLE is set — the package manager is switched off for this launch."
                    .to_string()
            } else {
                "No package root key is pinned in this build — atpkg refuses all installs."
                    .to_string()
            })
        } else if report.recorded {
            recorded_detail()
        } else {
            Some("atpkg has not run yet — check now to record a first status.".to_string())
        };
        if self.observed
            && let Some(error) = report.collection_error.as_deref()
        {
            let warning = format!("Package metadata warning: {error}");
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str("  ·  ");
                    detail.push_str(&warning);
                }
                None => detail = Some(warning),
            }
        }
        let loop_status = match (self.loop_running, self.master_enabled, self.loop_enabled) {
            (true, false, _) => {
                "Started this launch · Automatic maintenance Saved Off · Won’t start next launch"
            }
            (false, false, _) => "Not started · Automatic maintenance Saved Off",
            (true, true, false) => {
                "Started this launch · Auto-update Saved Off · Won’t run next launch"
            }
            (false, true, true) => "Not started this launch · Saved On · Starts next launch",
            (true, true, true) => "Started this launch",
            (false, true, false) => "Not started · Auto-update Saved Off",
        }
        .to_string();
        // The machine verb is NOT manager-gated: `atpkg machine apply` is a local
        // verb that works with the manager switched off, so its slice gets the
        // page's idleness alone — observed, atpkg present, no worker inflight.
        let machine_idle = self.observed && report.available && !self.inflight;
        let machine = self.machine_projection(machine_idle, now);
        PackagesProjection {
            machine,
            observed: self.observed,
            available: report.available,
            manager_enabled: report.manager_enabled,
            disabled_by_env: report.disabled_by_env,
            // The compiled pin IS the live anchor now, so its fingerprint is
            // always the honest one to show.
            root_fingerprint: report.root_fingerprint.clone(),
            recorded: report.recorded,
            updated_at: report.updated_at.clone(),
            outcome: report.outcome.clone(),
            index_source: report.index_source.clone(),
            programs: report.programs.clone(),
            needs_admin: report.needs_admin(),
            admin_door: cfg!(target_os = "macos"),
            collection_error: report.collection_error.clone(),
            busy: self.busy,
            refreshing: self.inflight && self.busy.is_none(),
            loop_enabled: self.loop_enabled,
            master_enabled: self.master_enabled,
            auto_update: self.auto_update,
            auto_install: self.auto_install,
            loop_running: self.loop_running,
            loop_status,
            actions_enabled,
            headline,
            detail,
            command_feedback,
        }
    }

    /// The "This Mac" card's words, derived HERE so pixels, accessibility and
    /// `aterm ctl` introspection read one truth. `idle` is the page's idleness
    /// (observed, atpkg present, no worker inflight) and deliberately NOT the
    /// manager-gated `actions_enabled`: `atpkg machine apply` is a local verb that
    /// works with the package manager switched off. The button additionally needs
    /// a parsed record, nothing in the way, and a verdict — measured posture ×
    /// the SAVED switches, which is what the apply child will read — that says
    /// something is left to do.
    fn machine_projection(&self, idle: bool, now: std::time::SystemTime) -> MachineProjection {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, UcPosture};
        let posture = &self.machine;
        let supported = cfg!(target_os = "macos");
        let state = posture.state.as_ref();
        // A read that failed AFTER a good record keeps the record (a transient
        // failure must not erase a measurement) but every measured sentence says
        // it is prior — a stale record never reads as a fresh one.
        let prior = if posture.read_error.is_some() && state.is_some() {
            " (from the last successful read)"
        } else {
            ""
        };
        let universal_control = match state {
            None => "Universal Control: not read yet".to_string(),
            Some(s) => {
                let word = match s.universal_control {
                    UcPosture::Disabled => "disabled on this Mac",
                    UcPosture::Default => {
                        "at the OS default — the cursor roams to other Macs and iPads"
                    }
                    UcPosture::Partial => "partly disabled",
                    UcPosture::Unknown => "unknown",
                };
                let policy = match (s.policy, s.universal_control) {
                    (UniversalControlPolicy::Leave, UcPosture::Disabled)
                    | (UniversalControlPolicy::Off, _) => "",
                    (UniversalControlPolicy::Leave, _) => {
                        " · left alone ([machine] universal_control = \"leave\")"
                    }
                };
                format!("Universal Control: {word}{policy}{prior}")
            }
        };
        let spotlight = match state {
            None => "Build output: not read yet".to_string(),
            Some(s) => {
                let at_least = if s.scan_complete {
                    ""
                } else {
                    " (at least — the scan hit its budget)"
                };
                let policy = if s.spotlight_noindex {
                    ""
                } else {
                    " · switched off ([machine] spotlight_noindex = false)"
                };
                // What an apply CAN hide — the CLI's `can_hide`: nothing with the
                // switch off, whatever the scan counted as migratable.
                let can_hide = if s.spotlight_noindex {
                    s.would_migrate
                } else {
                    0
                };
                format!(
                    "Build output: {} target dirs hidden, {} open to Spotlight — {can_hide} a pass would hide{at_least}{policy}{prior}",
                    s.hidden, s.exposed
                )
            }
        };
        let last_change = match posture.last_change.as_ref() {
            None => "No change recorded this launch".to_string(),
            Some((items, at)) => {
                let age_ms = now
                    .duration_since(*at)
                    .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or(0);
                format!(
                    "Last change: {items} · {}",
                    crate::session_chrome::relative_age(age_ms)
                )
            }
        };
        let reason = if let Some(error) = posture.read_error.as_deref() {
            Some(format!("Could not read the machine state: {error}"))
        } else {
            match state.map(|s| s.home) {
                Some(HomePosture::Mismatch) => Some(
                    "Not applied here: this window's HOME is not the account's home, and \
                     `defaults` writes the account's per-host settings regardless."
                        .to_string(),
                ),
                Some(HomePosture::Unresolved) => Some(
                    "Not applied here: the account home could not be resolved, so HOME \
                     cannot be proven to be it."
                        .to_string(),
                ),
                Some(HomePosture::Account) | None => None,
            }
        };
        // A switch saved in Settings after the last read: the record's `policy=` /
        // `noindex=` came from the file as it was THEN, so say what the next read,
        // pass or Apply now will use — the card must never look as if a flipped
        // switch had already been measured.
        let saved = state.and_then(|s| {
            let mut changed: Vec<String> = Vec::new();
            if s.policy != self.saved_universal_control {
                changed.push(format!(
                    "universal_control = \"{}\"",
                    match self.saved_universal_control {
                        UniversalControlPolicy::Off => "off",
                        UniversalControlPolicy::Leave => "leave",
                    }
                ));
            }
            if s.spotlight_noindex != self.saved_spotlight_noindex {
                changed.push(format!(
                    "spotlight_noindex = {}",
                    self.saved_spotlight_noindex
                ));
            }
            (!changed.is_empty()).then(|| {
                format!(
                    "Saved since the last read: {} — the next package pass or Apply now uses it",
                    changed.join(", ")
                )
            })
        });
        // THE verdict — the same `machine_next` table the CLI uses, over the
        // measured posture and the SAVED switches (the apply child reads the file
        // fresh, so a switch flipped after the read decides what it will do; the
        // record's own `policy=`/`noindex=` only feed the measured sentences and
        // the "Saved since the last read" line). None on any home but the
        // account's, exactly as `MachineState::next`.
        let next = state.and_then(|s| {
            (s.home == HomePosture::Account)
                .then(|| {
                    atpkg::machine::machine_next(
                        s.universal_control,
                        self.saved_universal_control,
                        self.saved_spotlight_noindex,
                        s.would_migrate,
                    )
                })
                .flatten()
        });
        let apply_enabled = idle
            && supported
            && posture.observed
            && !posture.refreshing
            && reason.is_none()
            && next.is_some();
        // `next()` is already None on a home mismatch, and the reason line says
        // why — so "nothing to apply" is claimed only when nothing is in the way.
        let nothing_to_apply = posture.observed && reason.is_none() && next.is_none();
        let next = match next {
            Some(n) if reason.is_none() => {
                let mut what: Vec<String> = Vec::new();
                if n.universal_control {
                    what.push("Universal Control off for this host".to_string());
                }
                if n.spotlight > 0 {
                    what.push(format!(
                        "{} target dir(s) hidden from Spotlight",
                        n.spotlight
                    ));
                }
                format!("Apply now would set: {}", what.join("; "))
            }
            _ if nothing_to_apply => {
                "Nothing to apply — Universal Control and Spotlight are where [machine] wants them"
                    .to_string()
            }
            _ => String::new(),
        };
        MachineProjection {
            supported,
            observed: posture.observed,
            refreshing: posture.refreshing,
            universal_control,
            spotlight,
            last_change,
            last_verdict: posture.last_verdict.clone(),
            reason,
            saved,
            next,
            apply_enabled,
            nothing_to_apply,
        }
    }
}

/// The "This Mac" card on Settings ▸ Security, as words (the
/// [`PackagesProjection`] slice for the `[machine]` host settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineProjection {
    /// These are macOS host settings; elsewhere the card is absent.
    pub(crate) supported: bool,
    /// A machine read has completed at least once.
    pub(crate) observed: bool,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// `Universal Control: …` — the measured posture.
    pub(crate) universal_control: String,
    /// `Build output: …` — the Spotlight counts.
    pub(crate) spotlight: String,
    /// `Last change: … · <age>` or `No change recorded this launch`.
    pub(crate) last_change: String,
    /// The last `atpkg machine apply` verdict this launch, when there is one.
    pub(crate) last_verdict: Option<String>,
    /// Why an apply would do nothing here (home mismatch/unresolved) or why the
    /// state could not be read. `None` when there is nothing in the way.
    pub(crate) reason: Option<String>,
    /// A `[machine]` switch saved after the last read, and what will use it.
    pub(crate) saved: Option<String>,
    /// What Apply now would do (`Apply now would set: …`), or the "nothing to
    /// apply" sentence; empty while unobserved or when a reason is in the way.
    pub(crate) next: String,
    /// The Apply now button: page idle, macOS, observed, not refreshing, and the
    /// verdict says something is left to do.
    pub(crate) apply_enabled: bool,
    /// Observed, readable, and the verdict is "nothing to apply" — the card says so
    /// in place of an enabled button.
    pub(crate) nothing_to_apply: bool,
}

/// Owned, structured read projection shared with the native Settings
/// `/packages` route (the [`crate::update_screen::UpdateProjection`] analogue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProjection {
    /// The `[machine]` host settings slice (Settings ▸ Security's "This Mac" card).
    pub(crate) machine: MachineProjection,
    pub(crate) observed: bool,
    pub(crate) available: bool,
    pub(crate) manager_enabled: bool,
    /// Typed admission cause retained from the worker report. Consumers must
    /// not reverse-engineer ATPKG_DISABLE from operator-facing detail text.
    pub(crate) disabled_by_env: bool,
    pub(crate) root_fingerprint: String,
    pub(crate) recorded: bool,
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    /// The programs waiting on an administrator, in the order the GUI door installs
    /// them ([`needs_admin_order`]). The Install control on any of them runs the door
    /// for that program AND everything before it in this list.
    pub(crate) needs_admin: Vec<String>,
    /// The osascript door exists on this platform (macOS). Elsewhere the Needs-admin
    /// rows name the terminal command instead of offering a control that could only
    /// record `needs admin` again.
    pub(crate) admin_door: bool,
    pub(crate) collection_error: Option<String>,
    pub(crate) busy: Option<PackagesBusy>,
    /// A SILENT status-refresh worker is inflight (no `busy` label). Action
    /// admission must refuse during this brief window too — the host runs one
    /// worker at a time, and a raced click would otherwise be dropped.
    pub(crate) refreshing: bool,
    pub(crate) loop_enabled: bool,
    pub(crate) master_enabled: bool,
    pub(crate) auto_update: bool,
    pub(crate) auto_install: bool,
    pub(crate) loop_running: bool,
    /// What is actually running now, plus any saved-vs-live mismatch. Derived
    /// here so pixels, accessibility, and introspection use one truth source.
    pub(crate) loop_status: String,
    /// Both action buttons: available AND enabled AND idle AND observed.
    pub(crate) actions_enabled: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// Final result text for replacing the initiating view's temporary
    /// synchronous “request accepted” feedback.
    pub(crate) command_feedback: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh(report: PackagesStatusReport) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::refresh(report)
    }

    fn succeeded(
        report: PackagesStatusReport,
        operation: PackagesBusy,
    ) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::command(report, PackagesCommandOutcome::Succeeded { operation })
    }

    fn status(outcome: &str) -> atpkg::Status {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(1971),
                state: "active".to_string(),
                tree_root: String::new(),
            },
        );
        atpkg::Status {
            schema: 1,
            updated_at: "2026-07-21T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: outcome.to_string(),
            seams: Vec::new(),
            last_success_at: String::new(),
            programs,
        }
    }

    /// One worker at a time; a stale sequence is reducer-inert; a current
    /// completion clears busy, stores the report, and bumps the revision.
    #[test]
    fn service_serializes_workers_and_rejects_stale_completions() {
        let mut service = PackagesService::new();
        let base = service.revision();
        let seq = service
            .begin(Some(PackagesBusy::Check))
            .expect("idle → begin");
        assert!(service.revision() > base, "busy flip fans out");
        assert_eq!(service.busy(), Some(PackagesBusy::Check));
        assert!(
            service.begin(Some(PackagesBusy::Install)).is_none(),
            "second verb joins, never overlaps"
        );

        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(
            !service.finish(seq + 1, succeeded(report.clone(), PackagesBusy::Check)),
            "a superseded worker cannot import facts"
        );
        assert_eq!(
            service.busy(),
            Some(PackagesBusy::Check),
            "stale finish is inert"
        );
        let before = service.revision();
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        assert_eq!(service.busy(), None);
        assert!(service.revision() > before);
        let state = service.state(true, true, true, false, true);
        let projection = state.projection();
        assert!(projection.recorded);
        assert_eq!(projection.programs.len(), 1);
        assert_eq!(projection.programs[0].installed_build, Some(1971));
        assert!(projection.actions_enabled);
        assert_eq!(projection.headline, "Package check completed");
        assert_eq!(
            projection.command_feedback.as_deref(),
            Some("Package check completed")
        );
    }

    /// A spawn-failure abort releases the reservation WITHOUT importing facts:
    /// a never-observed surface stays unobserved (no fabricated inert claim),
    /// a previously-observed one keeps its real report; stale aborts are inert.
    #[test]
    fn abort_releases_the_reservation_without_fabricating_a_report() {
        let mut service = PackagesService::new();
        let seq = service.begin(None).expect("idle → begin");
        let before = service.revision();
        assert!(service.abort(seq), "current abort releases");
        assert!(service.revision() > before, "the un-busy flip fans out");
        assert_eq!(service.busy(), None);
        let state = service.state(true, true, true, false, true);
        assert!(
            state.projection().headline.contains("Reading"),
            "never-observed stays honestly unobserved after an abort"
        );

        // Observed facts survive a later abort untouched.
        let seq = service.begin(Some(PackagesBusy::Check)).unwrap();
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        let seq = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.abort(seq + 1), "stale abort is inert");
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.abort(seq));
        let projection = service.state(true, true, true, false, true).projection();
        assert!(projection.recorded, "the prior report's facts survive");
        assert_eq!(projection.programs.len(), 1);
        assert!(projection.actions_enabled);
    }

    /// The honest-state ladder: unobserved → unavailable → inert → recorded,
    /// each with a distinct headline and action gating that never lies.
    #[test]
    fn projection_headlines_track_the_honest_manager_posture() {
        let unobserved = PackagesState::unobserved().projection();
        assert!(unobserved.headline.contains("Reading"));
        assert!(
            !unobserved.actions_enabled,
            "nothing is claimed before observation"
        );

        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                false,
                false,
                String::new(),
                None,
                &[],
            )),
        ));
        let unavailable = service.state(true, true, true, false, true).projection();
        assert!(unavailable.headline.contains("unavailable"));
        assert!(
            unavailable
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("co-located")
        );
        assert!(!unavailable.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                false,
                "0000".into(),
                None,
                &[],
            )),
        ));
        let inert = service.state(true, true, true, false, true).projection();
        assert!(inert.headline.contains("inert"));
        assert!(
            inert.detail.as_deref().unwrap_or("").contains("root key"),
            "the doctor line names the missing trust anchor"
        );
        assert!(!inert.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("up to date")),
                &[]
            )),
        ));
        let live = service.state(true, true, true, false, true).projection();
        assert!(live.actions_enabled);
        assert!(live.detail.as_deref().unwrap().contains("up to date"));

        // Busy while a verb runs: actions gate off and the headline says which.
        let _ = service.begin(Some(PackagesBusy::Install)).unwrap();
        let busy = service.state(true, true, true, false, true).projection();
        assert!(busy.headline.contains("Installing"));
        assert!(!busy.actions_enabled);
    }

    #[test]
    fn projection_retains_typed_atpkg_disable_cause() {
        let mut report = PackagesStatusReport::from_parts(
            true,
            false,
            "compiled-root-present".into(),
            None,
            &[],
        );
        report.disabled_by_env = true;
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));
        let projection = service.state(true, true, true, false, true).projection();
        assert!(projection.disabled_by_env);
        assert!(!projection.manager_enabled);
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("ATPKG_DISABLE")
        );
    }

    #[test]
    fn projection_distinguishes_saved_loop_consent_from_this_launch() {
        let service = PackagesService::new();
        let stopping_next_launch = service.state(false, true, false, false, true).projection();
        assert!(stopping_next_launch.loop_running);
        assert!(
            stopping_next_launch
                .loop_status
                .contains("Started this launch")
        );
        assert!(stopping_next_launch.loop_status.contains("Saved Off"));
        assert!(
            stopping_next_launch
                .loop_status
                .contains("Won’t run next launch")
        );

        let starting_next_launch = service.state(true, true, true, false, false).projection();
        assert!(!starting_next_launch.loop_running);
        assert!(
            starting_next_launch
                .loop_status
                .contains("Not started this launch")
        );
        assert!(starting_next_launch.loop_status.contains("Saved On"));
        assert!(
            starting_next_launch
                .loop_status
                .contains("Starts next launch")
        );

        let hidden_master = service.state(false, false, true, false, false).projection();
        assert!(!hidden_master.master_enabled);
        assert!(
            hidden_master
                .loop_status
                .contains("Automatic maintenance Saved Off"),
            "the visible master—not the still-On auto-update child—explains the gate"
        );
    }

    /// A failed process must outrank a stale successful status report, update
    /// the initiating Settings feedback, and survive a later silent refresh.
    #[test]
    fn failed_command_cannot_redisplay_an_earlier_success_as_this_attempt() {
        let mut service = PackagesService::new();
        let old_report = PackagesStatusReport::from_parts(
            true,
            true,
            "fp".into(),
            Some(&status("up to date")),
            &[],
        );
        let first = service.begin(None).unwrap();
        assert!(service.finish(first, refresh(old_report.clone())));

        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        let failed = PackagesWorkerCompletion::command(
            old_report.clone(),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "atpkg update exited with status 7".to_string(),
            },
        );
        assert!(service.finish(sequence, failed));
        let projection = service.state(true, true, true, false, true).projection();
        assert_eq!(projection.headline, "Package check failed");
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("exited with status 7")
        );
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap()
                .contains("Earlier recorded status (not this attempt): up to date"),
            "the old success is retained only as explicitly historical context"
        );
        assert!(
            projection
                .command_feedback
                .as_deref()
                .unwrap()
                .starts_with("Package check failed")
        );

        let refresh_sequence = service.begin(None).unwrap();
        assert!(service.finish(refresh_sequence, refresh(old_report)));
        assert_eq!(
            service
                .state(true, true, true, false, true)
                .projection()
                .headline,
            "Package check failed",
            "a background status read cannot erase the last verb result"
        );
    }

    /// Current sequence alone is insufficient: the completion kind/operation
    /// must match the reservation before the reducer may clear busy.
    #[test]
    fn completion_kind_and_operation_must_match_the_reservation() {
        let mut service = PackagesService::new();
        let report = PackagesStatusReport::from_parts(true, true, "fp".into(), None, &[]);
        let sequence = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.finish(sequence, refresh(report.clone())));
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(!service.finish(sequence, succeeded(report.clone(), PackagesBusy::Check)));
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.finish(sequence, succeeded(report, PackagesBusy::Install)));
        assert_eq!(service.busy(), None);
    }

    /// A `status.toml` fixture carrying every canonical §17.2 state: the rows parse
    /// through `atpkg::state`'s own readers (never a second spelling), the text is
    /// kept verbatim, and the grouping falls out — Default set, Extras (with the
    /// authored vendor / license / size), and Needs admin, where Homebrew's
    /// `blocked by clt: needs admin …` row waits on the same door as `clt`.
    #[test]
    fn rows_parse_every_canonical_state_and_group_by_it() {
        let p = std::path::Path::new;
        let fixture: Vec<(&str, String, Option<u64>)> = vec![
            ("trust", atpkg::state::managed(6808, 41), Some(6808)),
            (
                "ay",
                atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
                Some(1971),
            ),
            (
                "gh",
                atpkg::state::system(p("/opt/homebrew/bin/gh"), Some("2026-08-27")),
                None,
            ),
            ("codex", atpkg::state::agent_installing(), None),
            ("claude", atpkg::state::managed(2231, 41), Some(2231)),
            (
                "vendorx",
                atpkg::state::extra_not_installed("vendorx"),
                None,
            ),
            ("clt", atpkg::state::needs_admin("clt"), None),
            (
                "brew",
                atpkg::state::blocked("clt", &atpkg::state::needs_admin("clt")),
                None,
            ),
            (
                "emacs",
                atpkg::state::unavailable("aarch64-pc-windows-msvc", "no Windows-on-ARM build"),
                None,
            ),
            (
                "xc",
                atpkg::state::installed_via("pkg", p("/opt/homebrew/bin/xc")),
                None,
            ),
            ("ny", "error: x".to_string(), None),
        ];
        let mut programs = std::collections::BTreeMap::new();
        for (name, state, build) in &fixture {
            programs.insert(
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: *build,
                    state: state.clone(),
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            updated_at: "2026-08-27T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: "up to date".to_string(),
            seams: Vec::new(),
            last_success_at: String::new(),
            programs,
        };
        let text = status.to_toml().unwrap();
        let round: atpkg::Status = aterm_toml::from_str(&text).expect("the fixture is status.toml");
        let report = PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&round), &[]);
        let row = |name: &str| {
            report
                .programs
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("{name} row"))
        };
        // Verbatim text, every row.
        for (name, state, _) in &fixture {
            assert_eq!(
                &row(name).state,
                state,
                "{name} keeps the canonical spelling"
            );
        }
        assert_eq!(
            row("trust").kind,
            ProgramStateKind::Managed {
                build: 6808,
                index: 41
            }
        );
        assert_eq!(
            row("ay").kind,
            ProgramStateKind::Shadowed {
                build: 1971,
                path: "/Users//dev/.local/bin/ay".into()
            }
        );
        assert_eq!(
            row("gh").kind,
            ProgramStateKind::System {
                path: "/opt/homebrew/bin/gh".into(),
                retired: Some("2026-08-27".into())
            }
        );
        assert_eq!(row("codex").kind, ProgramStateKind::AgentInstalling);
        assert_eq!(row("vendorx").kind, ProgramStateKind::ExtraNotInstalled);
        assert_eq!(row("clt").kind, ProgramStateKind::NeedsAdmin);
        assert_eq!(
            row("brew").kind,
            ProgramStateKind::BlockedBy {
                dep: "clt".into(),
                dep_state: "needs admin — run: aterm pkg install clt".into()
            }
        );
        assert_eq!(row("emacs").kind, ProgramStateKind::Unavailable);
        assert_eq!(
            row("xc").kind,
            ProgramStateKind::InstalledVia {
                protocol: "pkg".into(),
                path: "/opt/homebrew/bin/xc".into()
            }
        );
        assert_eq!(row("ny").kind, ProgramStateKind::Other);

        // Grouping.
        for name in ["trust", "ay", "gh", "emacs", "xc", "ny"] {
            assert_eq!(row(name).group, RowGroup::Default, "{name}");
            assert!(row(name).facts.is_none(), "{name} carries no extra facts");
            assert!(!row(name).offers_extra_install(), "{name}");
        }
        // The agent programs are DEFAULT-SET (owner decision 2026-09-10): the pass
        // installs them unasked, so neither the one still installing nor the one
        // already managed is an extra, and neither offers an Install control.
        assert_eq!(
            row("codex").group,
            RowGroup::Default,
            "an agent program installing"
        );
        assert!(!row("codex").offers_extra_install());
        assert_eq!(
            row("claude").group,
            RowGroup::Default,
            "an agent program installed"
        );
        assert!(!row("claude").offers_extra_install());
        // A real opt-in extra still waits for consent behind the Install control.
        assert_eq!(row("vendorx").group, RowGroup::Extras);
        assert!(row("vendorx").offers_extra_install());
        assert!(
            row("vendorx").facts.is_none(),
            "an unauthored extra carries no facts"
        );
        assert_eq!(row("clt").group, RowGroup::NeedsAdmin);
        assert_eq!(
            row("brew").group,
            RowGroup::NeedsAdmin,
            "blocked behind a needs-admin dependency waits on the same door"
        );
        assert_eq!(
            report.needs_admin(),
            vec!["clt".to_string(), "brew".to_string()],
            "door order: the dependency first"
        );

        // The authored extras facts.
        let codex = row("codex").facts.clone().expect("codex facts");
        assert_eq!(codex.vendor, "OpenAI Codex CLI");
        assert_eq!(codex.license.as_deref(), Some("Apache-2.0"));
        // The authored fact moved to "~110 MB (~290 MB on disk)" in atpkg's
        // stub table (1f6d9332d) without this pin following it.
        assert_eq!(codex.size.as_deref(), Some("~110 MB (~290 MB on disk)"));
        assert_eq!(
            codex.line(),
            "OpenAI Codex CLI  ·  Apache-2.0  ·  ~110 MB (~290 MB on disk)"
        );
        let claude = row("claude").facts.clone().expect("claude facts");
        assert_eq!(claude.vendor, "Anthropic Claude Code");
        assert_eq!(claude.license.as_deref(), Some("proprietary"));
        // Same drift for Claude Code: the stub table says "~200 MB" now.
        assert_eq!(claude.size.as_deref(), Some("~200 MB"));
        let odd = ExtraFacts::parse("Some Tool without the grammar");
        assert_eq!(odd.vendor, "Some Tool without the grammar");
        assert_eq!(odd.license, None);
        assert_eq!(odd.size, None);

        // The projection carries the door order and the platform's door.
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));
        let projection = service.state(true, true, true, false, true).projection();
        assert_eq!(
            projection.needs_admin,
            vec!["clt".to_string(), "brew".to_string()]
        );
        assert_eq!(projection.admin_door, cfg!(target_os = "macos"));
    }

    /// An opt-in marker regroups an installed extra whose name the compiled roster
    /// does not carry (a newer index) — and never touches a needs-admin row.
    #[test]
    fn opt_in_markers_regroup_installed_extras() {
        let mut programs = std::collections::BTreeMap::new();
        for (name, state) in [
            ("newtool", atpkg::state::managed(5, 41)),
            ("clt", atpkg::state::needs_admin("clt")),
        ] {
            programs.insert(
                name.to_string(),
                atpkg::ProgramStatus {
                    installed_build: None,
                    state,
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        let mut report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[]);
        assert_eq!(report.programs[1].group, RowGroup::Default);
        report.mark_extras(["newtool", "clt", "absent"]);
        let newtool = report
            .programs
            .iter()
            .find(|r| r.name == "newtool")
            .unwrap();
        assert_eq!(newtool.group, RowGroup::Extras);
        assert!(
            newtool.facts.is_none(),
            "no authored line for a name the binary predates"
        );
        let clt = report.programs.iter().find(|r| r.name == "clt").unwrap();
        assert_eq!(
            clt.group,
            RowGroup::NeedsAdmin,
            "the door outranks the roster"
        );
    }

    /// The needs-admin order over chains: depth first, then name; a cycle, a dangling
    /// chain and a chain that ends anywhere but `needs admin` are not admin-waiting.
    #[test]
    fn needs_admin_order_walks_blocked_chains_dependency_first() {
        let s = |n: &str, st: String| (n.to_string(), st);
        let na = atpkg::state::needs_admin;
        let rows = vec![
            s("brew", atpkg::state::blocked("clt", &na("clt"))),
            s("clt", na("clt")),
            s(
                "cask",
                atpkg::state::blocked("brew", &atpkg::state::blocked("clt", &na("clt"))),
            ),
            s(
                "ay",
                atpkg::state::blocked("codex", &atpkg::state::extra_not_installed("codex")),
            ),
            s("codex", atpkg::state::extra_not_installed("codex")),
            s(
                "loop-a",
                atpkg::state::blocked("loop-b", "blocked by loop-a: x"),
            ),
            s(
                "loop-b",
                atpkg::state::blocked("loop-a", "blocked by loop-b: x"),
            ),
            s("dangling", atpkg::state::blocked("ghost", "not installed")),
            s("apt-thing", na("apt-thing")),
            s("trust", atpkg::state::managed(1, 1)),
        ];
        assert_eq!(
            needs_admin_order(&rows),
            vec![
                "apt-thing".to_string(),
                "clt".to_string(),
                "brew".to_string(),
                "cask".to_string()
            ]
        );
        // A quoted tail stands in when the dependency has no row of its own.
        let orphan = vec![s("brew", atpkg::state::blocked("clt", &na("clt")))];
        assert_eq!(needs_admin_order(&orphan), vec!["brew".to_string()]);
        assert!(needs_admin_order(&[]).is_empty());
    }

    /// The dismissal rule: never dismissed ⇒ show; the recorded set ⇒ silent; a
    /// changed set (one more name, one fewer, a different order) ⇒ show again; an
    /// empty set is never a card. The marker round-trips through the file helpers.
    #[test]
    fn the_admin_step_is_raised_once_per_distinct_set() {
        let both = vec!["clt".to_string(), "brew".to_string()];
        let only_clt = vec!["clt".to_string()];
        assert!(admin_step_should_show(None, &both));
        assert!(!admin_step_should_show(None, &[]));
        let recorded = admin_step_marker_text(&both);
        assert_eq!(recorded, "clt\nbrew");
        assert!(!admin_step_should_show(Some(&recorded), &both));
        assert!(
            !admin_step_should_show(Some("clt\nbrew\n"), &both),
            "a trailing newline is the same record"
        );
        assert!(admin_step_should_show(Some(&recorded), &only_clt));
        assert!(admin_step_should_show(
            Some(&admin_step_marker_text(&only_clt)),
            &both
        ));
        assert!(
            admin_step_should_show(Some("brew\nclt"), &both),
            "order is part of the set"
        );
        assert!(!admin_step_should_show(Some(&recorded), &[]));

        let dir = std::env::temp_dir().join(format!(
            "aterm-admin-step-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        assert_eq!(
            admin_step_marker_path(Some(&config)),
            Some(dir.join(ADMIN_STEP_MARKER))
        );
        assert_eq!(admin_step_marker_path(None), None);
        let mut programs = std::collections::BTreeMap::new();
        for (name, state) in [
            ("clt", atpkg::state::needs_admin("clt")),
            (
                "brew",
                atpkg::state::blocked("clt", &atpkg::state::needs_admin("clt")),
            ),
            ("trust", atpkg::state::managed(1, 1)),
        ] {
            programs.insert(
                name.to_string(),
                atpkg::ProgramStatus {
                    installed_build: None,
                    state,
                    tree_root: String::new(),
                },
            );
        }
        let status = atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        record_admin_step_dismissal(Some(&config), &both).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(ADMIN_STEP_MARKER)).unwrap(),
            "clt\nbrew"
        );
        assert_eq!(
            admin_step_due(&status, Some(&config)),
            None,
            "declined: silent"
        );
        // clt installs; only brew still waits — a NEW set, so the card is due again.
        let mut moved = status.clone();
        moved.programs.get_mut("clt").unwrap().state =
            atpkg::state::installed_via("softwareupdate", std::path::Path::new("/L/git"));
        moved.programs.get_mut("brew").unwrap().state = atpkg::state::needs_admin("brew");
        assert_eq!(
            admin_step_due(&moved, Some(&config)),
            Some(vec!["brew".to_string()])
        );
        // Nothing waits ⇒ nothing is due, marker or not.
        let mut done = moved.clone();
        done.programs.get_mut("brew").unwrap().state =
            atpkg::state::installed_via("pkg", std::path::Path::new("/opt/homebrew/bin/brew"));
        assert_eq!(admin_step_due(&done, Some(&config)), None);
        assert!(record_admin_step_dismissal(None, &both).is_err());

        // THE MARKER IS A PREFIX MARKER: a re-record replaces the regular file in place
        // (no temp file left behind); an oversized file is not a record; a planted
        // link is neither read nor written through and is left exactly as planted; a
        // directory at the path fails the write closed.
        let marker = dir.join(ADMIN_STEP_MARKER);
        record_admin_step_dismissal(Some(&config), &only_clt).unwrap();
        assert_eq!(read_admin_step_marker(&marker).as_deref(), Some("clt"));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "the exclusive sibling is renamed away, never left beside the marker"
        );
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        std::fs::write(&marker, "x".repeat(MAX_ADMIN_STEP_MARKER_BYTES + 1)).unwrap();
        assert_eq!(
            read_admin_step_marker(&marker),
            None,
            "oversized: not a record"
        );
        assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
        assert!(
            record_admin_step_dismissal(Some(&config), &vec!["x".repeat(65); 80]).is_err(),
            "a set too large for a marker is refused, not truncated"
        );
        std::fs::remove_file(&marker).unwrap();
        #[cfg(unix)]
        {
            let target = dir.join("planted-target");
            std::fs::write(&target, admin_step_marker_text(&both)).unwrap();
            std::os::unix::fs::symlink(&target, &marker).unwrap();
            assert_eq!(
                read_admin_step_marker(&marker),
                None,
                "a link whose target spells the set is still not a record"
            );
            assert_eq!(admin_step_due(&status, Some(&config)), Some(both.clone()));
            let refused = record_admin_step_dismissal(Some(&config), &both).unwrap_err();
            assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
            assert!(
                std::fs::symlink_metadata(&marker)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the planted link is left alone"
            );
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                admin_step_marker_text(&both),
                "nothing was written through the link"
            );
            std::fs::remove_file(&marker).unwrap();
            std::fs::remove_file(&target).unwrap();
        }
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(read_admin_step_marker(&marker), None);
        assert!(record_admin_step_dismissal(Some(&config), &both).is_err());
        assert!(std::fs::symlink_metadata(&marker).unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The card's copy names the vendor and the admin prompt honestly, in the notice
    /// grammar, in door order.
    #[test]
    fn the_admin_caption_names_the_vendors_and_the_prompt() {
        let both = vec!["clt".to_string(), "brew".to_string()];
        assert_eq!(
            admin_step_caption(&both),
            "\u{2699} Admin step waiting \u{2014} Apple Command Line Tools (Apple's installer, via softwareupdate), then Homebrew (its signed installer package) need an administrator; Install opens macOS's own password dialog"
        );
        assert_eq!(
            admin_step_caption(&["clt".to_string()]),
            "\u{2699} Admin step waiting \u{2014} Apple Command Line Tools (Apple's installer, via softwareupdate) needs an administrator; Install opens macOS's own password dialog"
        );
        assert_eq!(admin_vendor_line("newpkg"), "newpkg (its own installer)");
    }

    /// Dev-linked programs surface annotated — merged into an existing status
    /// row when atpkg recorded one, appended as `linked` when it never did —
    /// and the row list stays name-sorted for stable paint.
    #[test]
    fn report_merges_dev_links_into_the_program_rows() {
        let links = vec![
            (
                "ay".to_string(),
                Some(std::path::PathBuf::from("/Users//x/ay")),
            ),
            ("orc".to_string(), None),
        ];
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &links);
        assert_eq!(report.programs.len(), 2);
        let ay = report.programs.iter().find(|row| row.name == "ay").unwrap();
        assert_eq!(ay.installed_build, Some(1971), "status row kept");
        assert_eq!(ay.annotation.as_deref(), Some("dev-link → /Users//x/ay"));
        let orc = report
            .programs
            .iter()
            .find(|row| row.name == "orc")
            .unwrap();
        assert_eq!(orc.state, "linked");
        assert_eq!(orc.installed_build, None);
        assert!(report.programs.windows(2).all(|w| w[0].name <= w[1].name));
    }

    /// A hostile/corrupt metadata source settles the worker into an explicit,
    /// non-busy error projection; it can never strand Packages on “Reading”.
    #[test]
    fn metadata_admission_error_completes_into_visible_nonreading_state() {
        let mut service = PackagesService::new();
        let sequence = service.begin(None).unwrap();
        let mut report = PackagesStatusReport::from_parts(true, true, "fp".into(), None, &[]);
        report.collection_error = Some(
            "Could not read status.toml: package metadata is not a regular non-link file"
                .to_string(),
        );
        assert!(service.finish(sequence, refresh(report)));

        let projection = service.state(true, true, true, false, true).projection();
        assert_eq!(projection.headline, "Package status incomplete");
        assert!(!projection.refreshing);
        assert!(!projection.actions_enabled);
        assert!(
            projection
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("regular non-link"),
            "the checked-read diagnostic reaches the native Settings projection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_collector_turns_status_fifo_into_a_completed_error_report() {
        use std::os::unix::ffi::OsStrExt as _;

        let prefix =
            std::env::temp_dir().join(format!("aterm-packages-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let layout = atpkg::Layout {
            prefix: prefix.clone(),
        };
        let status = layout.status();
        let status_c = std::ffi::CString::new(status.as_os_str().as_bytes()).unwrap();
        // SAFETY: `status_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(status_c.as_ptr(), 0o600) }, 0);

        let report = collect_packages_status_from_layout(true, Some(&layout));
        assert!(
            report
                .collection_error
                .as_deref()
                .unwrap_or("")
                .contains("regular non-link"),
            "the production collector surfaces the FIFO refusal"
        );
        let mut service = PackagesService::new();
        let sequence = service.begin(None).unwrap();
        assert!(service.finish(sequence, refresh(report)));
        let projection = service.state(true, true, true, false, true).projection();
        assert!(!projection.refreshing);
        assert!(!projection.headline.contains("Reading"));
        let _ = std::fs::remove_dir_all(prefix);
    }

    fn machine_state(
        uc: atpkg::machine::UcPosture,
        would_migrate: usize,
        home: atpkg::machine::HomePosture,
    ) -> atpkg::machine::MachineState {
        atpkg::machine::MachineState {
            universal_control: uc,
            policy: atpkg::config::UniversalControlPolicy::Off,
            spotlight_noindex: true,
            exposed: would_migrate + 1,
            hidden: 8,
            would_migrate,
            scan_complete: true,
            home,
        }
    }

    /// The "This Mac" card reads the machine posture through the packages
    /// projection: unobserved says nothing and offers no button; a read bumps the
    /// revision and renders the measured words; `Apply now` follows
    /// `MachineState::next` (a home mismatch or nothing-left disables it); a
    /// recorded change and an apply verdict are quoted with their age.
    #[test]
    fn projection_carries_the_machine_posture() {
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = PackagesService::new();
        let base = service.revision();
        let unobserved = service.state(true, true, true, false, true).projection();
        assert!(!unobserved.machine.observed);
        assert!(!unobserved.machine.apply_enabled);
        assert!(!unobserved.machine.nothing_to_apply);
        assert_eq!(
            unobserved.machine.universal_control,
            "Universal Control: not read yet"
        );
        assert_eq!(
            unobserved.machine.last_change,
            "No change recorded this launch"
        );
        assert_eq!(unobserved.machine.supported, cfg!(target_os = "macos"));

        // The page must be observed and idle before any verb — the machine card
        // shares that gate, so seed a live report first.
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));

        service.set_machine_refreshing(true);
        let refreshing = service.state(true, true, true, false, true).projection();
        assert!(refreshing.machine.refreshing);
        assert!(!refreshing.machine.apply_enabled);

        let before = service.revision();
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        assert!(service.revision() > before, "a read fans out");
        assert!(!service.machine().refreshing, "a read ends the refresh");
        let now = std::time::SystemTime::now();
        let live = service
            .state(true, true, true, false, true)
            .projection_at(now);
        assert!(live.machine.observed);
        assert_eq!(
            live.machine.universal_control,
            "Universal Control: at the OS default — the cursor roams to other Macs and iPads"
        );
        assert_eq!(
            live.machine.spotlight,
            "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide"
        );
        assert!(live.machine.reason.is_none());
        assert_eq!(live.machine.apply_enabled, cfg!(target_os = "macos"));
        assert!(!live.machine.nothing_to_apply);
        assert_eq!(
            live.machine.next,
            "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight"
        );
        assert!(live.machine.saved.is_none(), "saved == measured ⇒ no note");
        // A switch flipped in Settings after the read is named, never shown as
        // already measured.
        let flipped = service
            .state(true, true, true, false, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Leave, false)
            .projection_at(now);
        assert_eq!(
            flipped.machine.saved.as_deref(),
            Some(
                "Saved since the last read: universal_control = \"leave\", spotlight_noindex = false — the next package pass or Apply now uses it"
            )
        );

        // A recorded change carries its age; a verdict is quoted verbatim.
        service.note_machine_change(
            "universal-control disabled".to_string(),
            now - std::time::Duration::from_secs(120),
        );
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(
            service.finish(
                seq,
                succeeded(
                    PackagesStatusReport::from_parts(
                        true,
                        true,
                        "fp".into(),
                        Some(&status("ok")),
                        &[]
                    ),
                    PackagesBusy::MachineApply,
                )
                .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
            )
        );
        let changed = service
            .state(true, true, true, false, true)
            .projection_at(now);
        assert_eq!(
            changed.machine.last_change,
            "Last change: universal-control disabled · 2m ago"
        );
        assert_eq!(
            changed.machine.last_verdict.as_deref(),
            Some("applied — universal-control disabled")
        );
        assert_eq!(changed.headline, "Machine settings applied");
        assert_eq!(
            changed.command_feedback.as_deref(),
            Some("Machine settings: applied — universal-control disabled")
        );

        // Nothing left to do: the button is off and the card says so.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        )));
        let done = service
            .state(true, true, true, false, true)
            .projection_at(now);
        assert_eq!(
            done.machine.universal_control,
            "Universal Control: disabled on this Mac"
        );
        assert!(!done.machine.apply_enabled);
        assert!(done.machine.nothing_to_apply);
        assert!(
            done.machine.next.starts_with("Nothing to apply"),
            "{}",
            done.machine.next
        );

        // A home mismatch disables the button and names the reason.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Mismatch,
        )));
        let mismatch = service
            .state(true, true, true, false, true)
            .projection_at(now);
        assert!(!mismatch.machine.apply_enabled);
        assert!(
            !mismatch.machine.nothing_to_apply,
            "a mismatch is not \"nothing to apply\""
        );
        assert!(mismatch.machine.next.is_empty());
        assert!(
            mismatch
                .machine
                .reason
                .as_deref()
                .is_some_and(|r| r.starts_with("Not applied here")),
            "{:?}",
            mismatch.machine.reason
        );

        // With the switch OFF a pass hides nothing, whatever the scan counted: the
        // sentence prints what an apply CAN hide (the CLI's `can_hide`), and with
        // the saved switch off too there is nothing left to apply.
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            spotlight_noindex: false,
            ..machine_state(UcPosture::Disabled, 2, HomePosture::Account)
        }));
        let switched_off = service
            .state(true, true, true, false, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Off, false)
            .projection_at(now);
        assert_eq!(
            switched_off.machine.spotlight,
            "Build output: 8 target dirs hidden, 3 open to Spotlight — 0 a pass would hide · switched off ([machine] spotlight_noindex = false)"
        );
        assert!(switched_off.machine.nothing_to_apply);
        assert!(
            switched_off.machine.next.starts_with("Nothing to apply"),
            "{}",
            switched_off.machine.next
        );
        assert!(switched_off.machine.saved.is_none());

        // A read error AFTER a good record: observed, the last record is kept but
        // every measured sentence says it is prior, the verdict line is silent and
        // the button is off — a stale record never stands in for a measured one.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        let _ = service.replace_machine_state(Err("atpkg machine printed no state".to_string()));
        let errored = service
            .state(true, true, true, false, true)
            .projection_at(now);
        assert!(errored.machine.observed);
        assert!(!errored.machine.apply_enabled);
        assert!(!errored.machine.nothing_to_apply);
        assert!(errored.machine.next.is_empty(), "{}", errored.machine.next);
        assert_eq!(
            errored.machine.reason.as_deref(),
            Some("Could not read the machine state: atpkg machine printed no state")
        );
        assert!(
            errored
                .machine
                .universal_control
                .ends_with("(from the last successful read)"),
            "{}",
            errored.machine.universal_control
        );
        assert!(
            errored
                .machine
                .spotlight
                .ends_with("(from the last successful read)"),
            "{}",
            errored.machine.spotlight
        );
        assert!(
            service.machine().state.is_some(),
            "a failed read never erases the last measurement"
        );
        let _ = base;
    }

    /// `atpkg machine apply` is a LOCAL verb: the card's Apply now must not inherit
    /// the package page's manager gate. With the manager switched off
    /// (ATPKG_DISABLE / no pinned root key) or the status collection torn, the
    /// package verbs are off but the machine verdict and its button stay live.
    #[test]
    fn machine_apply_ignores_the_manager_gate() {
        use atpkg::machine::{HomePosture, UcPosture};
        let manager_off =
            PackagesStatusReport::from_parts(true, false, "fp".into(), Some(&status("ok")), &[]);
        let mut torn =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        torn.collection_error = Some("status.toml: torn record".to_string());
        for (report, why) in [(manager_off, "manager off"), (torn, "collection error")] {
            let mut service = PackagesService::new();
            let seq = service.begin(None).unwrap();
            assert!(service.finish(seq, refresh(report)));
            let _ = service.replace_machine_state(Ok(machine_state(
                UcPosture::Default,
                2,
                HomePosture::Account,
            )));
            let p = service.state(true, true, true, false, true).projection();
            assert!(!p.actions_enabled, "{why}: the package verbs are gated");
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos"),
                "{why}: the machine verb is not"
            );
            assert!(!p.machine.nothing_to_apply, "{why}");
            assert!(
                p.machine.next.starts_with("Apply now would set"),
                "{why}: {}",
                p.machine.next
            );
        }
    }

    /// The apply child reads the SAVED `[machine]` switches, so the verdict and the
    /// button follow measured posture × saved switches — never the record's stale
    /// `policy=`/`noindex=` — in both directions.
    #[test]
    fn flipped_machine_switch_drives_the_apply_verdict_not_the_stale_record() {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("ok")),
                &[]
            )),
        ));
        // Direction B: the record says the pass would act; the user then saved
        // "leave" and switched Spotlight off — an apply would now do nothing.
        let _ = service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        )));
        let b = service
            .state(true, true, true, false, true)
            .with_machine_config(UniversalControlPolicy::Leave, false)
            .projection();
        assert!(!b.machine.apply_enabled);
        assert!(b.machine.nothing_to_apply);
        assert!(
            b.machine.next.starts_with("Nothing to apply"),
            "{}",
            b.machine.next
        );
        assert!(
            b.machine
                .saved
                .as_deref()
                .is_some_and(|s| s.contains("universal_control = \"leave\"")
                    && s.contains("spotlight_noindex = false")),
            "{:?}",
            b.machine.saved
        );
        // Direction A: the record was read under leave/off; the user then saved
        // "off" and switched Spotlight on — an apply WOULD act, so say so.
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            policy: UniversalControlPolicy::Leave,
            spotlight_noindex: false,
            ..machine_state(UcPosture::Default, 2, HomePosture::Account)
        }));
        let a = service
            .state(true, true, true, false, true)
            .with_machine_config(UniversalControlPolicy::Off, true)
            .projection();
        assert_eq!(a.machine.apply_enabled, cfg!(target_os = "macos"));
        assert!(!a.machine.nothing_to_apply);
        assert_eq!(
            a.machine.next,
            "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight"
        );
        assert!(
            a.machine
                .saved
                .as_deref()
                .is_some_and(|s| s.contains("universal_control = \"off\"")
                    && s.contains("spotlight_noindex = true")),
            "{:?}",
            a.machine.saved
        );
    }

    /// A read asked for while one is running is QUEUED behind it, never joined:
    /// the running read may have started before an apply and would land as a
    /// pre-apply record. The posture stays refreshing (Apply off, "Reading…")
    /// until the rerun lands.
    #[test]
    fn a_read_in_flight_when_the_apply_finishes_is_rerun_not_joined() {
        use atpkg::machine::{HomePosture, UcPosture};
        let report =
            || PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report())));
        assert!(service.request_machine_read(), "idle ⇒ the caller spawns");
        assert!(service.machine().refreshing);
        assert!(!service.machine().rerun);

        // An apply runs and finishes while that read is still out.
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(
            service.finish(
                seq,
                succeeded(report(), PackagesBusy::MachineApply)
                    .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
            )
        );
        assert!(
            !service.request_machine_read(),
            "a read in flight is queued, not joined"
        );
        assert!(service.machine().refreshing);
        assert!(service.machine().rerun);

        // The STALE read lands: the rerun is handed back to the caller, and the
        // card keeps reading — the stale record never enables Apply.
        assert!(service.replace_machine_state(Ok(machine_state(
            UcPosture::Default,
            2,
            HomePosture::Account,
        ))));
        assert!(!service.machine().rerun, "handed back exactly once");
        let stale = service.state(true, true, true, false, true).projection();
        assert!(stale.machine.observed);
        assert!(stale.machine.refreshing);
        assert!(!stale.machine.apply_enabled);

        // The rerun lands: fresh record, nothing pending, the verdict is final.
        assert!(!service.replace_machine_state(Ok(machine_state(
            UcPosture::Disabled,
            0,
            HomePosture::Account,
        ))));
        let fresh = service.state(true, true, true, false, true).projection();
        assert!(!fresh.machine.refreshing);
        assert!(fresh.machine.nothing_to_apply);
        assert!(!fresh.machine.apply_enabled);
    }

    /// Every posture word the card can say, pinned from the record: the two
    /// half/unknown Universal Control states, the `leave` suffix (and its absence
    /// once disabled), the scan-budget suffix, the switched-off Spotlight sentence
    /// and the unresolved-home reason — with the verdict each one yields.
    #[test]
    fn machine_projection_words_cover_every_posture() {
        use atpkg::config::UniversalControlPolicy::{self, Leave, Off};
        use atpkg::machine::{HomePosture, MachineState, UcPosture};
        struct Row {
            name: &'static str,
            state: MachineState,
            saved: (UniversalControlPolicy, bool),
            universal_control: &'static str,
            spotlight: &'static str,
            next: &'static str,
            reason_starts: Option<&'static str>,
            nothing_to_apply: bool,
            apply_on_macos: bool,
        }
        let rows = [
            Row {
                name: "partial",
                state: machine_state(UcPosture::Partial, 2, HomePosture::Account),
                saved: (Off, true),
                universal_control: "Universal Control: partly disabled",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide",
                next: "Apply now would set: Universal Control off for this host; 2 target dir(s) hidden from Spotlight",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "unknown",
                state: machine_state(UcPosture::Unknown, 0, HomePosture::Account),
                saved: (Off, true),
                universal_control: "Universal Control: unknown",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                next: "Apply now would set: Universal Control off for this host",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "default, left alone",
                state: MachineState {
                    policy: Leave,
                    ..machine_state(UcPosture::Default, 0, HomePosture::Account)
                },
                saved: (Leave, true),
                universal_control: "Universal Control: at the OS default — the cursor roams to other Macs and iPads · left alone ([machine] universal_control = \"leave\")",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                next: "Nothing to apply — Universal Control and Spotlight are where [machine] wants them",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "disabled, leave says nothing extra",
                state: MachineState {
                    policy: Leave,
                    ..machine_state(UcPosture::Disabled, 0, HomePosture::Account)
                },
                saved: (Leave, true),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 1 open to Spotlight — 0 a pass would hide",
                next: "Nothing to apply — Universal Control and Spotlight are where [machine] wants them",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "scan hit its budget",
                state: MachineState {
                    scan_complete: false,
                    ..machine_state(UcPosture::Disabled, 1, HomePosture::Account)
                },
                saved: (Off, true),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 2 open to Spotlight — 1 a pass would hide (at least — the scan hit its budget)",
                next: "Apply now would set: 1 target dir(s) hidden from Spotlight",
                reason_starts: None,
                nothing_to_apply: false,
                apply_on_macos: true,
            },
            Row {
                name: "spotlight switched off",
                state: MachineState {
                    spotlight_noindex: false,
                    ..machine_state(UcPosture::Disabled, 2, HomePosture::Account)
                },
                saved: (Off, false),
                universal_control: "Universal Control: disabled on this Mac",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 0 a pass would hide · switched off ([machine] spotlight_noindex = false)",
                next: "Nothing to apply — Universal Control and Spotlight are where [machine] wants them",
                reason_starts: None,
                nothing_to_apply: true,
                apply_on_macos: false,
            },
            Row {
                name: "home unresolved",
                state: machine_state(UcPosture::Default, 2, HomePosture::Unresolved),
                saved: (Off, true),
                universal_control: "Universal Control: at the OS default — the cursor roams to other Macs and iPads",
                spotlight: "Build output: 8 target dirs hidden, 3 open to Spotlight — 2 a pass would hide",
                next: "",
                reason_starts: Some("Not applied here: the account home could not be resolved"),
                nothing_to_apply: false,
                apply_on_macos: false,
            },
        ];
        for row in rows {
            let mut service = PackagesService::new();
            let seq = service.begin(None).unwrap();
            assert!(service.finish(
                seq,
                refresh(PackagesStatusReport::from_parts(
                    true,
                    true,
                    "fp".into(),
                    Some(&status("ok")),
                    &[]
                )),
            ));
            let _ = service.replace_machine_state(Ok(row.state));
            let p = service
                .state(true, true, true, false, true)
                .with_machine_config(row.saved.0, row.saved.1)
                .projection();
            assert_eq!(
                p.machine.universal_control, row.universal_control,
                "{}",
                row.name
            );
            assert_eq!(p.machine.spotlight, row.spotlight, "{}", row.name);
            assert_eq!(p.machine.next, row.next, "{}", row.name);
            match row.reason_starts {
                Some(prefix) => assert!(
                    p.machine
                        .reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with(prefix)),
                    "{}: {:?}",
                    row.name,
                    p.machine.reason
                ),
                None => assert!(
                    p.machine.reason.is_none(),
                    "{}: {:?}",
                    row.name,
                    p.machine.reason
                ),
            }
            assert_eq!(
                p.machine.nothing_to_apply, row.nothing_to_apply,
                "{}",
                row.name
            );
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos") && row.apply_on_macos,
                "{}",
                row.name
            );
            assert!(p.machine.saved.is_none(), "{}: saved == record", row.name);
        }
    }
}
