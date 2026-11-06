// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Activate accepted keyboard defaults when this login gets a new HID service.

use aterm_secrets::converge::{Last, RunnerRetry};
use std::ffi::OsString;
use std::process::ExitCode;

#[derive(Default)]
struct Keeper {
    settled: Option<(u64, Vec<u64>)>,
    attempted: Option<(u64, Vec<u64>)>,
    retry: RunnerRetry,
}

impl Keeper {
    fn round_at_console(
        &mut self,
        sys: &dyn aterm_machines::port::System,
        now_ms: u64,
        revision: impl FnOnce() -> Result<Option<u64>, String>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        if sys.euid() == 0 || aterm_machines::console::remote(sys) {
            *self = Self::default();
            return Ok(false);
        }
        self.round(now_ms, revision()?, services, activate)
    }

    fn round(
        &mut self,
        now_ms: u64,
        revision: Option<u64>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        let Some(revision) = revision else {
            *self = Self::default();
            return Ok(false);
        };
        let mut services = services()?;
        services.sort_unstable();
        if services.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate keyboard service identity".into());
        }
        let current = (revision, services);
        if self.settled.as_ref() == Some(&current) {
            return Ok(false);
        }
        let new: Vec<_> = current
            .1
            .iter()
            .copied()
            .filter(|id| {
                self.settled.as_ref().is_none_or(|(before, ids)| {
                    *before != revision || ids.binary_search(id).is_err()
                })
            })
            .collect();
        if new.is_empty() {
            self.settled = Some(current);
            return Ok(false);
        }
        if self.attempted.as_ref() != Some(&current) {
            self.retry = RunnerRetry::default();
            self.attempted = Some(current.clone());
        } else if self
            .retry
            .remaining(now_ms)
            .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(false);
        }
        self.retry.started();
        let result = activate(revision, &new);
        self.retry.reaped(
            match &result {
                Ok(Some(true)) => Last::Ended,
                Ok(None) => Last::Held,
                Ok(Some(false)) => Last::Unfinished,
                Err(_) => Last::Failed,
            },
            now_ms,
        );
        if matches!(result, Ok(Some(true))) {
            self.settled = Some(current);
            return Ok(true);
        }
        result.map(|_| false)
    }
}

/// The subscriber starts the same keeper before entering its long-lived runtime. No
/// diagnostic, malformed command or help request starts a keyboard observer.
pub(super) fn secrets_entry(args: Vec<OsString>) -> ExitCode {
    secrets_entry_with(args, spawn_background, aterm_secrets::cli::main_entry)
}

fn secrets_entry_with<R>(
    args: Vec<OsString>,
    start: impl FnOnce(),
    run: impl FnOnce(Vec<OsString>) -> R,
) -> R {
    if args.len() == 1 && args[0] == "watch" {
        start();
    }
    run(args)
}

/// Start only inside the node job's owned startup, after its boot and exclusive lease.
pub(super) fn node_serve(startup: impl FnOnce()) -> ExitCode {
    node_serve_with(aterm_gui::run_node_serve_owning, spawn_background, startup)
}

fn node_serve_with<'a, R>(
    own: impl FnOnce(Box<dyn FnOnce() + 'a>) -> R,
    start: impl FnOnce() + 'a,
    runtime: impl FnOnce() + 'a,
) -> R {
    own(Box::new(move || {
        start();
        runtime();
    }))
}

fn spawn_background() {
    start_background_with(&aterm_machines::port::Live::new(), || spawn_with(|| {}));
}

fn start_background_with(sys: &dyn aterm_machines::port::System, start: impl FnOnce()) {
    // This process's Aqua session survives Fast User Switching. Console ownership is a
    // round's changing fact, not a permanent reason to omit its keeper at process birth.
    if sys.euid() != 0 && !sys.remote_login() && sys.graphic_session() == Some(true) {
        start();
    }
}

pub(super) fn spawn() {
    spawn_with(|| {
        super::request_observation(
            None,
            vec![aterm_types::macos_setup::Subject::Setting(
                aterm_types::macos_setup::Setting::CapsLock,
            )],
            false,
        );
    });
}

fn spawn_with(changed: fn()) {
    if aterm_types::dirs::runs_under_private_state_root() || std::env::var_os("CI").is_some() {
        return;
    }
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| spawn_worker(changed));
}

fn spawn_worker(changed: fn()) {
    let worker = move || {
        use aterm_machines::port::System as _;
        let _ = aterm_log::wait_for_logger(super::EARLY_LOG_WAIT);
        let clock = std::time::Instant::now();
        let sys = aterm_machines::port::Live::new();
        let mut keeper = Keeper::default();
        let mut said = None;
        loop {
            let result = keeper.round_at_console(
                &sys,
                u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
                aterm_cli::macos_doctor::input_activation_revision,
                || sys.keyboard_service_ids(),
                |revision, ids| {
                    aterm_cli::macos_doctor::activate_input_attachments(
                        revision,
                        ids,
                        super::setup_stop_requested,
                    )
                },
            );
            match result {
                Ok(true) => {
                    changed();
                    said = None;
                }
                Ok(false) => {}
                Err(why) if said.as_ref() != Some(&why) => {
                    aterm_log::info!("keyboard attachment activation waits: {why}");
                    said = Some(why);
                }
                Err(_) => {}
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    };
    if let Err(why) = std::thread::Builder::new()
        .name("aterm-input-attachments".into())
        .spawn(worker)
    {
        super::warn_when_logging(format!(
            "keyboard attachment activation could not start: {why}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriber_start_keeps_inputs_before_watch_but_other_commands_only_dispatch() {
        use std::cell::RefCell;

        for (words, watching) in [
            (&["watch"][..], true),
            (&["watch", "--help"], false),
            (&["watch", "extra"], false),
            (&["watch", "--invalid"], false),
            (&["help", "watch"], false),
            (&["sync"], false),
            (&["status"], false),
            (&[], false),
        ] {
            let events = RefCell::new(Vec::new());
            let args: Vec<_> = words.iter().map(OsString::from).collect();
            let expected = args.clone();
            assert_eq!(
                secrets_entry_with(
                    args,
                    || events.borrow_mut().push("keeper"),
                    |forwarded| {
                        assert_eq!(forwarded, expected);
                        assert_eq!(
                            events.borrow().as_slice(),
                            if watching { &["keeper"][..] } else { &[] }
                        );
                        events.borrow_mut().push("runtime");
                        ExitCode::from(75)
                    },
                ),
                ExitCode::from(75)
            );
            assert_eq!(
                events.into_inner(),
                if watching {
                    vec!["keeper", "runtime"]
                } else {
                    vec!["runtime"]
                }
            );
        }
    }

    #[test]
    fn node_start_keeps_inputs_only_inside_its_owned_startup() {
        use std::cell::RefCell;

        for owns in [false, true] {
            let events = RefCell::new(Vec::new());
            assert_eq!(
                node_serve_with(
                    |startup| {
                        events.borrow_mut().push("lease");
                        if owns {
                            startup();
                        }
                        ExitCode::from(75)
                    },
                    || events.borrow_mut().push("keeper"),
                    || {
                        assert_eq!(*events.borrow(), ["lease", "keeper"]);
                        events.borrow_mut().push("runtime");
                    },
                ),
                ExitCode::from(75)
            );
            assert_eq!(
                events.into_inner(),
                if owns {
                    vec!["lease", "keeper", "runtime"]
                } else {
                    vec!["lease"]
                }
            );
        }
    }

    #[test]
    fn background_keeper_requires_a_local_aqua_birth_but_no_terminal_or_vault() {
        use aterm_machines::port::{Fake, Stat};
        use std::cell::Cell;

        for (actor, console, graphic, remote, starts) in [
            (501, 501, Some(true), false, true),
            (0, 0, Some(true), false, false),
            (501, 502, Some(true), false, true),
            (501, 0, Some(true), false, true),
            (501, 501, Some(false), false, false),
            (501, 501, None, false, false),
            (501, 501, Some(true), true, false),
        ] {
            let mut sys = Fake::root();
            sys.euid = actor;
            sys.terminal = false;
            sys.graphic_session = graphic;
            sys.remote_login = remote;
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: console,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            let started = Cell::new(false);
            start_background_with(&sys, || started.set(true));
            assert_eq!(started.get(), starts);
            assert!(
                sys.calls().is_empty(),
                "startup needs no vault or hardware command"
            );
        }
    }

    #[test]
    fn an_aqua_keeper_parks_without_hardware_reads_and_resumes_after_user_switching() {
        use aterm_machines::port::{Fake, Stat};
        use std::cell::{Cell, RefCell};

        let mut sys = Fake::root();
        sys.euid = 501;
        sys.terminal = false;
        let console = |uid| {
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
        };
        // launchd can start this user's Aqua job while another login owns the console.
        console(502);
        let started = Cell::new(false);
        start_background_with(&sys, || started.set(true));
        assert!(started.get());

        let mut keeper = Keeper::default();
        let reads = Cell::new((0, 0));
        let activated = RefCell::new(Vec::new());
        for (owner, ids, active) in [
            (502, vec![10], false),
            (501, vec![11], true),
            (502, vec![12], false),
            (501, vec![12], true),
        ] {
            console(owner);
            let before = reads.get();
            let applied = activated.borrow().len();
            assert_eq!(
                keeper
                    .round_at_console(
                        &sys,
                        0,
                        || {
                            reads.set((reads.get().0 + 1, reads.get().1));
                            Ok(Some(7))
                        },
                        || {
                            reads.set((reads.get().0, reads.get().1 + 1));
                            Ok(ids.clone())
                        },
                        |revision, new| {
                            assert_eq!(revision, 7);
                            activated.borrow_mut().push(new.to_vec());
                            Ok(Some(true))
                        },
                    )
                    .unwrap(),
                active
            );
            if !active {
                assert_eq!(
                    reads.get(),
                    before,
                    "no setup or hardware reading while away"
                );
                assert_eq!(activated.borrow().len(), applied);
            }
        }
        assert_eq!(reads.get(), (2, 2));
        assert_eq!(*activated.borrow(), [vec![11], vec![12]]);
        assert!(sys.calls().is_empty());

        // Returning to the console cannot borrow a held or withdrawn Caps choice.
        assert!(
            !keeper
                .round_at_console(
                    &sys,
                    0,
                    || Ok(None),
                    || panic!("held Caps choice reads no hardware"),
                    |_, _| panic!("held Caps choice activates nothing"),
                )
                .unwrap()
        );
    }

    #[test]
    fn attach_and_reattach_activate_only_new_services_and_off_reads_no_hardware() {
        let mut keeper = Keeper::default();
        for (ids, expected) in [
            (vec![1], vec![1]),
            (vec![1, 2], vec![2]),
            (vec![1, 3], vec![3]),
        ] {
            assert!(
                keeper
                    .round(
                        0,
                        Some(7),
                        || Ok(ids),
                        |revision, new| {
                            assert_eq!(revision, 7);
                            assert_eq!(new, expected);
                            Ok(Some(true))
                        }
                    )
                    .unwrap()
            );
        }
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("removal only"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("unchanged"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    0,
                    None,
                    || panic!("Off reads no HID state"),
                    |_, _| panic!("Off")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    0,
                    Some(8),
                    || Ok(vec![1]),
                    |revision, ids| {
                        assert_eq!((revision, ids), (8, &[1][..]));
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
    }

    #[test]
    fn failed_or_held_attachments_retry_without_a_hot_loop_and_new_devices_wake_now() {
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| Ok(None))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("lock retry paced")
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(5000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(false)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    9999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("unfinished retry paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(10000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(true)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("success stops")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1, 2]),
                    |_, ids| {
                        assert_eq!(ids, [2]);
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10002,
                    Some(7),
                    || Ok(vec![1, 2, 2]),
                    |_, _| panic!("duplicate identity")
                )
                .is_err()
        );
    }

    #[test]
    fn an_unseen_model_retries_the_same_native_service_and_applies_when_its_census_recovers() {
        use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
        use aterm_machines::port::{Fake, Ran, Stat};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const REGISTRY: u64 = 41;
        fn dump(native: &[(i64, i64)]) -> String {
            let pairs = native
                .iter()
                .map(|(source, destination)| {
                    format!("<dict><key>Src</key><integer>{source}</integer><key>Dst</key><integer>{destination}</integer></dict>")
                })
                .collect::<String>();
            format!(
                "<plist version=\"1.0\"><dict><key>ServiceRecords</key><array><dict>\
                 <key>IORegistryEntryID</key><integer>{REGISTRY}</integer>\
                 <key>PrimaryUsagePage</key><integer>1</integer>\
                 <key>PrimaryUsage</key><integer>6</integer>\
                 <key>ServiceFilterDebug</key><array><dict><key>plugin</key><dict>\
                 <key>Class</key><string>IOHIDKeyboardFilter</string>\
                 <key>HIDKeyboardModifierMappingPairs</key><array>{pairs}</array>\
                 <key>UserKeyMapping</key><array/>\
                 <key>FnFunctionUsageMap</key><array/>\
                 <key>FnKeyboardUsageMap</key><array/>\
                 <key>NumLockKeyboardUsageMap</key><array/>\
                 </dict></dict></array></dict></array></dict></plist>"
            )
        }

        let who = aterm_machines::model::User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        sys.logged_in(&who);
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let census = format!(
            "Services:\n{:<12}{:<12}{:<24}Built-In\n{:<12}{:<12}{:<24}0\n{:<12}{:<12}{:<24}0\n\nDevices:\n",
            "VendorID",
            "ProductID",
            "Product",
            "0x5ac",
            "0x378",
            "Other keyboard",
            "0xfffe",
            "0xa9",
            "Fixture keyboard"
        );
        let available = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&available);
        sys.simulate(move |sys, cmd| {
            let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
            match cmd.program.file_name().and_then(|name| name.to_str()) {
                Some("hidutil") if args.first() == Some(&"list") => {
                    Some(Ran::ok(if observed.load(Ordering::SeqCst) {
                        &census
                    } else {
                        "Services:\n\nDevices:\n"
                    }))
                }
                Some("hidutil") if args.first() == Some(&"property") => {
                    let matching: aterm_json::Value = aterm_json::from_str(args[2]).unwrap();
                    let external = matching
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["VendorID"].as_i64() == Some(65534));
                    Some(Ran::ok(&format!(
                        "RegistryID Key Value\n{:x} UserKeyMapping (null)\n",
                        if external { REGISTRY } else { 10 }
                    )))
                }
                Some("hidutil") if args.first() == Some(&"dump") => {
                    let native = sys
                        .keyboard_changes()
                        .into_iter()
                        .find(|change| change.registry == REGISTRY)
                        .and_then(|change| change.native)
                        .unwrap_or_default();
                    Some(Ran::ok(&dump(&native)))
                }
                Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                    &aterm_machines::model::PValue::Dict(Vec::new()),
                ))),
                _ => None,
            }
        });
        let activate = |revision, ids: &[u64]| {
            assert_eq!(revision, 7);
            aterm_machines::init::activate_keyboard_attachments(&sys, ids, &|_| false, &|| true)
                .map(Some)
        };
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![REGISTRY]), activate)
                .unwrap()
        );
        assert!(sys.keyboard_changes().is_empty());
        available.store(true, Ordering::SeqCst);
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || Ok(vec![REGISTRY]),
                    |_, _| panic!("unfinished retry remains paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(5000, Some(7), || Ok(vec![REGISTRY]), activate)
                .unwrap()
        );
        let changed = sys.keyboard_changes();
        assert_eq!(
            changed.len(),
            1,
            "the unrelated model's NotApplicable row does not block coverage"
        );
        assert_eq!(changed[0].registry, REGISTRY);
        assert_eq!(changed[0].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
        assert!(sys.calls().iter().all(|call| {
            !call.contains("defaults") || (!call.contains(" write ") && !call.contains(" delete "))
        }));
        assert!(
            !keeper
                .round(
                    5001,
                    Some(7),
                    || Ok(vec![REGISTRY]),
                    |_, _| panic!("verified cohort settles")
                )
                .unwrap()
        );
    }
}
