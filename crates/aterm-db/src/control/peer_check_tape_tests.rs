// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_secrets::client::Paths;

/// One event of the tape, its kernel values as codes (the kernel's enums carry no `Debug`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A check began at this custody code, with or without a peer log to combine.
    Begin(u8, bool),
    /// A stage (its code) read, classified to this outcome code.
    Read(u8, u8),
    /// The check passed: it minted a pass.
    Pass,
    /// The check refused.
    Refuse,
    /// A grant spent a pass.
    Grant,
}

thread_local! {
    static TAPE: std::cell::RefCell<Option<Vec<Step>>> = const { std::cell::RefCell::new(None) };
    static PROBES: std::cell::RefCell<Option<Probes>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn note(step: Step) {
    TAPE.with(|tape| {
        if let Some(tape) = tape.borrow_mut().as_mut() {
            tape.push(step);
        }
    });
    PROBES.with(|probes| {
        if let Some(probes) = probes.borrow_mut().as_mut() {
            match step {
                Step::Begin(..) => probes.began = Some(Looked::now()),
                Step::Pass | Step::Refuse => {
                    if let Some(began) = probes.began.take() {
                        probes.checks.push(Looked::now().since(began));
                    }
                }
                Step::Read(..) | Step::Grant => {}
            }
        }
    });
}

/// Run `run` with this thread's checks and grants taped, and the tape.
pub(crate) fn recorded<T>(run: impl FnOnce() -> T) -> (T, Vec<Step>) {
    struct Restore(Option<Vec<Step>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TAPE.with(|tape| *tape.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(TAPE.with(|tape| tape.replace(Some(Vec::new()))));
    let out = run();
    let steps = TAPE.with(|tape| tape.replace(None)).unwrap_or_default();
    drop(restore);
    (out, steps)
}

/// What one check took of its vault ([`probed`]): the looks it opened on an observation of their
/// own (`aterm_secrets::client::test_looks_observed`), and how many times its looks stamped the
/// files the vault's memos read (`aterm_secrets::client::Derivations::vault_stamps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Looked {
    pub(crate) looks: usize,
    pub(crate) vault_stamps: usize,
}

impl Looked {
    fn now() -> Self {
        Self {
            looks: aterm_secrets::client::test_looks_observed(),
            vault_stamps: aterm_secrets::client::test_look_derivations()
                .derived
                .vault_stamps,
        }
    }

    const fn since(self, before: Self) -> Self {
        Self {
            looks: self.looks - before.looks,
            vault_stamps: self.vault_stamps - before.vault_stamps,
        }
    }
}

type Probe = Box<dyn FnMut(&Paths)>;

/// What [`probed`] installs in a thread, and what it counted there.
struct Probes {
    on_look: Probe,
    between: Probe,
    began: Option<Looked>,
    checks: Vec<Looked>,
}

/// A check took a look at the vault at `paths` for its next stage (`peer_check::through`).
pub(super) fn look_taken(paths: &Paths) {
    call(paths, |probes| &mut probes.on_look);
}

/// A check read a stage of the vault at `paths` and has not asked the kernel for the next.
pub(super) fn between(paths: &Paths) {
    call(paths, |probes| &mut probes.between);
}

/// Call the probe `pick` names with `paths`, taken out of its slot while it runs, so it runs with
/// no borrow of this thread's probes held.
fn call(paths: &Paths, pick: fn(&mut Probes) -> &mut Probe) {
    let taken = PROBES.with(|probes| {
        probes
            .borrow_mut()
            .as_mut()
            .map(|probes| std::mem::replace(pick(probes), Box::new(|_| {})))
    });
    if let Some(mut probe) = taken {
        probe(paths);
        PROBES.with(|probes| {
            if let Some(probes) = probes.borrow_mut().as_mut() {
                *pick(probes) = probe;
            }
        });
    }
}

/// Run `run` with this thread's checks probed: `on_look` called with the vault's paths each time
/// a check takes a look at it, `between` each time a check read a stage of it and before it asks
/// for the next, and what each check took of the vault ([`Looked`]), in order.
pub(crate) fn probed<T>(
    on_look: impl FnMut(&Paths) + 'static,
    between: impl FnMut(&Paths) + 'static,
    run: impl FnOnce() -> T,
) -> (T, Vec<Looked>) {
    struct Restore(Option<Probes>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBES.with(|probes| *probes.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(PROBES.with(|probes| {
        probes.replace(Some(Probes {
            on_look: Box::new(on_look),
            between: Box::new(between),
            began: None,
            checks: Vec::new(),
        }))
    }));
    let out = run();
    let checks = PROBES
        .with(|probes| probes.replace(None))
        .map(|probes| probes.checks)
        .unwrap_or_default();
    drop(restore);
    (out, checks)
}
