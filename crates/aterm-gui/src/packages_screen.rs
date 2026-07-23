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
    /// The co-located CLI's OWN posture, mirrored: a root anchor exists (the
    /// compiled pin, or `ATPKG_ROOTKEY_OVERRIDE` — which the CLI honors) and
    /// `ATPKG_DISABLE` is unset. False ⇒ the manager is inert by construction.
    pub(crate) manager_enabled: bool,
    /// `ATPKG_DISABLE` was set at collection time — the inert cause is the
    /// user's opt-out, NOT a missing key; the page must say the true reason.
    pub(crate) disabled_by_env: bool,
    /// The root anchor is `ATPKG_ROOTKEY_OVERRIDE` rather than the compiled
    /// pin (the CLI's status calls this "root key via ATPKG_ROOTKEY_OVERRIDE").
    pub(crate) override_root: bool,
    /// The pinned root key's operator-facing fingerprint (the doctor line).
    pub(crate) root_fingerprint: String,
    /// `status.toml` existed and parsed (atpkg has run at least once).
    pub(crate) recorded: bool,
    /// `status.toml` fields (empty when `recorded` is false).
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
}

impl PackagesStatusReport {
    /// The honest zero state before any worker observation: nothing is claimed
    /// available or enabled until a collection pass has actually looked.
    fn unobserved() -> Self {
        Self {
            available: false,
            manager_enabled: false,
            disabled_by_env: false,
            override_root: false,
            root_fingerprint: String::new(),
            recorded: false,
            updated_at: String::new(),
            outcome: String::new(),
            index_source: String::new(),
            programs: Vec::new(),
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
            override_root: false,
            root_fingerprint,
            recorded: status.is_some(),
            updated_at: status.map(|s| s.updated_at.clone()).unwrap_or_default(),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            index_source: status.map(|s| s.index_source.clone()).unwrap_or_default(),
            programs,
        }
    }
}

/// Collect the packages report from the real machine — filesystem reads only
/// (no network, no subprocess). MUST run off the event loop (worker threads
/// only): it stats the co-located binary and parses `status.toml`.
pub(crate) fn collect_packages_status(available: bool) -> PackagesStatusReport {
    let layout = atpkg::store::resolve(None);
    let status = layout.as_ref().and_then(atpkg::status::read);
    let links: Vec<(String, Option<std::path::PathBuf>)> = layout
        .as_ref()
        .map(|layout| {
            atpkg::linked_programs(layout)
                .into_iter()
                .map(|name| {
                    let target = atpkg::linked_checkout(layout, &name);
                    (name, target)
                })
                .collect()
        })
        .unwrap_or_default();
    // Mirror the CO-LOCATED CLI's own posture, not just the compiled pin:
    // `ATPKG_ROOTKEY_OVERRIDE` (which the CLI honors as the root anchor) can
    // enable an unpinned build, and `ATPKG_DISABLE` inerts a pinned one — the
    // page must report the cause that is actually in force.
    let disabled_by_env = std::env::var_os("ATPKG_DISABLE").is_some();
    let override_root = std::env::var("ATPKG_ROOTKEY_OVERRIDE").is_ok_and(|k| !k.is_empty());
    let manager_enabled = atpkg::enabled() || (override_root && !disabled_by_env);
    let mut report = PackagesStatusReport::from_parts(
        available,
        manager_enabled,
        atpkg::root_key_fingerprint(),
        status.as_ref(),
        &links,
    );
    report.disabled_by_env = disabled_by_env;
    report.override_root = override_root;
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
}

impl PackagesService {
    pub(crate) fn new() -> Self {
        Self {
            revision: 1,
            sequence: 0,
            inflight: false,
            busy: None,
            report: None,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn busy(&self) -> Option<PackagesBusy> {
        self.busy
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
        self.revision = self.revision.saturating_add(1);
        Some(self.sequence)
    }

    /// Reduce one worker completion. `false` ⇒ stale (reducer-inert means
    /// presentation-inert too: the caller must not publish).
    pub(crate) fn finish(&mut self, sequence: u64, report: PackagesStatusReport) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        self.report = Some(report);
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
        auto_update: bool,
        auto_install: bool,
    ) -> PackagesState {
        PackagesState {
            observed: self.report.is_some(),
            report: self
                .report
                .clone()
                .unwrap_or_else(PackagesStatusReport::unobserved),
            busy: self.busy,
            inflight: self.inflight,
            loop_enabled,
            auto_update,
            auto_install,
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
    /// Resolved `[packages]` flags (`enabled && auto_update`, `auto_update`,
    /// `auto_install`) — display only; the switches edit the config keys.
    loop_enabled: bool,
    auto_update: bool,
    auto_install: bool,
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
            loop_enabled: true,
            auto_update: true,
            auto_install: false,
        }
    }

    /// Snapshot the exact state the Packages page paints.
    pub(crate) fn projection(&self) -> PackagesProjection {
        let report = &self.report;
        let actions_enabled =
            self.observed && report.available && report.manager_enabled && !self.inflight;
        let headline = if !self.observed {
            "Reading package status…".to_string()
        } else if let Some(busy) = self.busy {
            match busy {
                PackagesBusy::Check => "Checking toolchain packages…".to_string(),
                PackagesBusy::Install => "Installing the ALab toolset…".to_string(),
            }
        } else if !report.available {
            "Package manager unavailable".to_string()
        } else if !report.manager_enabled {
            "Package manager inert".to_string()
        } else if report.recorded {
            "Toolchain packages managed".to_string()
        } else {
            "No package activity yet".to_string()
        };
        let detail = if !self.observed {
            None
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
        } else {
            Some("atpkg has not run yet — check now to record a first status.".to_string())
        };
        PackagesProjection {
            observed: self.observed,
            available: report.available,
            manager_enabled: report.manager_enabled,
            // Under an override the compiled pin's digest is NOT the live
            // anchor — say what the CLI's own status says instead of showing
            // a fingerprint of a key that is not in force.
            root_fingerprint: if report.override_root {
                "root key via ATPKG_ROOTKEY_OVERRIDE".to_string()
            } else {
                report.root_fingerprint.clone()
            },
            recorded: report.recorded,
            updated_at: report.updated_at.clone(),
            outcome: report.outcome.clone(),
            index_source: report.index_source.clone(),
            programs: report.programs.clone(),
            busy: self.busy,
            refreshing: self.inflight && self.busy.is_none(),
            loop_enabled: self.loop_enabled,
            auto_update: self.auto_update,
            auto_install: self.auto_install,
            actions_enabled,
            headline,
            detail,
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
    pub(crate) root_fingerprint: String,
    pub(crate) recorded: bool,
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    pub(crate) busy: Option<PackagesBusy>,
    /// A SILENT status-refresh worker is inflight (no `busy` label). Action
    /// admission must refuse during this brief window too — the host runs one
    /// worker at a time, and a raced click would otherwise be dropped.
    pub(crate) refreshing: bool,
    pub(crate) loop_enabled: bool,
    pub(crate) auto_update: bool,
    pub(crate) auto_install: bool,
    /// Both action buttons: available AND enabled AND idle AND observed.
    pub(crate) actions_enabled: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            !service.finish(seq + 1, report.clone()),
            "a superseded worker cannot import facts"
        );
        assert_eq!(
            service.busy(),
            Some(PackagesBusy::Check),
            "stale finish is inert"
        );
        let before = service.revision();
        assert!(service.finish(seq, report));
        assert_eq!(service.busy(), None);
        assert!(service.revision() > before);
        let state = service.state(true, true, false);
        let projection = state.projection();
        assert!(projection.recorded);
        assert_eq!(projection.programs.len(), 1);
        assert_eq!(projection.programs[0].installed_build, Some(1971));
        assert!(projection.actions_enabled);
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
        let state = service.state(true, true, false);
        assert!(
            state.projection().headline.contains("Reading"),
            "never-observed stays honestly unobserved after an abort"
        );

        // Observed facts survive a later abort untouched.
        let seq = service.begin(Some(PackagesBusy::Check)).unwrap();
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(service.finish(seq, report));
        let seq = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.abort(seq + 1), "stale abort is inert");
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.abort(seq));
        let projection = service.state(true, true, false).projection();
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
            PackagesStatusReport::from_parts(false, false, String::new(), None, &[]),
        ));
        let unavailable = service.state(true, true, false).projection();
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
            PackagesStatusReport::from_parts(true, false, "0000".into(), None, &[]),
        ));
        let inert = service.state(true, true, false).projection();
        assert!(inert.headline.contains("inert"));
        assert!(
            inert.detail.as_deref().unwrap_or("").contains("root key"),
            "the doctor line names the missing trust anchor"
        );
        assert!(!inert.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("up to date")),
                &[]
            ),
        ));
        let live = service.state(true, true, false).projection();
        assert!(live.actions_enabled);
        assert!(live.detail.as_deref().unwrap().contains("up to date"));

        // Busy while a verb runs: actions gate off and the headline says which.
        let _ = service.begin(Some(PackagesBusy::Install)).unwrap();
        let busy = service.state(true, true, false).projection();
        assert!(busy.headline.contains("Installing"));
        assert!(!busy.actions_enabled);
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
}
