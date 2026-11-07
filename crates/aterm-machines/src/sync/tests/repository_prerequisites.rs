// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual unattended toolset runner supplied through the sync factory.

use super::*;
use crate::init_toolset::{Atpkg, Found, Toolset};
use crate::items::{Ctx, Item as MachineItem};

#[derive(Default)]
pub(super) struct Packages {
    pub(super) git: AtomicBool,
    online: AtomicBool,
    fail_other: AtomicBool,
    calls: Mutex<Vec<String>>,
}

struct Prerequisite {
    toolset: Toolset,
    sys: crate::port::Fake,
    ctx: Ctx,
}

pub(super) fn item(packages: &Arc<Packages>, released: &Arc<AtomicU32>) -> Box<dyn Item> {
    let found = Arc::clone(packages);
    let attempt = Arc::clone(packages);
    let released = Arc::clone(released);
    Box::new(Prerequisite {
        toolset: Toolset::new(
            Box::new(move || {
                if found.git.load(Ordering::SeqCst) {
                    Found::Complete("Git is installed".into())
                } else {
                    Found::Missing("Git is missing".into())
                }
            }),
            Box::new(move |args, _, _| {
                assert!(released.load(Ordering::SeqCst) > 0, "store lock released");
                attempt.calls.lock().unwrap().push(args.join(" "));
                if !attempt.online.load(Ordering::SeqCst) {
                    return Ok(crate::port::Ran::exit(69, "offline"));
                }
                if args.first().is_some_and(|verb| verb == "update") {
                    attempt.git.store(true, Ordering::SeqCst);
                    if attempt.fail_other.load(Ordering::SeqCst) {
                        return Ok(crate::port::Ran::exit(1, "an unrelated package failed"));
                    }
                }
                Ok(crate::port::Ran::ok(""))
            }),
            None,
            Atpkg {
                update_flags: &["--defer-busy-flip"],
                left_for_next_pass: |_| Vec::new(),
            },
        ),
        sys: crate::port::Fake::default(),
        ctx: crate::items::defaults::tests::ctx(&[]),
    })
}

#[test]
fn usable_git_allows_cloning_even_when_another_package_failed() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let packages = Arc::new(Packages::default());
    packages.online.store(true, Ordering::SeqCst);
    packages.fail_other.store(true, Ordering::SeqCst);
    let mut ports = Inert::new(scratch.path());
    ports.console = false;
    ports.prerequisites = Some(Arc::clone(&packages));
    ports.clone_is(crate::init::OwedClone::Due(
        "private inventory is ready".into(),
    ));
    let pass = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(
        row(&pass.rows, "sync.repository-prerequisites").outcome,
        Outcome::Failed
    );
    assert_eq!(row(&pass.rows, CLONE).outcome, Outcome::Changed);
    assert!(
        !pass.complete(),
        "the unrelated package failure is still reported"
    );
}

impl Item for Prerequisite {
    fn id(&self) -> String {
        "sync.repository-prerequisites".into()
    }
    fn title(&self) -> String {
        "repository tools".into()
    }
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn layer(&self) -> Layer {
        Layer::Workstation
    }
    fn class(&self) -> Class {
        Class::Install
    }
    fn probe(&self) -> Probe {
        self.toolset.probe(&self.sys, &self.ctx)
    }
    fn apply(&self) -> Result<(), String> {
        self.toolset.apply(&self.sys, &self.ctx)
    }
    fn explain(&self, _: &Probe) -> Explain {
        Explain {
            why: "repository prerequisite".into(),
            remedy: Remedy::None,
        }
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.toolset.stood_aside()
    }
}

#[test]
fn a_headless_join_retries_its_missing_git_before_automatic_cloning() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let packages = Arc::new(Packages::default());
    let mut ports = Inert::new(scratch.path());
    ports.console = false;
    ports.prerequisites = Some(Arc::clone(&packages));
    ports.clone_is(crate::init::OwedClone::Due(
        "private inventory is ready".into(),
    ));

    let offline = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert!(!offline.complete());
    assert_eq!(
        packages.calls.lock().unwrap().len(),
        1,
        "one owned offline attempt"
    );
    assert!(!ports.applies().contains(&"clone"));
    assert!(!ports.base_applied().iter().any(|id| id == "base.pkg.store"));

    packages.online.store(true, Ordering::SeqCst);
    let online = ran(ports.pass(Trigger::Subscriber, Mode::Apply));
    assert_eq!(row(&online.rows, CLONE).outcome, Outcome::Changed);
    assert!(packages.git.load(Ordering::SeqCst));
    assert_eq!(
        *packages.calls.lock().unwrap(),
        ["seed", "seed", "update --defer-busy-flip"]
    );
    assert!(
        online
            .rows
            .iter()
            .position(|row| row.id == "sync.repository-prerequisites")
            .unwrap()
            < online.rows.iter().position(|row| row.id == CLONE).unwrap()
    );
    let calls = packages.calls.lock().unwrap().len();
    ports.pass(Trigger::Subscriber, Mode::Apply).unwrap();
    assert_eq!(
        packages.calls.lock().unwrap().len(),
        calls,
        "ready Git needs no new package attempt"
    );
}
