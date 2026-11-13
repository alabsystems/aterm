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
#[path = "codex_enrollment_cleanup_tests.rs"]
mod tests;
