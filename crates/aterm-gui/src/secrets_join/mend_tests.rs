// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::service::{self, Job};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct ScratchSubscriber {
    home: PathBuf,
    config: PathBuf,
    installs: AtomicUsize,
    fail_once: AtomicBool,
    running: AtomicBool,
}

impl ScratchSubscriber {
    /// A member owed a subscriber, as a join whose install failed leaves it: the only absent
    /// subscriber the mend installs.
    fn new(home: &Path, fail_once: bool) -> Arc<Self> {
        let this = Arc::new(Self {
            home: home.to_owned(),
            config: home.join("aterm.toml"),
            installs: AtomicUsize::new(0),
            fail_once: AtomicBool::new(fail_once),
            running: AtomicBool::new(false),
        });
        note_owed(Some(&this.config), Some("MAC-A"), true);
        this
    }

    fn install_effect(&self) -> aterm_secrets::Result<String> {
        let independent = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.home.join(".config/aterm/secrets/subscriber.lock"))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "an independent scratch handle verifies the injected service effect is serialized with Off"
        )]
        let taken = independent.try_lock();
        assert!(matches!(taken, Err(std::fs::TryLockError::WouldBlock)));
        self.installs.fetch_add(1, Ordering::SeqCst);
        let plist = service::plist_path(&self.home, Job::Subscriber);
        std::fs::create_dir_all(plist.parent().unwrap())?;
        std::fs::write(
            &plist,
            service::plist_text(
                Job::Subscriber.label(),
                Path::new("/bin/sh"),
                &["secrets".into(), "watch".into()],
                &service::log_path(&self.home, Job::Subscriber),
            ),
        )?;
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(aterm_secrets::Error::new("injected bootstrap failure"));
        }
        self.running.store(true, Ordering::SeqCst);
        Ok("scratch subscriber installed".into())
    }

    fn off(&self) {
        service::turn_off_subscriber_for_test(&self.home, || {
            assert!(service::subscriber_turned_off(&self.home));
            self.running.store(false, Ordering::SeqCst);
            match std::fs::remove_file(service::plist_path(&self.home, Job::Subscriber)) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        })
        .unwrap();
    }

    fn explicit_on(&self) {
        service::install_subscriber_for_test(&self.home, || self.install_effect()).unwrap();
    }

    fn setup(&self) -> Setup {
        let plist = subscriber_plist(&service::plist_path(&self.home, Job::Subscriber));
        Setup {
            member: true,
            pending: false,
            plist: match plist {
                Plist::Sound if !self.running.load(Ordering::SeqCst) => Plist::Down,
                plist => plist,
            },
            owed: owed(Some(&self.config), Some("MAC-A")),
            turned_off: service::subscriber_turned_off(&self.home),
        }
    }

    fn reach(self: &Arc<Self>, off_after_probe: bool) -> MendReach {
        MendReach {
            wanted: Box::new(|| true),
            while_wanted: Box::new(|work| {
                work();
                true
            }),
            configured: Box::new({
                let this = Arc::clone(self);
                move || {
                    let setup = this.setup();
                    if off_after_probe {
                        this.off();
                    }
                    Ok(Configured::Some(setup))
                }
            }),
            install: Box::new({
                let this = Arc::clone(self);
                move || {
                    install_subscriber_with(this.as_ref(), Some(&this.config), || {
                        Some("MAC-A".into())
                    })
                }
            }),
            paid: Box::new({
                let this = Arc::clone(self);
                move || note_owed(Some(&this.config), Some("MAC-A"), false)
            }),
        }
    }
}

impl SubscriberInstaller for ScratchSubscriber {
    fn repair(&self) -> Result<Option<String>, Unsynced> {
        service::repair_subscriber_for_test(&self.home, || self.install_effect())
            .map_err(|error| Unsynced::Failed(error.to_string()))
    }
}

#[test]
fn subscriber_mend_keeps_off_after_a_failed_install_left_it_owed() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let service = ScratchSubscriber::new(scratch.path(), true);
    let reach = service.reach(false);
    let failed = mend(&reach).unwrap();
    assert!(matches!(failed.subscriber, Subscriber::Missing(_)));
    assert!(mends_again(None, Some(&failed)));
    assert!(service.setup().owed);
    assert_eq!(service.installs.load(Ordering::SeqCst), 1);

    service.off();
    let stopped = service.setup();
    assert_eq!(stopped.plist, Plist::Absent);
    assert!(stopped.owed && stopped.turned_off);
    let settled = mend(&reach).unwrap();
    assert_eq!(service.installs.load(Ordering::SeqCst), 1);
    assert_eq!(settled.subscriber, Subscriber::Off);
    assert!(service::subscriber_turned_off(&service.home));
    assert!(!service.running.load(Ordering::SeqCst));
    assert!(!service.setup().owed, "Off retires the old obligation");

    // A failed Off can leave a stale plist behind its durable choice.
    std::fs::write(
        service::plist_path(&service.home, Job::Subscriber),
        service::plist_text(
            Job::Subscriber.label(),
            Path::new("/no-such-aterm/aterm"),
            &[],
            &service::log_path(&service.home, Job::Subscriber),
        ),
    )
    .unwrap();
    assert!(matches!(service.setup().plist, Plist::Stale(_)));
    assert_eq!(mend(&reach).unwrap().subscriber, Subscriber::Off);
    assert_eq!(service.installs.load(Ordering::SeqCst), 1);
    assert!(service::subscriber_turned_off(&service.home));
}

#[test]
fn subscriber_mend_keeps_off_published_after_its_probe() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let service = ScratchSubscriber::new(scratch.path(), false);
    let settled = mend(&service.reach(true)).unwrap();
    assert_eq!(service.installs.load(Ordering::SeqCst), 0);
    assert_eq!(settled.subscriber, Subscriber::Off);
    assert!(service::subscriber_turned_off(&service.home));
    assert!(!service.running.load(Ordering::SeqCst));
}

#[test]
fn subscriber_mend_retries_failures_and_explicit_on_can_replace_off() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let service = ScratchSubscriber::new(scratch.path(), true);
    let reach = service.reach(false);
    assert!(matches!(
        mend(&reach).unwrap().subscriber,
        Subscriber::Missing(_)
    ));
    assert_eq!(service.setup().plist, Plist::Down);
    assert!(service.setup().owed);
    assert_eq!(mend(&reach).unwrap().subscriber, Subscriber::Installed);
    assert_eq!(service.installs.load(Ordering::SeqCst), 2);
    assert!(!service.setup().owed);

    service.off();
    assert!(service.repair().unwrap().is_none());
    assert_eq!(service.installs.load(Ordering::SeqCst), 2);
    service.explicit_on();
    assert!(!service::subscriber_turned_off(&service.home));
    assert!(service.running.load(Ordering::SeqCst));
    assert_eq!(mend(&reach).unwrap().subscriber, Subscriber::Installed);
    assert_eq!(service.installs.load(Ordering::SeqCst), 3);
}
