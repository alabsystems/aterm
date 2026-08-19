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
}

impl PackagesBusy {
    fn completed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check completed",
            Self::Install => "ALab toolset install completed",
            Self::Uninstall => "ALab toolset removed",
        }
    }

    fn failed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check failed",
            Self::Install => "ALab toolset install failed",
            Self::Uninstall => "ALab toolset removal failed",
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
    /// atpkg's free-text state line (`"active"`, `"tombstoned: …"`, …).
    pub(crate) state: String,
    /// Source annotation: `Some("dev-link → /path")` for a dev-linked checkout.
    pub(crate) annotation: Option<String>,
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
                    .map(|(name, program)| PackagesProgramRow {
                        name: name.clone(),
                        installed_build: program.installed_build,
                        state: program.state.clone(),
                        annotation: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
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
    let layout = atpkg::store::resolve(None);
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
        };
        let (last_operation, last_result) = match self.last_command.as_ref() {
            None => (0, 0),
            Some(PackagesCommandOutcome::Succeeded { operation }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                },
                1,
            ),
            Some(PackagesCommandOutcome::Failed { operation, .. }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
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
