// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real live snapshot reader belongs off the event loop: its production
//! control round trips can require that same loop to answer a status hop.

use super::*;
use aterm_agent::harness::{native_act::Process, relaunch, upgrade_drive};

pub(super) struct Reader {
    stop: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<Result<(), String>>,
}

impl Reader {
    pub(super) fn start(
        ctx: Arc<crate::SessionCtx>,
        master: i32,
        mut opts: upgrade_drive::Opts,
        root: PathBuf,
    ) -> Result<Self, String> {
        opts.only_sid = Some(ctx.self_id.as_str().to_owned());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = std::thread::Builder::new().name("native-live-snapshot".into()).spawn(move || {
            let mut previous = None;
            while !stopping.load(Ordering::Acquire) {
                let observed = live_snapshot(&ctx, master, &opts);
                if stopping.load(Ordering::Acquire) { break }
                let (outcome, pid) = match observed {
                    Ok((snapshot, process)) => {
                        crate::harness_host::publish_native_fixture_snapshot(ctx.self_id.as_str(), &snapshot);
                        if crate::native_act::fixture_live_conversation(&ctx, master, &process) {
                            let mono = aterm_agent::harness::native_act::monotonic_us()
                                .ok_or("fixture live snapshot clock unavailable")?;
                            let first = root.join("live-snapshot-first-ready.json");
                            if !first.exists() {
                                let event = aterm_json::json!({"pid":process.pid,"birth":process.birth,
                                    "shellPid":snapshot.shell,"thread":snapshot.session,
                                    "conversationProven":true,"monotonicUs":mono});
                                let bytes = aterm_json::to_vec(&event).map_err(|e|e.to_string())?;
                                aterm_uds::atomic_file::write(&first, &bytes, 0o600).map_err(|e|e.to_string())?;
                            }
                            ("published", Some(process.pid))
                        } else { ("conversation-unavailable", Some(process.pid)) }
                    }
                    Err(why) => (why, None),
                };
                let key = (outcome, pid);
                if previous.as_ref() != Some(&key) {
                    ctx.native_policy.renewal_control_observed(&aterm_json::json!({"kind":"live-snapshot",
                        "outcome":outcome,"pid":pid,"monotonicUs":aterm_agent::harness::native_act::monotonic_us()}));
                    previous = Some(key);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        }).map_err(|e|e.to_string())?;
        Ok(Self { stop, worker })
    }

    pub(super) fn shutdown(self, mut pump: impl FnMut()) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while !self.worker.is_finished() {
            assert!(
                std::time::Instant::now() < until,
                "fixture live snapshot shutdown hang detector"
            );
            pump();
        }
        self.worker
            .join()
            .map_err(|_| "fixture live snapshot worker panicked".to_owned())?
    }
}

fn live_snapshot(
    ctx: &crate::SessionCtx,
    master: i32,
    opts: &upgrade_drive::Opts,
) -> Result<(relaunch::Snapshot, Process), &'static str> {
    let copy = crate::self_claim::live_copy(ctx).ok_or("no-live-primary")?;
    let resume = copy.resume.as_ref().map_err(|_| "no-live-resume")?;
    let before = Process::observe(copy.pid).ok_or("source-unreadable")?;
    // This is the ordinary production live reader, including actual roster,
    // foreground job, terminal owner, argv/start/image and host-proven copy.
    let snapshot = relaunch::snapshot_with_claim(opts, Some(&copy))?;
    let current = crate::self_claim::live_copy(ctx).ok_or("primary-no-longer-live")?;
    if current.pid != copy.pid
        || current.start != copy.start
        || current.home != copy.home
        || current.resume.as_ref().ok() != Some(resume)
        || Process::observe(copy.pid).as_ref() != Some(&before)
        || snapshot.pid != before.pid
        || snapshot.shell != before.parent
        || snapshot.start != copy.start
        || snapshot.tab != ctx.self_id.as_str()
        || snapshot.session.as_deref() != Some(resume.session.as_str())
        || !aterm_agent::harness::self_claimed::is_uuid(&resume.session)
        || aterm_agent::harness::native_act::Image::at(&snapshot.program).as_ref()
            != Some(&before.image)
        || !matches!(&snapshot.run, relaunch::AgentRun::Codex(run)
            if run.home == copy.home && run.self_claimed.as_ref() == Some(resume))
        || before.group != before.pid
        || u32::try_from(crate::quit_safety::foreground_pgrp(master)).ok() != Some(before.group)
    {
        return Err("live-primary-changed");
    }
    let shell = Process::observe(snapshot.shell).ok_or("shell-unreadable")?;
    if before.session != shell.session {
        return Err("live-shell-changed");
    }
    Ok((snapshot, before))
}

#[cfg(test)]
#[test]
fn codex_renewal_live_reader_keeps_an_unclaimed_session_unpublished_and_joins() {
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = crate::session_store::test_handle_on(8004, -1);
    let opts = crate::harness_host::window_drive_opts(
        root.path().join("home"),
        root.path().join("state"),
        None,
    );
    assert!(matches!(
        live_snapshot(&handle.ctx, -1, &opts),
        Err("no-live-primary")
    ));
    let reader =
        Reader::start(Arc::clone(&handle.ctx), -1, opts, root.path().to_path_buf()).unwrap();
    reader
        .shutdown(|| std::thread::sleep(Duration::from_millis(1)))
        .unwrap();
    assert!(!root.path().join("live-snapshot-first-ready.json").exists());
    assert!(crate::harness_host::published_snapshot(handle.sid.as_str()).is_none());
}
