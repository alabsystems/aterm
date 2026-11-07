use std::ffi::CString;
use std::num::NonZeroU32;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use super::*;

fn local_fifo(dir: &std::path::Path, width: usize) -> jobserver::Client {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = dir.join(format!("credits-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    let path_c = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: this NUL-terminated path lies in the fixture's private directory.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    let client = jobserver::Client::from_fifo(&path).unwrap();
    for _ in 0..width {
        client.release_raw().unwrap();
    }
    client.acquire_raw().unwrap();
    client
}

fn fixture() -> (tempfile::TempDir, Pool, jobserver::Client, WorkerClient, Arc<AtomicU32>) {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open_in(dir.path(), NonZeroU32::new(3).unwrap()).unwrap();
    let implicit = Arc::new(pool.try_acquire(Owner::current(None)).unwrap().unwrap());
    let local = local_fifo(dir.path(), 3);
    let limit = Arc::new(AtomicU32::new(1));
    let observation = Arc::clone(&limit);
    let client = WorkerClient::new(
        local.clone(),
        pool.clone(),
        implicit,
        Arc::new(move |_| observation.load(Ordering::Acquire)),
        Owner::current(None),
    )
    .unwrap();
    (dir, pool, local, client, limit)
}

#[test]
fn global_refusal_returns_the_local_token_and_growth_is_observed() {
    let _fixture = crate::tests::kernel_fixture();
    let (_dir, _pool, local, client, limit) = fixture();
    assert_eq!(client.try_pause_extras().err().unwrap().kind(), io::ErrorKind::Unsupported);
    assert!(client.extra_admission_running());
    assert!(client.try_acquire().unwrap().is_none());
    assert_eq!(local.available().unwrap(), 2);
    limit.store(3, Ordering::Release);
    let credit = client.try_acquire().unwrap().unwrap();
    assert_eq!(local.available().unwrap(), 1);
    limit.store(0, Ordering::Release);
    assert!(client.try_acquire().unwrap().is_none());
    assert_eq!(local.available().unwrap(), 1);
    drop(credit);
    assert_eq!(local.available().unwrap(), 2);
}

#[test]
fn cancellation_stops_waiting_without_revoking_an_active_credit() {
    let _fixture = crate::tests::kernel_fixture();
    let (_dir, pool, local, client, limit) = fixture();
    let (tx, rx) = mpsc::channel();
    let helper = client
        .clone()
        .into_helper_thread(move |credit| {
            tx.send(credit).unwrap();
        })
        .unwrap();
    helper.request_token();
    assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
    // An in-flight nonblocking attempt briefly owns a local token before a
    // global refusal returns it. Sample return after helper join below;
    // global_refusal_returns_the_local_token checks the completed attempt.
    limit.store(3, Ordering::Release);
    let mut active = rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    assert!(active.commit());
    limit.store(0, Ordering::Release);
    helper.request_token();
    let start = Instant::now();
    drop(helper);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(
        pool.snapshot()
            .unwrap()
            .iter()
            .filter(|s| matches!(s, crate::SlotState::Held { .. }))
            .count(),
        2
    );
    drop(active);
    assert_eq!(local.available().unwrap(), 2);
}

#[test]
fn cancellation_wins_after_acquisition_before_callback_dispatch() {
    let _fixture = crate::tests::kernel_fixture();
    let (_dir, _pool, _local, client, limit) = fixture();
    limit.store(3, Ordering::Release);
    let (offered_tx, offered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (committed_tx, committed_rx) = mpsc::channel();
    let helper = client
        .into_helper_thread(move |credit| {
            let mut credit = credit.unwrap();
            offered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            committed_tx.send(credit.commit()).unwrap();
        })
        .unwrap();
    helper.request_token();
    offered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    helper.cancel();
    resume_tx.send(()).unwrap();
    assert!(!committed_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    drop(helper);
}

#[test]
fn independent_roots_share_capacity_and_released_workers_move_between_them() {
    let _fixture = crate::tests::kernel_fixture();
    let (dir, pool, _local, first, limit) = fixture();
    limit.store(3, Ordering::Release);
    let implicit = Arc::new(pool.try_acquire(Owner::current(None)).unwrap().unwrap());
    let local = local_fifo(dir.path(), 2);
    let second =
        WorkerClient::new(local, pool, implicit, Arc::new(|_| 3), Owner::current(None)).unwrap();
    let first_credit = first.try_acquire().unwrap().unwrap();
    assert!(second.try_acquire().unwrap().is_none());
    drop(first_credit);
    assert!(second.try_acquire().unwrap().is_some());
}
