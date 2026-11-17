// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node job's outer loop rests until what it waits for happens, not on a clock
//! (KI-1108-2, step 5). Each job here runs with an old tick of an hour, so every look a test
//! waits on came from the event the test made. Scratch state and loopback sockets only: no real
//! `~/.aterm`, vault server, broker or LAN.

use super::*;
use crate::fleet_listen::fleet_inputs::{idles, ring_node, waiting_on};
use aterm_agent::node_service::{NODE_PORT_LOCK, NodeJobLock, WINDOW_PORT_LOCK};
use aterm_spec::wait::{MUST_HAPPEN, until_within};

/// A tick no test outlives: a look at it would come long after any test's hang detector.
const NO_TICK: Duration = Duration::from_secs(3600);

/// A loopback address no listener holds now, for a job that binds it itself.
fn free_addr() -> SocketAddr {
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind a probe");
    probe.local_addr().expect("the probe's address")
}

/// A node job on `state` that binds `addr` itself, its `aterm.toml` holding `toml`, its old tick
/// an hour: how many generations it served, its stop, and its thread.
struct Job {
    state: PathBuf,
    config: PathBuf,
    served: Arc<AtomicUsize>,
    stop: Arc<Stop>,
    server: Option<std::thread::JoinHandle<ExitCode>>,
    _relay: aterm_tempfile::TempDir,
}

impl Job {
    fn start(root: &Path, state: &Path, toml: &str) -> Self {
        let relay = crate::test_scratch::short_dir("node-idle");
        let config = root.join("aterm.toml");
        crate::test_scratch::write_config(&config, toml);
        let stop = Arc::new(Stop::new());
        let served = Arc::new(AtomicUsize::new(0));
        let told = Arc::clone(&served);
        let cfg = NodeServeConfig {
            state_dir: state.to_path_buf(),
            relay_dir: relay.path().to_path_buf(),
            home_dir: None,
            config_path: Some(config.clone()),
            vault_paths: None,
            members: None,
            alab_browse: None,
            prebound_listener: None,
            on_generation: Some(Arc::new(move |_: &str| {
                told.fetch_add(1, Ordering::AcqRel);
            })),
            tick_every: NO_TICK,
            wake_timeout: Duration::from_secs(1),
            lan_unavailable: || None,
            stop: Arc::clone(&stop),
            launcher: idle_launcher(),
            updates: None,
            mesh: NodeMesh::off(),
        };
        Self {
            state: state.to_path_buf(),
            config,
            served,
            stop,
            server: Some(std::thread::spawn(move || run_node_serve_with(cfg))),
            _relay: relay,
        }
    }

    fn served(&self) -> usize {
        self.served.load(Ordering::Acquire)
    }

    fn until_served(&self) {
        until_within(MUST_HAPPEN, "the job serves", || {
            (self.served() >= 1).then_some(())
        });
    }

    /// Once the job rested more than `than` times: the count then.
    fn rested(&self, than: usize) -> usize {
        until_within(MUST_HAPPEN, "the job rests", || {
            let now = idles(&self.state);
            (now > than).then_some(now)
        })
    }

    fn finish(mut self) {
        self.stop.set();
        let server = self.server.take().expect("the job runs");
        assert_eq!(server.join().expect("join"), ExitCode::SUCCESS);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

/// `[fleet]` listening at `addr`, sharing `on` or off.
fn listening(addr: SocketAddr, on: bool) -> String {
    format!("[swarm]\nenabled = {on}\n[fleet]\nlisten = \"{addr}\"\nallow = \"facts\"\n")
}

/// A JOB WITH NOTHING TO SERVE SERVES AT ITS INPUTS' EVENT: sharing turned on in `aterm.toml`
/// wakes a job that rested with it off, and it serves.
/// NEGATIVE CONTROL: a ring that changes nothing wakes it too, and it looks again and serves
/// nothing.
#[test]
fn a_job_with_nothing_to_serve_serves_at_its_inputs_event() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let addr = free_addr();
    let job = Job::start(root.path(), &state, &listening(addr, false));
    let rested = job.rested(0);
    ring_node(&state);
    job.rested(rested);
    assert_eq!(job.served(), 0, "CONTROL: sharing off serves nothing");
    crate::test_scratch::write_config(&job.config, listening(addr, true));
    job.until_served();
    job.finish();
}

/// A JOB HELD OFF THE PORT TAKES IT AT ITS RELEASE: while another holds `node-port.lock` the job
/// rests on that lock, and its release wakes the job, which takes it and serves.
/// NEGATIVE CONTROL: while it is held, the job waits on it and serves nothing.
#[test]
fn a_job_held_off_the_port_takes_it_at_its_release() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let held = NodeJobLock::try_acquire_port(&state)
        .expect("read the port lock")
        .expect("held here first");
    let job = Job::start(root.path(), &state, &listening(free_addr(), true));
    until_within(MUST_HAPPEN, "the job waits for the port's release", || {
        waiting_on(&state, NODE_PORT_LOCK).then_some(())
    });
    assert_eq!(job.served(), 0, "CONTROL: the port is held elsewhere");
    drop(held);
    job.until_served();
    job.finish();
}

/// A WINDOW LETTING THE PORT GO WAKES THE JOB AT ONCE: the job's bind meets the port a window
/// holds with `window-port.lock`, rests on that lock, and binds once the window closes its
/// listener and lets the lock go.
/// NEGATIVE CONTROL: while the window holds both, the job waits on the window's lock and serves
/// nothing.
#[test]
fn a_window_letting_the_port_go_wakes_the_job_at_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    seed_fleet_node(&state);
    let window = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("the window's listener");
    let addr = window.local_addr().unwrap();
    let window_port = NodeJobLock::try_acquire_window_port(&state)
        .expect("read the window's lock")
        .expect("the window's custody");
    let job = Job::start(root.path(), &state, &listening(addr, true));
    until_within(
        MUST_HAPPEN,
        "the job waits for the window's release",
        || waiting_on(&state, WINDOW_PORT_LOCK).then_some(()),
    );
    assert_eq!(job.served(), 0, "CONTROL: the port is the window's");
    drop(window);
    drop(window_port);
    job.until_served();
    job.finish();
}
