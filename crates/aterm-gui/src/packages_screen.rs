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
}

impl PackagesBusy {
    fn completed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check completed",
            Self::Install => "ALab toolset install completed",
            Self::Uninstall => "ALab toolset removed",
            Self::InstallExtra => "Extra install completed",
            Self::InstallAdmin => "Admin install completed",
        }
    }

    fn failed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check failed",
            Self::Install => "ALab toolset install failed",
            Self::Uninstall => "ALab toolset removal failed",
            Self::InstallExtra => "Extra install failed",
            Self::InstallAdmin => "Admin install failed",
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
}

impl PackagesWorkerCompletion {
    pub(crate) fn refresh(report: PackagesStatusReport) -> Self {
        Self {
            report,
            command: None,
        }
    }

    pub(crate) fn command(report: PackagesStatusReport, command: PackagesCommandOutcome) -> Self {
        Self {
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
            facts: (group == RowGroup::Extras)
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
        if matches!(kind, ProgramStateKind::ExtraNotInstalled) || atpkg::stub::compiled_extra(name)
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
/// `optin_exists`). A planted link, a directory, an oversized or non-UTF-8 file is not a
/// dismissal this machine recorded: `None`, and the card shows.
pub(crate) fn read_admin_step_marker(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    // The handle is opened without following a final-component link and checked AS A
    // HANDLE, not by a separate stat something could swap under: the same boundary the
    // theme files use (`app_config::open_regular_theme_file`).
    let file = crate::app_config::open_regular_theme_file(path).ok()?;
    let mut bytes = Vec::with_capacity(256);
    file.take((MAX_ADMIN_STEP_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_ADMIN_STEP_MARKER_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Record "Not now" for `names`. Best-effort: an unwritable config dir means the card
/// comes back next pass, which errs toward disclosure.
///
/// Never writes THROUGH anything already at the marker path: the text lands in a fresh
/// sibling created exclusively (`create_new`, owner-only on unix) and is renamed over
/// the marker — a rename replaces a planted link rather than following it — and a
/// non-regular occupant (a link, a directory) fails closed and is left alone, exactly as
/// atpkg's `record_retired`/`record_optin` refuse an occupied marker path.
pub(crate) fn record_admin_step_dismissal(
    config_path: Option<&std::path::Path>,
    names: &[String],
) -> std::io::Result<()> {
    use std::io::Write as _;
    let Some(path) = admin_step_marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the dismissal in",
        ));
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "dismissal marker path is occupied by something that is not a marker",
            ));
        }
        Err(_) => {}
    }
    let text = admin_step_marker_text(names);
    if text.len() > MAX_ADMIN_STEP_MARKER_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "dismissal set is larger than a marker may hold",
        ));
    }
    let tmp = path.with_file_name(format!("{ADMIN_STEP_MARKER}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
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
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn busy(&self) -> Option<PackagesBusy> {
        self.busy
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
}

impl PackagesState {
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
        }
    }

    /// Snapshot the exact state the Packages page paints.
    pub(crate) fn projection(&self) -> PackagesProjection {
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
        let mut detail = if !self.observed {
            None
        } else if let Some(command) = self.last_command.as_ref() {
            let mut detail = command.feedback();
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
        let command_feedback = self
            .last_command
            .as_ref()
            .map(PackagesCommandOutcome::feedback);
        PackagesProjection {
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
}

/// Owned, structured read projection shared with the native Settings
/// `/packages` route (the [`crate::update_screen::UpdateProjection`] analogue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProjection {
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
            ("codex", atpkg::state::extra_not_installed("codex"), None),
            ("claude", atpkg::state::managed(2231, 41), Some(2231)),
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
        assert_eq!(row("codex").kind, ProgramStateKind::ExtraNotInstalled);
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
        assert_eq!(row("codex").group, RowGroup::Extras);
        assert!(row("codex").offers_extra_install());
        assert_eq!(
            row("claude").group,
            RowGroup::Extras,
            "an installed extra is still an extra (the compiled roster says so)"
        );
        assert!(
            !row("claude").offers_extra_install(),
            "an installed extra offers no Install"
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
        assert_eq!(codex.size.as_deref(), Some("~90 MB"));
        assert_eq!(codex.line(), "OpenAI Codex CLI  ·  Apache-2.0  ·  ~90 MB");
        let claude = row("claude").facts.clone().expect("claude facts");
        assert_eq!(claude.vendor, "Anthropic Claude Code");
        assert_eq!(claude.license.as_deref(), Some("proprietary"));
        assert_eq!(claude.size.as_deref(), Some("~230 MB"));
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
}
