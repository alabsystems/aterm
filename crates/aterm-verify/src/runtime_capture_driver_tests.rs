// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::VecDeque;

const TARGET: &str = "gui/501/systems.alab.aterm.runtime.take-1";

fn identity() -> Identity {
    Identity {
        pid: 42,
        birth: aterm_uds::process::Birth(100),
        idversion: aterm_uds::process::KEEPS_PID_VERSIONS.then_some(7),
    }
}

fn request_at(directory: &str) -> Request {
    Request {
        schema: "aterm-runtime-driver-request-v1".into(),
        nonce: "0123456789abcdef0123456789abcdef".into(),
        directory: directory.into(),
        label: "systems.alab.aterm.runtime.take-1".into(),
    }
}

fn state(ended: bool) -> String {
    format!(
        "{TARGET} = {{\n\tstate = {}\n\truns = 1\n{}}}\n",
        if ended { "not running" } else { "running" },
        if ended { "" } else { "\tpid = 42\n" }
    )
}

struct Fake {
    request: Request,
    binding: Binding,
    values: Vec<Option<u64>>,
    index: usize,
    now: Instant,
    jobs: VecDeque<Result<String, String>>,
    identities: usize,
    change_at: Option<usize>,
    changed: Option<Identity>,
    missing_until: usize,
    samples: usize,
    extra_progress: Option<(usize, Option<u64>)>,
}

impl Fake {
    fn new(values: Vec<Option<u64>>) -> Self {
        let request = request_at("/private/phase");
        Self {
            binding: Binding {
                schema: "aterm-runtime-driver-binding-v1".into(),
                nonce: request.nonce.clone(),
                directory: request.directory.clone(),
                target: TARGET.into(),
                identity: identity().token(),
            },
            request,
            values,
            index: 0,
            now: Instant::now(),
            jobs: VecDeque::new(),
            identities: 0,
            change_at: None,
            changed: None,
            missing_until: 0,
            samples: 0,
            extra_progress: None,
        }
    }
    fn run(&mut self) -> Observation {
        let request = request_at(&self.request.directory);
        observe(&request, TARGET, self)
    }
}

impl Driver for Fake {
    fn job(&mut self) -> Result<String, String> {
        self.jobs
            .pop_front()
            .unwrap_or_else(|| Ok(state(self.index >= self.values.len())))
    }
    fn binding(&mut self) -> Result<Option<Binding>, String> {
        Ok((self.index >= self.missing_until).then(|| self.binding.clone()))
    }
    fn identity(&mut self, _: i32) -> Option<Identity> {
        self.identities += 1;
        if self.change_at.is_some_and(|at| self.identities >= at) {
            self.changed
        } else {
            Some(identity())
        }
    }
    fn progress(&mut self, _: u32) -> Option<u64> {
        self.samples += 1;
        if let Some((sample, work)) = self.extra_progress {
            if sample == self.samples {
                return work;
            }
        }
        self.values[self.index]
    }
    fn now(&mut self) -> Instant {
        self.now
    }
    fn pause(&mut self) {
        self.now += Duration::from_secs(30);
        self.index += 1;
    }
}

#[test]
fn advancing_original_guest_work_can_outlive_many_wall_minutes() {
    let mut fake = Fake::new((0..12).map(Some).collect());
    let result = fake.run();
    assert_eq!(result.outcome, "ended");
    assert_eq!(fake.index, 12);
    assert_eq!(fake.samples, 12);
    assert_eq!(result.original_identity, Some(identity().token()));
}

#[test]
fn only_a_whole_comparable_idle_minute_can_name_a_hang() {
    let mut fake = Fake::new(vec![Some(9); 8]);
    let result = fake.run();
    assert_eq!(result.outcome, "idle");
    assert_eq!(fake.index, 2);
    assert_eq!(result.observations, 3);
    assert!(result.reason.unwrap().contains("CPU/I/O"));
}

#[test]
fn exact_ended_at_the_final_idle_confirmation_wins() {
    let mut fake = Fake::new(vec![Some(9); 8]);
    fake.jobs = (0..6)
        .map(|_| Ok(state(false)))
        .chain(std::iter::once(Ok(state(true))))
        .collect();
    assert_eq!(fake.run().outcome, "ended");
    assert_eq!(fake.index, 2);
    assert_eq!(
        fake.samples, 4,
        "the idle candidate had its final fresh native sample"
    );
}

#[test]
fn work_resuming_during_the_final_job_read_cancels_the_idle_candidate() {
    let mut fake = Fake::new(vec![Some(0), Some(0), Some(0), Some(1)]);
    fake.extra_progress = Some((5, Some(1)));
    assert_eq!(fake.run().outcome, "ended");
    assert_eq!(fake.index, 4);
    assert_eq!(fake.samples, 6);
}

#[test]
fn unread_or_malformed_job_state_is_unknown_and_never_zero_work() {
    for reading in [
        Err("native job observation unread".into()),
        Ok("partial job description".into()),
    ] {
        let mut fake = Fake::new(vec![Some(0); 8]);
        fake.jobs = [reading.clone(), reading].into();
        assert_eq!(fake.run().outcome, "unknown");
        assert_eq!(fake.samples, 0);
        assert_eq!(fake.identities, 0);
    }
}

#[test]
fn incomparable_samples_disarm_and_require_a_fresh_full_idle_window() {
    let mut fake = Fake::new(vec![
        Some(1),
        None,
        None,
        None,
        None,
        Some(1),
        Some(1),
        Some(1),
    ]);
    let result = fake.run();
    assert_eq!(result.outcome, "idle");
    assert_eq!(fake.index, 7);
    assert_eq!(result.incomparable, 4);
}

#[test]
fn missing_publication_is_incomparable_and_fast_ended_needs_no_fake_identity() {
    let mut fast = Fake::new(vec![]);
    let result = fast.run();
    assert_eq!(result.outcome, "ended");
    assert_eq!(result.original_identity, None);
    assert_eq!(fast.identities, 0);
    assert_eq!(fast.samples, 0);
    let mut late = Fake::new(vec![Some(0); 8]);
    late.missing_until = 5;
    let result = late.run();
    assert_eq!(result.outcome, "idle");
    assert_eq!(late.index, 7);
    assert_eq!(result.incomparable, 5);
}

#[test]
fn mismatched_private_binding_never_admits_a_process() {
    for field in ["nonce", "directory", "target", "identity"] {
        let mut fake = Fake::new(vec![Some(0); 8]);
        match field {
            "nonce" => fake.binding.nonce.push('0'),
            "directory" => fake.binding.directory.push('x'),
            "target" => fake.binding.target.push('x'),
            _ => fake.binding.identity = "42".into(),
        }
        let result = fake.run();
        assert_eq!(result.outcome, "unknown", "{field}");
        assert_eq!(fake.samples, 0);
        assert!(result.reason.is_some());
    }
}

#[test]
fn native_unread_reuse_or_exec_never_relatches_a_successor() {
    let mut reused = identity();
    reused.birth.0 += 1;
    let mut exec = identity();
    exec.idversion = Some(8);
    for changed in [None, Some(reused), Some(exec)] {
        let mut fake = Fake::new(vec![Some(0); 8]);
        fake.change_at = Some(3); // identity changed during the first native tree read
        fake.changed = changed;
        let result = fake.run();
        assert_eq!(result.outcome, "unknown");
        assert_eq!(result.original_identity, Some(identity().token()));
        assert_eq!(fake.samples, 1);
        assert_eq!(fake.index, 0);
    }
}

#[test]
fn exact_ended_wins_a_sample_race_but_an_ended_successor_does_not() {
    let mut fake = Fake::new(vec![Some(0); 8]);
    fake.change_at = Some(3);
    fake.jobs = [Ok(state(false)), Ok(state(true))].into();
    assert_eq!(fake.run().outcome, "ended");
    let mut successor = Fake::new(vec![]);
    successor.jobs = [
        Ok(state(true).replace("runs = 1", "runs = 2")),
        Ok(state(true).replace("runs = 1", "runs = 2")),
    ]
    .into();
    assert_eq!(successor.run().outcome, "unknown");
    assert_eq!(successor.identities, 0);
    assert!(
        job(
            TARGET,
            &state(false).replace("\tpid = 42", "\tchild = {\n\t\tpid = 42\n\t}")
        )
        .unwrap()
        .1
        .is_none()
    );
}

#[test]
fn self_binding_is_atomic_single_use_and_does_not_replace_existing_evidence() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let req = request_at(&tmp.path().display().to_string());
    std::fs::write(
        tmp.path().join("driver-request.json"),
        aterm_json::json!({
            "schema":req.schema, "nonce":req.nonce, "directory":req.directory, "label":req.label
        })
        .to_string(),
    )
    .unwrap();
    publish_self(tmp.path()).unwrap();
    let binding: Binding = read_record(&tmp.path().join("driver-binding.json")).unwrap();
    assert_eq!(
        binding.identity,
        aterm_uds::process::read_identity(std::process::id() as i32)
            .unwrap()
            .token()
    );
    assert!(!tmp.path().join("driver-binding.json.tmp").exists());
    let before = std::fs::read(tmp.path().join("driver-binding.json")).unwrap();
    assert!(publish_self(tmp.path()).is_err());
    assert_eq!(
        std::fs::read(tmp.path().join("driver-binding.json")).unwrap(),
        before
    );
}
