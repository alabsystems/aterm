// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Counted native cancellation cleanup, independent of ordinary Begin admission.
use super::*;

const MAX_CLEANUP: usize = 8;
struct OwnedCleanup {
    control: enrollment::RetirementControl,
    env: Env,
    previous: Option<Arc<OwnedCleanup>>,
}
impl OwnedCleanup {
    fn depth(&self) -> usize {
        1 + self.previous.as_ref().map_or(0, |old| old.depth())
    }
    fn all_retired(&self) -> bool {
        self.control.work_retired() && self.previous.as_ref().is_none_or(|old| old.all_retired())
    }
    fn settle(&self) -> bool {
        if !self.all_retired() {
            return false;
        }
        let mut current = Some(self);
        let mut had_proof = false;
        while let Some(control) = current {
            if let Some(proof) = control.control.retired() {
                had_proof = true;
                if proof.finish(&control.env).is_ok() {
                    return true;
                }
            }
            current = control.previous.as_deref();
        }
        !had_proof
    }
}
type CleanupControls = BTreeMap<(String, String), Arc<OwnedCleanup>>;
fn registry() -> &'static Mutex<Registry> {
    static TABLE: OnceLock<Mutex<Registry>> = OnceLock::new();
    TABLE.get_or_init(Mutex::default)
}
fn cleanup_controls() -> &'static Mutex<CleanupControls> {
    static TABLE: OnceLock<Mutex<CleanupControls>> = OnceLock::new();
    TABLE.get_or_init(Mutex::default)
}
fn remove_cleanup(key: &(String, String), control: &Arc<OwnedCleanup>) {
    let mut table = cleanup_controls().lock().unwrap_or_else(|p| p.into_inner());
    if table
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, control))
    {
        table.remove(key);
    }
}
pub(super) fn pending(lifetime: &account_work::Lifetime) -> bool {
    cleanup_controls()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .any(|control| control.control.owned_by(lifetime))
}
pub(super) fn seal_controls(lifetime: &account_work::Lifetime) -> bool {
    let owned: Vec<_> = cleanup_controls()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(_, control)| control.control.owned_by(lifetime))
        .map(|(key, control)| (key.clone(), Arc::clone(control)))
        .collect();
    let mut sealed = true;
    for (key, control) in owned {
        if !control.settle() {
            sealed = false;
            continue;
        }
        remove_cleanup(&key, &control);
    }
    sealed
}

enum Observed {
    Terminal(Value),
    Cleanup(enrollment::Request),
}
fn observe(
    env: &Env,
    sid: &str,
    proof: &crate::self_claim::Proven,
    command: &Command,
) -> Result<Option<Observed>, ()> {
    if matches!(command, Command::Discover(_)) {
        return Ok(None);
    }
    let store = env.accounts_store_for(accounts::Cli::Codex).ok_or(())?;
    let logins = match store.load_logins(env.now) {
        Ok((logins, None)) => logins,
        _ => return Ok(None), // Ordinary counted repair remains the only repair path.
    };
    let owner = caller(sid, proof);
    let Ok((intent, status)) = logins.enrollment.retained(command.id(), &owner) else {
        return Ok(None);
    };
    if let Command::Enroll {
        revision, account, ..
    } = command
        && (intent.revision != *revision || intent.account_id != *account)
    {
        return Err(());
    }
    let Ok(request) = logins.enrollment.admitted(command.id(), &owner) else {
        return Ok(None);
    };
    if matches!(
        status.phase,
        enrollment::Phase::Applied | enrollment::Phase::Cancelled
    ) && logins
        .enrollment
        .native_admitted(&request)
        .map_err(|_| ())?
    {
        return Ok(Some(Observed::Terminal(progress(&intent, &status))));
    }
    if status.phase == enrollment::Phase::CancelRequested
        && logins
            .enrollment
            .native_recovery_pending(&request)
            .map_err(|_| ())?
    {
        return Ok(Some(Observed::Cleanup(request)));
    }
    Ok(None)
}

struct DispatchRollback {
    key: Key,
    job: Arc<Job>,
    owner_key: (String, String),
    control: Arc<OwnedCleanup>,
    armed: bool,
    previous: Option<Arc<OwnedCleanup>>,
    pending: Arc<Mutex<Option<enrollment::RetirementWork>>>,
}
impl Drop for DispatchRollback {
    fn drop(&mut self) {
        if self.armed {
            registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.key, &self.job);
            let mut table = cleanup_controls().lock().unwrap_or_else(|p| p.into_inner());
            if table
                .get(&self.owner_key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.control))
            {
                table.remove(&self.owner_key);
                if let Some(previous) = &self.previous {
                    table.insert(self.owner_key.clone(), Arc::clone(previous));
                }
            }
            drop(table);
            // Registrations retire before the unstarted operation's final count.
            let pending = self
                .pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            drop(pending);
        }
    }
}

fn submit(
    env: &Env,
    sid: &str,
    proof: &crate::self_claim::Proven,
    command: &Command,
    lifetime: &account_work::Lifetime,
    authorize: impl FnOnce() -> bool + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> Result<Option<Arc<Job>>, ()> {
    let Some(observation) = observe(env, sid, proof, command)? else {
        return Ok(None);
    };
    let key = (sid.to_owned(), command.kind(), command.id().to_owned());
    if let Some(job) = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entries
        .get(&key)
        .cloned()
    {
        return if job.proof == *proof
            && job.command == *command
            && job
                .cleanup_owner
                .as_ref()
                .is_none_or(|owner| owner.owned_by(lifetime))
        {
            Ok(Some(job))
        } else {
            Err(())
        };
    }
    let request = match observation {
        Observed::Terminal(value) => {
            return Ok(Some(Arc::new(Job {
                cleanup_owner: None,
                command: command.clone(),
                proof: proof.clone(),
                started: std::time::Instant::now(),
                intent_sequence: 0,
                result: Mutex::new(Some(Ok(Some(value)))),
            })));
        }
        Observed::Cleanup(request) => request,
    };
    let owner_key = (sid.to_owned(), request.id.clone());
    // Snapshots only under mutexes. Native metadata/authentication/IO never runs
    // under either the ordinary or cleanup registry/control lock.
    let previous = controls()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&owner_key)
        .cloned();
    if let Some(previous) = &previous
        && (!previous.owned_by(lifetime) || !previous.work_retired())
    {
        return Err(());
    }
    let old_cleanup = cleanup_controls()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&owner_key)
        .cloned();
    if let Some(old) = &old_cleanup
        && (!old.control.owned_by(lifetime) || !old.control.work_retired())
    {
        return Err(());
    }
    let (work, control) =
        enrollment::reserve_cancelled_retirement(env, &request, lifetime).map_err(|_| ())?;
    let control = Arc::new(OwnedCleanup {
        control,
        env: env.clone(),
        previous: old_cleanup.clone(),
    });
    let job = Arc::new(Job {
        cleanup_owner: Some(control.control.clone()),
        command: command.clone(),
        proof: proof.clone(),
        started: std::time::Instant::now(),
        intent_sequence: 0,
        result: Mutex::new(None),
    });
    {
        let mut table = registry().lock().unwrap_or_else(|p| p.into_inner());
        table.prune();
        if let Some(existing) = table.entries.get(&key) {
            return if existing.proof == *proof
                && existing.command == *command
                && existing
                    .cleanup_owner
                    .as_ref()
                    .is_none_or(|owner| owner.owned_by(lifetime))
            {
                Ok(Some(Arc::clone(existing)))
            } else {
                Err(())
            };
        }
        if table.exhausted || table.entries.len() >= MAX_CLEANUP {
            return Err(());
        }
        let mut owned = cleanup_controls().lock().unwrap_or_else(|p| p.into_inner());
        let current = owned.get(&owner_key);
        if match (&old_cleanup, current) {
            (None, None) => false,
            (Some(old), Some(current)) => !Arc::ptr_eq(old, current),
            _ => true,
        } || owned.values().map(|control| control.depth()).sum::<usize>() >= MAX_CLEANUP
        {
            return Err(());
        }
        owned.insert(owner_key.clone(), Arc::clone(&control));
        table.insert(key.clone(), Arc::clone(&job));
    }
    let pending = Arc::new(Mutex::new(Some(work)));
    let mut rollback = DispatchRollback {
        key,
        job: Arc::clone(&job),
        owner_key: owner_key.clone(),
        control: Arc::clone(&control),
        armed: true,
        previous: old_cleanup,
        pending: Arc::clone(&pending),
    };
    let worker = Arc::clone(&job);
    let completion = Arc::clone(&control);
    let previous_normal = previous.clone();
    spawn(Box::new(move || {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let work = pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .ok_or(())?;
            if !authorize() {
                return Err(());
            }
            work.run()
                .map(|status| Some(progress(&request.intent(), &status)))
                .map_err(|_| ())
        })) {
            Ok(result) => result,
            Err(payload) => {
                if let Err(secondary) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
                {
                    std::mem::forget(secondary);
                }
                Err(())
            }
        };
        let successful = result.is_ok();
        complete_in(registry(), &worker, result);
        if successful && completion.control.work_retired() {
            remove_cleanup(&owner_key, &completion);
            if let Some(previous) = previous_normal {
                remove_control(&owner_key, &previous);
            }
        }
    }))
    .map_err(|_| ())?;
    rollback.armed = false;
    Ok(Some(job))
}

pub(super) fn serve_if_eligible(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    command: &Command,
    proof: &crate::self_claim::Proven,
    lifetime: &account_work::Lifetime,
) -> Option<String> {
    let env = match crate::native_act::codex_account_env() {
        Ok(env) => env,
        Err(_) => return None,
    };
    // SAFETY: this authenticated control request owns the live PTY master.
    // Its duplicate pins the descriptor through the worker's kernel recheck.
    let owned_master = match unsafe { BorrowedFd::borrow_raw(master) }.try_clone_to_owned() {
        Ok(master) => master,
        Err(_) => return Some("ERR codex-accounts-change refused\n".into()),
    };
    let weak = Arc::downgrade(ctx);
    let expected = proof.clone();
    let result = submit(
        &env,
        ctx.self_id.as_str(),
        proof,
        command,
        lifetime,
        move || {
            weak.upgrade().is_some_and(|ctx| {
                still_native(&ctx, &expected, owned_master.as_raw_fd())
                    && ctx.fabric.hold().is_none()
            })
        },
        |work| {
            std::thread::Builder::new()
                .name("codex-enrollment-cleanup".into())
                .spawn(work)
                .map(|_| ())
        },
    );
    match result {
        Ok(None) => None,
        Ok(Some(job)) if ctx.fabric.hold().is_none() => Some(reply(
            ctx,
            master,
            ("codex-accounts-change", false),
            command,
            proof,
            &job,
            registry(),
        )),
        Ok(Some(_)) | Err(()) => Some("ERR codex-accounts-change refused\n".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, serial};
    use super::*;
    use crate::App;

    fn native_fixture() -> (
        enrollment::NativeRowRecoveryFixture,
        crate::self_claim::Proven,
        String,
    ) {
        let (_world, _, proof, sid) = fixture();
        let native = enrollment::NativeRowRecoveryFixture::new(&caller(&sid, &proof));
        native.retire_originals().unwrap();
        native.cancel_first_after_publication_claim().unwrap();
        (native, proof, sid)
    }
    fn remove_job(sid: &str, command: &Command, job: &Arc<Job>) {
        registry().lock().unwrap().remove(
            &(sid.to_owned(), command.kind(), command.id().to_owned()),
            job,
        );
    }

    #[test]
    fn paused_cold_cleanup_is_counted_before_dispatch_and_independent_of_begin_budget() {
        let _serial = serial();
        let (native, proof, sid) = native_fixture();
        let request = native.requests()[0].clone();
        let env = native.env();
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let before = store.load_logins(env.now).unwrap().0;
        let roster = env.accounts_roster_for(accounts::Cli::Codex).unwrap();
        std::fs::write(&roster, "not [valid TOML").unwrap();
        let normal_keys: Vec<_> = (0..MAX_JOBS)
            .map(|index| {
                let command = Command::Poll(format!("{index:032x}"));
                let key = (sid.clone(), command.kind(), command.id().to_owned());
                jobs().lock().unwrap().insert(
                    key.clone(),
                    Arc::new(Job {
                        cleanup_owner: None,
                        command,
                        proof: proof.clone(),
                        started: std::time::Instant::now(),
                        intent_sequence: 0,
                        result: Mutex::new(None),
                    }),
                );
                key
            })
            .collect();
        let lifetime = account_work::Lifetime::default();
        let pause = lifetime.pause().unwrap();
        assert!(lifetime.enter().is_none());
        let command = Command::Poll(request.id.clone());
        let queued = std::cell::RefCell::new(None);
        let job = submit(
            env,
            &sid,
            &proof,
            &command,
            &lifetime,
            || true,
            |work| {
                assert!(App::enrollment_retirement_pending(&lifetime));
                assert!(jobs().try_lock().is_ok());
                assert!(controls().try_lock().is_ok());
                assert!(registry().try_lock().is_ok());
                assert!(cleanup_controls().try_lock().is_ok());
                *queued.borrow_mut() = Some(work);
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert!(
            job.result.lock().unwrap().is_none(),
            "counted Pending precedes Waiting"
        );
        let pause = match pause.try_wait() {
            Err(pause) => pause,
            Ok(_) => panic!("queued release owns the drain count"),
        };
        let foreign = account_work::Lifetime::default();
        assert!(
            submit(
                env,
                &sid,
                &proof,
                &command,
                &foreign,
                || true,
                |_| panic!("foreign App cannot dispatch")
            )
            .is_err()
        );
        assert!(foreign.pause().unwrap().try_wait().is_ok());
        queued.borrow_mut().take().unwrap()();
        assert_eq!(
            job.result
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()["phase"]
                .as_str(),
            Some("cancelled")
        );
        assert!(!App::enrollment_retirement_pending(&lifetime));
        let drained = pause
            .try_wait()
            .unwrap_or_else(|_| panic!("actual cleanup retired"));
        let after = store.load_logins(env.now).unwrap().0;
        assert_eq!(before.labels, after.labels);
        assert_eq!(before.generation_clock, after.generation_clock);
        assert_eq!(std::fs::read_to_string(&roster).unwrap(), "not [valid TOML");
        remove_job(&sid, &command, &job);
        let replay = submit(
            env,
            &sid,
            &proof,
            &command,
            &lifetime,
            || panic!("terminal replay is metadata only"),
            |_| panic!("sealed terminal cannot dispatch"),
        )
        .unwrap()
        .unwrap();
        assert!(replay.result.lock().unwrap().is_some());
        drop(drained);
        for key in normal_keys {
            jobs().lock().unwrap().entries.remove(&key);
        }
    }

    #[test]
    fn cold_cleanup_rejects_changed_body_and_caller_before_admission() {
        let _serial = serial();
        let (native, proof, sid) = native_fixture();
        let request = native.requests()[0].clone();
        let lifetime = account_work::Lifetime::default();
        let before = native
            .env()
            .accounts_store_for(accounts::Cli::Codex)
            .unwrap()
            .load_logins(native.env().now)
            .unwrap()
            .0;
        for command in [
            Command::Enroll {
                operation: request.id.clone(),
                revision: "f".repeat(64),
                account: accounts_control::account_id(&request.label),
            },
            Command::Enroll {
                operation: request.id.clone(),
                revision: request.revision.clone(),
                account: "f".repeat(64),
            },
        ] {
            assert!(
                submit(
                    native.env(),
                    &sid,
                    &proof,
                    &command,
                    &lifetime,
                    || true,
                    |_| panic!("changed body dispatch")
                )
                .is_err()
            );
        }
        let foreign = crate::self_claim::Proven {
            pid: proof.pid + 1,
            start: proof.start.clone(),
        };
        assert!(
            submit(
                native.env(),
                &sid,
                &foreign,
                &Command::Poll(request.id.clone()),
                &lifetime,
                || true,
                |_| panic!("foreign caller dispatch")
            )
            .unwrap()
            .is_none()
        );
        assert!(
            submit(
                native.env(),
                &sid,
                &proof,
                &Command::Poll("f".repeat(32)),
                &lifetime,
                || true,
                |_| panic!("unknown operation dispatch")
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            native
                .env()
                .accounts_store_for(accounts::Cli::Codex)
                .unwrap()
                .load_logins(native.env().now)
                .unwrap()
                .0,
            before
        );
        assert!(lifetime.pause().unwrap().try_wait().is_ok());
    }

    #[test]
    fn cold_cleanup_failed_dispatch_rolls_back_before_its_pending_count_retires() {
        let _serial = serial();
        let (native, proof, sid) = native_fixture();
        let request = native.requests()[0].clone();
        let command = Command::Poll(request.id.clone());
        let lifetime = account_work::Lifetime::default();
        let pause = std::cell::RefCell::new(Some(lifetime.pause().unwrap()));
        let store = native
            .env()
            .accounts_store_for(accounts::Cli::Codex)
            .unwrap();
        let before = store.load_logins(native.env().now).unwrap().0;
        assert!(
            submit(
                native.env(),
                &sid,
                &proof,
                &command,
                &lifetime,
                || true,
                |work| {
                    drop(work); // Failed spawn drops its callback before returning.
                    let waiting = pause.borrow_mut().take().unwrap();
                    let waiting = match waiting.try_wait() {
                        Err(waiting) => waiting,
                        Ok(_) => panic!("rollback still owns the unstarted Pending work"),
                    };
                    *pause.borrow_mut() = Some(waiting);
                    Err(std::io::Error::other("injected thread dispatch failure"))
                }
            )
            .is_err()
        );
        assert!(!pending(&lifetime));
        assert!(!registry().lock().unwrap().entries.contains_key(&(
            sid.clone(),
            command.kind(),
            command.id().to_owned()
        )));
        assert_eq!(store.load_logins(native.env().now).unwrap().0, before);
        let drained = pause
            .into_inner()
            .unwrap()
            .try_wait()
            .unwrap_or_else(|_| panic!("failed dispatch leaves no Pending count"));
        assert!(
            submit(
                native.env(),
                &sid,
                &proof,
                &command,
                &lifetime,
                || true,
                |_| panic!("sealed cleanup dispatch")
            )
            .is_err()
        );
        drop(drained);
    }

    #[test]
    fn cold_cleanup_can_complete_before_dispatch_returns() {
        let _serial = serial();
        let (native, proof, sid) = native_fixture();
        let command = Command::Cancel(native.requests()[0].id.clone());
        let lifetime = account_work::Lifetime::default();
        let pause = lifetime.pause().unwrap();
        let job = submit(
            native.env(),
            &sid,
            &proof,
            &command,
            &lifetime,
            || true,
            |work| {
                assert!(App::enrollment_retirement_pending(&lifetime));
                assert!(registry().try_lock().is_ok());
                assert!(cleanup_controls().try_lock().is_ok());
                work();
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert!(job.result.lock().unwrap().as_ref().unwrap().is_ok());
        assert!(!App::enrollment_retirement_pending(&lifetime));
        assert!(pause.try_wait().is_ok());
        remove_job(&sid, &command, &job);
    }

    #[test]
    fn cold_cleanup_write_failure_retains_nonce_for_actual_app_settlement() {
        let _serial = serial();
        let (native, proof, sid) = native_fixture();
        let request = native.requests()[0].clone();
        let command = Command::Poll(request.id.clone());
        let env = native.env();
        let store = env.accounts_store_for(accounts::Cli::Codex).unwrap();
        let before = store.load_logins(env.now).unwrap().0;
        let lifetime = account_work::Lifetime::default();
        let pause = lifetime.pause().unwrap();
        let queued = std::cell::RefCell::new(None);
        let job = submit(
            env,
            &sid,
            &proof,
            &command,
            &lifetime,
            || true,
            |work| {
                *queued.borrow_mut() = Some(work);
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        let control = cleanup_controls()
            .lock()
            .unwrap()
            .get(&(sid.clone(), request.id.clone()))
            .cloned()
            .unwrap();
        // This metadata-only transaction does not touch wake. Obstruct its
        // actual retained publication witness after counted admission instead.
        let witness = store.logins_witness_file();
        let original_witness = witness.with_extension("cold-retirement-held");
        std::fs::rename(&witness, &original_witness).unwrap();
        std::fs::create_dir(&witness).unwrap();
        queued.borrow_mut().take().unwrap()();
        assert!(job.result.lock().unwrap().as_ref().unwrap().is_err());
        assert!(control.control.work_retired());
        assert!(
            control.control.retired().is_some(),
            "actual nonce receipt survives write uncertainty"
        );
        assert!(App::enrollment_retirement_pending(&lifetime));
        let foreign = account_work::Lifetime::default();
        assert!(App::seal_enrollment_retirement(&foreign));
        assert!(App::enrollment_retirement_pending(&lifetime));
        std::fs::remove_dir(&witness).unwrap();
        std::fs::rename(&original_witness, &witness).unwrap();
        let drained = pause
            .try_wait()
            .unwrap_or_else(|_| panic!("kernel resources really retired"));
        assert!(App::seal_enrollment_retirement(&lifetime));
        assert!(!App::enrollment_retirement_pending(&lifetime));
        let after = store.load_logins(env.now).unwrap().0;
        assert_eq!(
            after
                .enrollment
                .retained(&request.id, &request.caller)
                .unwrap()
                .1
                .phase,
            enrollment::Phase::Cancelled
        );
        assert_eq!(before.labels, after.labels);
        assert_eq!(before.generation_clock, after.generation_clock);
        remove_job(&sid, &command, &job);
        drop(drained);
    }
}
