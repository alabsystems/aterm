// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Attachment activation belongs to the exact accepted Caps setting, beyond stage completion.

use super::*;
use aterm_machines::init::KeyboardAttachmentActivation;

/// Current Caps authority, or a pending setting whose hardware facts may wake its worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSetupScope {
    /// No current Caps setting has work this observer may do.
    Inactive,
    /// Caps was verified or carried; only this revision admits attachment activation.
    Applied(u64),
    /// Caps is still owed; this revision permits observations and scoped coordinator resumption.
    Pending(u64),
}

/// One bounded, canonical set of native keyboard service identities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputServiceSet(Vec<u64>);

impl InputServiceSet {
    /// Normalize one native census, refusing an ambiguous or unbounded observation.
    ///
    /// # Errors
    /// More than 4096 services, a zero identity, or a duplicate identity.
    pub fn new(mut ids: Vec<u64>) -> Result<Self, String> {
        if ids.len() > 4096 || ids.contains(&0) {
            return Err("invalid keyboard service identities".into());
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate keyboard service identity".into());
        }
        Ok(Self(ids))
    }

    /// Observe current membership and the services whose complete native state is readable.
    /// Readability is a wake fact only; it never certifies a mapping or admits a setter.
    ///
    /// # Errors
    /// The observation has unbounded, zero or duplicate service identities.
    pub fn observed(
        rows: &[aterm_machines::port::KeyboardServiceObservation],
    ) -> Result<(Self, Self), String> {
        let present = Self::new(rows.iter().map(|row| row.registry()).collect())?;
        let readable = Self::new(
            rows.iter()
                .filter(|row| row.unread().is_none())
                .map(|row| row.registry())
                .collect(),
        )?;
        Ok((present, readable))
    }

    /// Keep previously observed positive facts only for services still present in this run.
    /// This scheduling history supplies no current mapping or mutation authority.
    pub fn retain_present_from(&mut self, before: &Self, present: &Self) {
        self.0.retain(|id| present.0.binary_search(id).is_ok());
        self.0.extend(
            before
                .0
                .iter()
                .filter(|id| present.0.binary_search(id).is_ok())
                .copied(),
        );
        self.0.sort_unstable();
        self.0.dedup();
    }

    /// Whether a service gained a positive observation; errors and lost reads add nothing.
    #[must_use]
    pub fn gained_from(&self, before: &Self) -> bool {
        self.0.iter().any(|id| before.0.binary_search(id).is_err())
    }

    /// Take the normalized identities without another allocation.
    #[must_use]
    pub fn into_vec(self) -> Vec<u64> {
        self.0
    }
}

fn scope_at(root: &Path) -> Result<InputSetupScope, String> {
    let records = observation_at(root).map_err(|why| why.to_string())?.records;
    Ok(if let Some(revision) = accepted(&records) {
        InputSetupScope::Applied(revision)
    } else if let Some(revision) = pending_inputs::caps_revision(root, &records) {
        InputSetupScope::Pending(revision)
    } else {
        InputSetupScope::Inactive
    })
}

/// Read the accepted or pending Caps scope from one canonical observation. Pending scope
/// authorizes a hardware observation only, never a setter or another setup stage.
///
/// # Errors
/// This account's scope or setup records cannot be read.
pub fn input_setup_scope() -> Result<InputSetupScope, String> {
    ensure_person_scope()?;
    scope_at(&state_root()?)
}

/// The same input-scope reader over inert scratch records, with no live account scope.
///
/// # Errors
/// The scratch setup records cannot be read.
#[cfg(any(test, debug_assertions))]
pub fn input_setup_scope_at(root: &Path) -> Result<InputSetupScope, String> {
    scope_at(root)
}

fn accepted(records: &Records) -> Option<u64> {
    let (plan, _) = records.plan.as_ref()?;
    let held = holds(records);
    if !plan.selection.contains(Setting::CapsLock)
        || held.holds(Setting::CapsLock)
        || matches!(records.decision, Decision::Stopped(_))
        || records.restored.is_some_and(|restored| {
            restored.left.contains(Setting::CapsLock)
                || restored
                    .queue(records.progress, Some(plan))
                    .contains(Setting::CapsLock)
        })
    {
        return None;
    }
    let Some(progress) = records.progress else {
        // The older selection-only writer kept no progress. Its applied decision already
        // proves the workstation pass ran (the same evidence a later choice carries with
        // older_run). Keep that run's existing revision zero without admitting another pass.
        // With no physical run to disambiguate a Stop, any request withholds this authority.
        if records.stop_request.is_some() {
            return None;
        }
        let run = older_run(records.plan.as_ref(), records.decision)?;
        return (run.done & Stage::Workstation.bit() != 0).then_some(run.revision);
    };
    // A choice can carry Caps while its Workstation stage owes only other preferences.
    // `only` names what still has to apply; without that explicit scope, an unfinished
    // stage supplies no per-setting completion evidence.
    let caps_applied = progress.done & Stage::Workstation.bit() != 0
        || progress
            .only
            .is_some_and(|only| !only.contains(Setting::CapsLock));
    (progress.belongs_to(plan)
        && progress.admitted
        && !progress.stopped
        && !progress.named_by(records.stop_request)
        && caps_applied)
        .then_some(progress.revision)
}

pub(super) fn revision_at(root: &Path) -> Result<Option<u64>, String> {
    Ok(accepted(
        &observation_at(root).map_err(|why| why.to_string())?.records,
    ))
}

/// Activate new or changed services under their original accepted Caps setting. The
/// profile lock orders the operation against choices and Restore, while both the durable and
/// this process's immediate Stop are checked before each whole item.
/// `invalidate` retires an earlier whole-Caps reading before a mutation or on uncertain
/// attachment coverage; this path never supplies a positive whole-Caps observation.
///
/// # Errors
/// Setup authority, console identity, input facts or live activation cannot be read safely.
pub fn activate_input_attachments(
    expected: u64,
    registries: &[u64],
    stopped: impl Fn(u64) -> bool,
    invalidate: &dyn Fn(),
) -> Result<Option<KeyboardAttachmentActivation>, String> {
    let invalidated = std::cell::Cell::new(false);
    let invalidate_once = || {
        if !invalidated.replace(true) {
            invalidate();
        }
    };
    let result = (|| {
        ensure_person_scope()?;
        let root = state_root()?;
        activate_at(
            &root,
            expected,
            &stopped,
            &invalidate_once,
            |kept, continues| {
                aterm_machines::init::activate_keyboard_attachments(
                    &aterm_machines::port::Live::new(),
                    registries,
                    &|item| kept.keeps(item),
                    continues,
                    &invalidate_once,
                )
            },
        )
    })();
    if matches!(
        result,
        Err(_) | Ok(Some(KeyboardAttachmentActivation::Unfinished))
    ) {
        invalidate_once();
    }
    result
}

fn activate_at<T>(
    root: &Path,
    expected: u64,
    stopped: &dyn Fn(u64) -> bool,
    invalidate: &dyn Fn(),
    work: impl FnOnce(&kept::Kept, &dyn Fn() -> bool) -> Result<T, String>,
) -> Result<Option<T>, String> {
    with_current_at(root, expected, stopped, invalidate, |continues| {
        let kept = kept::Kept::load(root)?;
        work(&kept, continues)
    })
}

/// Publish an already-paid input diagnostic under the same profile/current-revision guard
/// as attachment activation. This reads only setup authority, never preferences or HID.
///
/// # Errors
/// This account's scope, setup lock or current records cannot be resolved.
pub fn with_current_input_scope<T>(
    expected: u64,
    stopped: impl Fn(u64) -> bool,
    work: impl FnOnce() -> T,
) -> Result<Option<T>, String> {
    ensure_person_scope()?;
    with_current_at(&state_root()?, expected, &stopped, &|| {}, |_| {
        // The authority read can block; honor an immediate Stop again after it.
        Ok((!stopped(expected)).then(work))
    })
    .map(Option::flatten)
}

fn with_current_at<T>(
    root: &Path,
    expected: u64,
    stopped: &dyn Fn(u64) -> bool,
    invalidate: &dyn Fn(),
    work: impl FnOnce(&dyn Fn() -> bool) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Ok(None);
    };
    let continues = || {
        let current =
            !stopped(expected) && revision_at(root).is_ok_and(|now| now == Some(expected));
        if !current {
            invalidate();
        }
        current
    };
    if !continues() {
        return Ok(None);
    }
    work(&continues).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_plan() -> Plan {
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        Plan::from(selected)
    }

    fn choose(root: &Path, plan: &Plan) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                plan,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result
        .unwrap()
        .expect("admitted choice")
    }

    #[test]
    fn an_unfinished_desktop_choice_keeps_the_caps_setting_it_carried() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut plan = caps_plan();
        let original = choose(root, &plan);
        record_applied(root, plan.selection).unwrap();
        assert_eq!(revision_at(root).unwrap(), Some(original));

        plan.selection.set(Setting::Desktop, true);
        plan.selection.set(Setting::Dock, true);
        let current = choose(root, &plan);
        assert_ne!(current, original);
        let before = observation_at(root).unwrap();
        let run = before.records.progress.unwrap();
        assert_eq!(run.done & Stage::Workstation.bit(), 0);
        let only = run
            .only
            .expect("the choice carries its finished preferences");
        assert!(only.contains(Setting::Desktop) && only.contains(Setting::Dock));
        assert!(!only.contains(Setting::CapsLock));

        assert_eq!(
            revision_at(root).unwrap(),
            Some(current),
            "an unrelated pending preference must not suspend accepted keyboard attachments"
        );
        assert_eq!(
            activate_at::<bool>(root, current, &|_| false, &|| {}, |_, continues| Ok(
                continues()
            ))
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            activate_at::<bool>(root, original, &|_| false, &|| {}, |_, _| panic!(
                "old revision"
            ))
            .unwrap(),
            None
        );
        assert_eq!(observation_at(root).unwrap(), before);
    }

    #[test]
    fn a_first_caps_pass_enrolls_attachments_while_an_unrelated_preference_stays_pending() {
        use crate::macos_doctor::NativeHost;
        use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL, Model, caps_to_control};
        use aterm_machines::model::{PValue, User};
        use aterm_machines::port::{Fake, Ran, Stat};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Caps {
            Attached,
            Partial,
            Applied,
            Failed,
            Unread,
            Unobserved,
        }
        for (caps, legacy, dock_pending) in [
            (Caps::Attached, false, true),
            (Caps::Partial, false, true),
            (Caps::Applied, false, true),
            (Caps::Failed, false, true),
            (Caps::Unread, false, true),
            (Caps::Unobserved, false, true),
            (Caps::Applied, true, true),
            (Caps::Applied, true, false),
        ] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let mut plan = caps_plan();
            plan.selection.set(Setting::Dock, dock_pending);
            let current = if legacy {
                crate::macos_doctor::authorize_automatic_setup_at(root, plan.selection).unwrap();
                0
            } else {
                choose(root, &plan)
            };
            let user = User {
                name: "alice".into(),
                uid: 501,
                gid: 20,
                home: "/Users//alice".into(),
                shell: "/bin/zsh".into(),
                admin: false,
            };
            let mut sys = Fake::root();
            sys.euid = user.uid;
            sys.keyboard_services = Some(Ok(match caps {
                Caps::Partial => vec![41, 42],
                Caps::Attached | Caps::Unobserved => vec![41],
                _ => Vec::new(),
            }));
            sys.logged_in(&user);
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: user.uid,
                    gid: user.gid,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            let census = if matches!(caps, Caps::Attached | Caps::Partial) {
                let mut census = format!(
                    "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n",
                    "VendorID",
                    "ProductID",
                    "RegistryID",
                    "Product",
                    "0x0",
                    "0x0",
                    "0x29",
                    "Fixture keyboard"
                );
                if caps == Caps::Partial {
                    census.push_str(&format!(
                        "{:<12}{:<12}{:<16}{:<24}0\n",
                        "0x0", "0x0", "0x2a", "Unread fixture keyboard"
                    ));
                }
                census.push_str("\nDevices:\n");
                census
            } else {
                "Services:\n\nDevices:\n".into()
            };
            sys.on(
                &aterm_machines::facts::hidutil_keyboards().display(),
                Ran::ok(&census),
            );
            sys.on(
                "/usr/bin/defaults export /Library/Preferences/com.apple.keyboardtype -",
                Ran::ok(&aterm_machines::plist::document(&PValue::Dict(Vec::new()))),
            );
            let mapped = Arc::new(AtomicBool::new(false));
            let reading = Arc::clone(&mapped);
            let hid_reads = Arc::new(AtomicUsize::new(0));
            let reads = Arc::clone(&hid_reads);
            let key = Model {
                vendor: 0,
                product: 0,
            }
            .key();
            sys.simulate(move |_, cmd| {
                if cmd.program.file_name().is_some_and(|name| name == "hidutil") {
                    reads.fetch_add(1, Ordering::SeqCst);
                    if !matches!(caps, Caps::Attached | Caps::Partial) {
                        return None;
                    }
                    if cmd.args.first().is_some_and(|word| word == "property") {
                        return Some(Ran::ok(&format!("RegistryID Key Value\n29 UserKeyMapping ( {{ HIDKeyboardModifierMappingSrc = {CAPS_LOCK}; HIDKeyboardModifierMappingDst = {LEFT_CONTROL}; }} )\n")));
                    }
                    if cmd.args.first().is_some_and(|word| word == "dump") {
                        let plugin = PValue::Dict(vec![
                            ("Class".into(), PValue::Str("IOHIDKeyboardFilter".into())),
                            ("HIDKeyboardModifierMappingPairs".into(), PValue::Array(Vec::new())),
                            ("UserKeyMapping".into(), PValue::Array(vec![PValue::Dict(vec![
                                ("Src".into(), PValue::Int(CAPS_LOCK)),
                                ("Dst".into(), PValue::Int(LEFT_CONTROL)),
                            ])])),
                            ("FnFunctionUsageMap".into(), PValue::Array(Vec::new())),
                            ("FnKeyMode".into(), PValue::Int(0)),
                            ("UnifiedKeyMapping".into(), PValue::Bool(false)),
                            ("LegacyKeyMaps".into(), PValue::Array(Vec::new())),
                            ("UnifiedKeyMaps".into(), PValue::Array(Vec::new())),
                            ("NumLockKeyboardUsageMap".into(), PValue::Array(Vec::new())),
                        ]);
                        let service = PValue::Dict(vec![
                            ("IORegistryEntryID".into(), PValue::Int(41)),
                            ("PrimaryUsagePage".into(), PValue::Int(1)),
                            ("PrimaryUsage".into(), PValue::Int(6)),
                            ("ServiceFilterDebug".into(), PValue::Array(vec![PValue::Dict(vec![("plugin".into(), plugin)])])),
                        ]);
                        return Some(Ran::ok(&aterm_machines::plist::document(&PValue::Dict(vec![("ServiceRecords".into(), PValue::Array(vec![service]))]))));
                    }
                    None
                } else if cmd.display() == "alice$ /usr/bin/defaults -currentHost export -g -" {
                    if caps == Caps::Unread {
                        return Some(Ran::exit(1, "inert unread preference domain"));
                    }
                    let entries = if reading.load(Ordering::SeqCst) {
                        vec![(key.clone(), PValue::Array(vec![caps_to_control()]))]
                    } else {
                        Vec::new()
                    };
                    Some(Ran::ok(&aterm_machines::plist::document(&PValue::Dict(
                        entries,
                    ))))
                } else if cmd.program == Path::new("/usr/bin/defaults")
                    && cmd.args.get(1).is_some_and(|arg| arg == "write")
                    && cmd.args.get(3).is_some_and(|arg| arg == key.as_str())
                {
                    assert_eq!(cmd.user.as_ref().unwrap().uid, 501);
                    if caps == Caps::Failed {
                        return Some(Ran::exit(1, "inert failed Caps write"));
                    }
                    reading.store(true, Ordering::SeqCst);
                    Some(Ran::ok(""))
                } else {
                    None
                }
            });
            let mut native = NativeHost::over(
                Box::new(sys),
                Box::new(aterm_machines::init::prepare_workstation_selected),
            );
            let report = if legacy {
                // The selection-only first-run entry has no modern progress. A partial cohort
                // cannot complete its older whole-pass decision.
                crate::macos_doctor::apply_on_first_run_with(
                    root,
                    &crate::macos_doctor::tests::Inert::default(),
                    &mut native,
                    "inert-legacy-preference-completion",
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
                )
                .unwrap()
                .unwrap()
            } else {
                let mut host = super::super::tests::Fake::default().with_workstation(native);
                execute_initial(&mut host, root, &mut None)
                    .unwrap()
                    .unwrap()
            };
            let applied = matches!(caps, Caps::Applied | Caps::Attached);
            assert_eq!(
                mapped.load(Ordering::SeqCst),
                applied || caps == Caps::Partial,
                "{caps:?}"
            );
            if caps == Caps::Attached {
                let paid_reads = hid_reads.load(Ordering::SeqCst);
                assert!(
                    paid_reads > 0,
                    "the ordinary native producer read the attached cohort"
                );
                let readback = &report
                    .page
                    .as_ref()
                    .expect("the pass keeps its page")
                    .readback;
                let observed = readback
                    .observations()
                    .iter()
                    .find(|row| row.subject == Subject::Setting(Setting::CapsLock));
                assert_eq!(
                    observed.map(|row| (row.standing, row.now.as_deref())),
                    Some((aterm_types::macos_setup::Standing::InPlace, Some("Control"))),
                    "the pass must retain its already-read effective native Caps facts"
                );
                let _ = readback.clone().take();
                assert_eq!(
                    hid_reads.load(Ordering::SeqCst),
                    paid_reads,
                    "retaining and taking readback starts no HID query"
                );
            }
            if caps == Caps::Partial {
                let paid_reads = hid_reads.load(Ordering::SeqCst);
                let observed = report.page.as_ref().and_then(|page| {
                    page.readback
                        .observations()
                        .iter()
                        .find(|row| row.subject == Subject::Setting(Setting::CapsLock))
                });
                assert!(
                    observed.is_none_or(|row| {
                        !matches!(
                            row.standing,
                            aterm_types::macos_setup::Standing::InPlace
                                | aterm_types::macos_setup::Standing::NotNeeded
                        ) && row.now.as_deref() != Some("Control")
                    }),
                    "a paid but partial native cohort must not claim physical Control"
                );
                assert_eq!(hid_reads.load(Ordering::SeqCst), paid_reads);
            }
            assert_eq!(
                report.complete(),
                !dock_pending,
                "the unread Dock still owes its pass"
            );
            assert_eq!(first_run_complete(root).unwrap(), !dock_pending);
            if !legacy {
                assert_eq!(
                    progress(root).unwrap().unwrap().done & Stage::Workstation.bit(),
                    0
                );
            }
            if applied && !legacy {
                let remaining = progress(root).unwrap().unwrap().only.unwrap();
                assert!(remaining.contains(Setting::Dock));
                assert!(!remaining.contains(Setting::CapsLock));
            }
            let enrolled = applied && (!legacy || !dock_pending);
            assert_eq!(
                revision_at(root).unwrap(),
                enrolled.then_some(current),
                "{caps:?}: Caps completion cannot be borrowed from the unfinished Dock"
            );
            assert_eq!(
                activate_at::<bool>(root, current, &|_| false, &|| {}, |_, continues| Ok(
                    continues()
                ))
                .unwrap(),
                enrolled.then_some(true)
            );
            if enrolled {
                let before = observation_at(root).unwrap();
                assert_eq!(
                    activate_at::<bool>(root, current + 1, &|_| false, &|| {}, |_, _| panic!(
                        "other revision"
                    ))
                    .unwrap(),
                    None
                );
                assert_eq!(
                    activate_at::<bool>(root, current, &|_| true, &|| {}, |_, _| panic!(
                        "immediate Stop"
                    ))
                    .unwrap(),
                    None
                );
                assert_eq!(observation_at(root).unwrap(), before);
                request_stop_at(root, current).unwrap();
                assert_eq!(revision_at(root).unwrap(), None);
            }
        }
    }

    #[test]
    fn unfinished_caps_work_is_not_carried_as_an_applied_setting() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut plan = caps_plan();
        plan.selection.set(Setting::Desktop, true);
        choose(root, &plan);
        assert_eq!(
            revision_at(root).unwrap(),
            None,
            "a fresh Caps pass is owed"
        );

        let mut desktop = Selection::none();
        desktop.set(Setting::Desktop, true);
        record_applied(root, desktop).unwrap();
        assert_eq!(revision_at(root).unwrap(), None, "partial stage completion");
        plan.selection.set(Setting::Dock, true);
        choose(root, &plan);
        assert!(
            progress(root)
                .unwrap()
                .unwrap()
                .only
                .unwrap()
                .contains(Setting::CapsLock)
        );
        let before = observation_at(root).unwrap();
        assert_eq!(
            revision_at(root).unwrap(),
            None,
            "a later choice cannot invent completion of the unfinished Caps setting"
        );
        assert_eq!(observation_at(root).unwrap(), before);

        let added = aterm_tempfile::tempdir().unwrap();
        let root = added.path();
        plan.selection.set(Setting::CapsLock, false);
        choose(root, &plan);
        record_applied(root, plan.selection).unwrap();
        plan.selection.set(Setting::CapsLock, true);
        let current = choose(root, &plan);
        assert!(
            progress(root)
                .unwrap()
                .unwrap()
                .only
                .unwrap()
                .contains(Setting::CapsLock)
        );
        assert_eq!(
            revision_at(root).unwrap(),
            None,
            "newly selected Caps is owed"
        );
        record_applied(root, caps_plan().selection).unwrap();
        assert_eq!(revision_at(root).unwrap(), Some(current));
    }

    #[test]
    fn a_choice_cannot_enroll_caps_that_still_stands_restored() {
        for desktop_pending in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let mut plan = caps_plan();
            choose(root, &plan);
            record_applied(root, plan.selection).unwrap();
            // Restore's durable first publication can outlive a cut-off final plan save.
            save_restore(root).unwrap();
            plan.selection.set(Setting::Desktop, desktop_pending);
            let current = choose(root, &plan);
            let before = observation_at(root).unwrap();
            let run = before.records.progress.unwrap();
            assert_eq!(
                run.done & Stage::Workstation.bit() != 0,
                !desktop_pending,
                "a choice can keep the stage finished while its selected Caps stands restored"
            );
            if desktop_pending {
                assert!(!run.only.unwrap().contains(Setting::CapsLock));
            }
            assert!(
                before
                    .records
                    .restored
                    .unwrap()
                    .left
                    .contains(Setting::CapsLock)
            );
            assert_eq!(revision_at(root).unwrap(), None);
            assert_eq!(
                activate_at::<bool>(root, current, &|_| false, &|| {}, |_, _| panic!(
                    "Caps is restored"
                ))
                .unwrap(),
                None
            );
            assert_eq!(observation_at(root).unwrap(), before);
        }
    }

    #[test]
    fn carried_caps_still_requires_its_current_unheld_revision() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut plan = caps_plan();
        choose(root, &plan);
        record_applied(root, plan.selection).unwrap();
        plan.selection.set(Setting::Desktop, true);
        let current = choose(root, &plan);
        assert_eq!(revision_at(root).unwrap(), Some(current));
        assert_eq!(
            activate_at::<bool>(root, current, &|_| true, &|| {}, |_, _| panic!(
                "immediate Stop"
            ))
            .unwrap(),
            None
        );
        request_stop_at(root, current).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);

        plan.selection.set(Setting::CapsLock, false);
        choose(root, &plan);
        assert_eq!(revision_at(root).unwrap(), None);
    }

    #[test]
    fn automatic_upgrade_enrolls_only_caps_that_were_applied_or_carried() {
        for applied in [Setting::Dock, Setting::CapsLock] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let root = dir.path();
            let mut partial = Selection::none();
            partial.set(applied, true);
            let explicit = save_explicit_workstation_plan(root, partial).unwrap();
            complete_explicit_workstation_plan(root, explicit).unwrap();
            execute_initial(&mut super::super::tests::Fake::default(), root, &mut None).unwrap();
            let original = progress(root).unwrap().unwrap();
            assert!(original.completed);
            assert_eq!(
                revision_at(root).unwrap(),
                (applied == Setting::CapsLock).then_some(original.revision)
            );

            let records = observation_at(root).unwrap().records;
            let full = scope::upgrade_of(&records, || true).expect("authenticated scope widens");
            let mut ticket = InitialSetupAuthorization::capture_at(root).unwrap();
            ticket.offer_reading(root, || Ok(None)).unwrap();
            let current = ticket.upgrade_at(root, &full).result.unwrap().unwrap();
            let before = observation_at(root).unwrap();
            let run = before.records.progress.unwrap();
            assert_eq!(run.done & Stage::Workstation.bit(), 0);
            assert_eq!(
                run.only.unwrap().contains(Setting::CapsLock),
                applied != Setting::CapsLock,
                "new Caps is owed; completed Caps is carried"
            );
            assert_eq!(
                revision_at(root).unwrap(),
                (applied == Setting::CapsLock).then_some(current),
                "attachment authority belongs to this setting, not the unfinished stage"
            );
            assert_eq!(
                activate_at::<bool>(root, current, &|_| false, &|| {}, |_, continues| {
                    assert_eq!(applied, Setting::CapsLock, "new Caps has not applied");
                    Ok(continues())
                })
                .unwrap(),
                (applied == Setting::CapsLock).then_some(true)
            );
            assert_eq!(
                activate_at::<bool>(root, original.revision, &|_| false, &|| {}, |_, _| panic!(
                    "the old partial revision grants nothing"
                ))
                .unwrap(),
                None
            );
            assert_eq!(observation_at(root).unwrap(), before);

            let report =
                execute_initial(&mut super::super::tests::Fake::default(), root, &mut None)
                    .unwrap()
                    .unwrap();
            assert!(report.complete());
            assert_eq!(revision_at(root).unwrap(), Some(current));
        }
    }

    fn ready(root: &Path) -> (Plan, Progress) {
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let plan = Plan::from(selected);
        save_plan(root, &plan).unwrap();
        save_workstation_authorization(root, &plan).unwrap();
        let mut progress = Progress::for_plan(&plan);
        progress.revision = 7;
        progress.admitted = true;
        progress.done = Stage::Workstation.bit();
        progress.completed = true;
        save_progress(root, progress).unwrap();
        (plan, progress)
    }

    fn older(root: &Path, selection: Selection, state: &str, verified: bool) {
        save_automatic_plan(root, selection).unwrap();
        write_json(
            &root.join("first-run.json"),
            &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
        )
        .unwrap();
    }

    #[test]
    fn completed_selection_only_setup_keeps_caps_authority_without_restarting_its_pass() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let selected = Selection::all().workstation();
        older(root, selected, "applied", true);
        bring_forward_restored_at(root).unwrap();
        let before = observation_at(root).unwrap();
        assert!(finished_at(root).unwrap());
        assert!(before.records.progress.is_none());
        assert!(scope::upgrade_of(&before.records, || true).is_none());
        assert_eq!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at::<bool>(root, 0, &|_| false, &|| {}, |_, go| {
                assert!(HeldLock::try_take(root)?.is_none());
                Ok(go())
            })
            .unwrap(),
            Some(true)
        );
        assert_eq!(observation_at(root).unwrap(), before);
        assert!(finished_at(root).unwrap());
        assert!(progress_file(root).unwrap().is_none());

        // A subsequent modern choice is staged under a different revision: its saved plan
        // alone never lets the old worker borrow the new setting's authority.
        let (plan, mut progress) = ready(root);
        assert_ne!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at::<bool>(root, 0, &|_| false, &|| {}, |_, _| panic!(
                "superseded legacy setting"
            ))
            .unwrap(),
            None
        );
        progress.admitted = false;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        std::fs::remove_file(root.join("initial-setup.json")).unwrap();
        save_workstation_decision(root, true).unwrap();
        assert!(saved_plan_record(root).unwrap().unwrap().1);
        assert!(plan.selection.contains(Setting::CapsLock));
        assert_eq!(revision_at(root).unwrap(), None);
    }

    #[test]
    fn selection_only_attachment_authority_requires_an_applied_caps_choice_without_holds() {
        for state in ["pending", "cancelled", "restored"] {
            let dir = aterm_tempfile::tempdir().unwrap();
            older(dir.path(), Selection::all().workstation(), state, true);
            assert_eq!(revision_at(dir.path()).unwrap(), None, "{state}");
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut selected = Selection::all().workstation();
        selected.set(Setting::CapsLock, false);
        older(root, selected, "applied", true);
        assert_eq!(revision_at(root).unwrap(), None);

        // Unverified sign-outs do not revoke a workstation pass that already applied.
        selected.set(Setting::CapsLock, true);
        older(root, selected, "applied", false);
        assert_eq!(revision_at(root).unwrap(), Some(0));
        assert_eq!(
            activate_at::<bool>(root, 0, &|_| true, &|| {}, |_, _| panic!(
                "immediate legacy Stop"
            ))
            .unwrap(),
            None
        );
        request_stop_at(root, 0).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        request_stop_at(root, 9).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        std::fs::write(root.join(STOP_REQUEST), b"unreadable").unwrap();
        assert!(revision_at(root).is_err() || revision_at(root).unwrap().is_none());
    }

    #[test]
    fn attachment_authority_survives_completion_but_not_stop_off_or_unadmitted_work() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let (mut plan, mut progress) = ready(root);
        assert_eq!(revision_at(root).unwrap(), Some(7));
        assert_eq!(
            activate_at::<bool>(root, 7, &|_| false, &|| {}, |_, go| Ok(go())).unwrap(),
            Some(true)
        );
        assert_eq!(
            activate_at::<bool>(root, 8, &|_| false, &|| {}, |_, _| panic!("stale revision"))
                .unwrap(),
            None
        );
        assert_eq!(
            activate_at::<bool>(root, 7, &|_| true, &|| {}, |_, _| panic!("immediate Stop"))
                .unwrap(),
            None
        );
        progress.admitted = false;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        progress.admitted = true;
        progress.done = 0;
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        progress.done = Stage::Workstation.bit();
        plan.selection.set(Setting::CapsLock, false);
        progress.bind(&plan);
        save_plan(root, &plan).unwrap();
        save_progress(root, progress).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        ready(root);
        request_stop_at(root, 7).unwrap();
        assert_eq!(revision_at(root).unwrap(), None);
        assert_eq!(
            activate_at::<bool>(root, 7, &|_| false, &|| {}, |_, _| panic!("durable Stop"))
                .unwrap(),
            None
        );
        let restored = aterm_tempfile::tempdir().unwrap();
        ready(restored.path());
        save_restore(restored.path()).unwrap();
        assert_eq!(revision_at(restored.path()).unwrap(), None);
        let unread = aterm_tempfile::tempdir().unwrap();
        ready(unread.path());
        std::fs::write(unread.path().join(STOP_REQUEST), b"unreadable").unwrap();
        assert!(
            revision_at(unread.path()).is_err() || revision_at(unread.path()).unwrap().is_none()
        );
    }

    #[test]
    fn the_effect_boundary_rechecks_a_stop_and_keeps_the_profile_lock() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        ready(root);
        let invalidated = std::cell::Cell::new(0);
        let invalidate = || invalidated.set(invalidated.get() + 1);
        let held = HeldLock::try_take(root).unwrap().unwrap();
        assert_eq!(
            activate_at::<bool>(root, 7, &|_| false, &invalidate, |_, _| panic!(
                "a busy profile does not admit a read"
            ))
            .unwrap(),
            None
        );
        assert_eq!(
            invalidated.get(),
            0,
            "busy admission preserves an owned reading"
        );
        drop(held);
        assert_eq!(
            with_current_at(root, 7, &|_| false, &invalidate, |current| {
                assert!(HeldLock::try_take(root)?.is_none());
                assert!(current());
                Ok(7)
            })
            .unwrap(),
            Some(7)
        );
        assert_eq!(
            activate_at::<bool>(root, 7, &|_| false, &invalidate, |_, go| {
                assert!(HeldLock::try_take(root)?.is_none());
                assert!(go());
                request_stop_at(root, 7)?;
                Ok(go())
            })
            .unwrap(),
            Some(false)
        );
        assert_eq!(invalidated.get(), 1, "an observed Stop retires the reading");
        assert_eq!(
            with_current_at::<()>(root, 7, &|_| false, &|| {}, |_| {
                panic!("a stopped revision cannot publish paid live evidence")
            })
            .unwrap(),
            None
        );
        assert!(HeldLock::try_take(root).unwrap().is_some());
    }
}
