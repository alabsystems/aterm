// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scratch-only deferred-preference effects for the binary's local Stop tests.

use super::*;
use aterm_machines::items::Ctx;
use aterm_machines::model::{HostFacts, User};
use aterm_machines::port::{Fake, Ran};
use std::sync::{Arc, Mutex};

const NC: &str = "com.apple.notificationcenterui";
const WIDGETS: &str = "consumer.widgets.notification-center@alice";
const BUILD: &str = "1.0.0+gfollow-up-stop";

struct NotificationCenter {
    value: Option<PValue>,
    writes: usize,
}

/// Real setup admission, withheld records and the real follow-up worker over
/// the selected Notification Center item and an entirely inert system port.
pub struct InitialSetupWithheldStopTest<'a> {
    root: &'a Path,
    notification_center: Arc<Mutex<NotificationCenter>>,
}

impl<'a> InitialSetupWithheldStopTest<'a> {
    /// A whole-profile init interrupted after recording its withheld item,
    /// before its verification or any effect: admitted, with no completed stage.
    pub fn unfinished(root: &'a Path) -> Result<Self, String> {
        let held = init_hold_at(root, true, &|_| {})?;
        aterm_machines::init::Held::withheld(&held, &[WIDGETS.to_owned()])?;
        drop(held);
        Ok(Self {
            root,
            notification_center: Arc::new(Mutex::new(NotificationCenter {
                value: Some(PValue::Dict(vec![
                    ("vers".into(), PValue::Int(1)),
                    (
                        "instances".into(),
                        PValue::Array(vec![PValue::Data(vec![42])]),
                    ),
                ])),
                writes: 0,
            })),
        })
    }

    /// Clear the real item once, then Restore without FDA so the original
    /// remains backed up and its exact put-back waits for the grant.
    pub fn waiting_restore(root: &'a Path) -> Result<Self, String> {
        let mut setup = Self::unfinished(root)?;
        let (observed, _reserved) = setup.follow_up(|| (), |_| false);
        observed.result?;
        let _lock = HeldLock::take(root)?;
        run_mutation(&mut setup.host(false), root, Action::Restore)?;
        Ok(setup)
    }

    pub fn revision(&self) -> Result<u64, String> {
        initial::revision_at(self.root)
    }

    /// A later real manual group choice, preserving the current whole-profile
    /// selection while admitting a run not covered by its predecessor's Stop.
    pub fn choose(&mut self) -> Result<u64, String> {
        InitialSetupAdmissionStopTest::restart(self.root)?
            .choose_group(&[Setting::Widgets])?
            .ok_or_else(|| "the later widget choice was refused".into())
    }

    pub fn follow_up<S>(
        &mut self,
        reserve: impl FnMut() -> S,
        stopped: impl Fn(u64) -> bool,
    ) -> (InitialSetupObserved<Option<Report>>, Option<S>) {
        follow_up_finished_waiting_observed_unless_stopped(
            self.root,
            reserve,
            || Ok(self.host(true)),
            BUILD,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
            || false,
            stopped,
        )
    }

    pub fn notification_center_writes(&self) -> usize {
        self.notification_center.lock().unwrap().writes
    }

    pub fn owed(&self) -> Result<bool, String> {
        Ok(!load_withheld(self.root)?.items.is_empty() || !restore_waiting(self.root)?.is_empty())
    }

    fn host(&self, granted: bool) -> NativeHost {
        let state = Arc::clone(&self.notification_center);
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.full_disk_access = Some(granted);
        sys.simulate(move |_, command| {
            let args: Vec<_> = command
                .args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect();
            if command
                .program
                .file_name()
                .is_some_and(|name| name == "pgrep")
            {
                // No real desktop process is measured or signalled.
                return Some(Ran::exit(1, ""));
            }
            if !command
                .program
                .file_name()
                .is_some_and(|name| name == "defaults")
            {
                return None;
            }
            match args.as_slice() {
                [verb, domain, dash] if verb == "export" && domain == NC && dash == "-" => {
                    let value = state.lock().unwrap().value.clone();
                    let domain = PValue::Dict(
                        value
                            .into_iter()
                            .map(|value| ("widgets".into(), value))
                            .collect(),
                    );
                    Some(Ran::ok(&plist::document(&domain)))
                }
                [verb, domain, key, value]
                    if verb == "write" && domain == NC && key == "widgets" =>
                {
                    let mut state = state.lock().unwrap();
                    state.value = Some(
                        plist::parse(value.as_bytes())
                            .expect("the real widget item writes a plist"),
                    );
                    state.writes += 1;
                    Some(Ran::ok(""))
                }
                [verb, domain, key] if verb == "delete" && domain == NC && key == "widgets" => {
                    let mut state = state.lock().unwrap();
                    state.value = None;
                    state.writes += 1;
                    Some(Ran::ok(""))
                }
                _ => None,
            }
        });
        NativeHost::over(
            Box::new(sys),
            Box::new(move |_, group, selected| {
                let user = User {
                    name: "alice".into(),
                    uid: 501,
                    gid: 20,
                    home: "/Users//alice".into(),
                    shell: "/bin/zsh".into(),
                    admin: false,
                };
                let ctx = Ctx {
                    facts: HostFacts {
                        full_disk_access: Some(granted),
                        ..HostFacts::default()
                    },
                    users: vec![user.clone()],
                    console: Some(user),
                    host_name: String::new(),
                    cache_dir: PathBuf::new(),
                    disrupt: true,
                };
                let items = if group == WorkstationGroup::Withheld {
                    aterm_machines::init_preferences::withheld_items(&ctx, selected)?
                        .into_iter()
                        .map(|(_, item)| item)
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(Workstation::of(items))
            }),
        )
    }
}
