// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_exit_line_names_astreams_reason_and_who_reopens_the_log() {
    let reason = "a flush failed (EIO)\nbadly";
    let supervised = poisoned_line("/r/bus.log", reason, None);
    assert_eq!(
        supervised,
        "aterm link broker: /r/bus.log: poisoned, exiting 75 so the supervisor reopens it \
         (astream: a flush failed (EIO) badly)"
    );
    let argv = "aterm link broker /r/bus.sock /r/bus.log --secret-file /r/mint.secret";
    let alone = poisoned_line("/r/bus.log", reason, Some(argv));
    assert_eq!(
        alone,
        format!(
            "aterm link broker: /r/bus.log: poisoned, exiting 75; no `aterm fabric on` job \
             runs this broker, so unless something else restarts it, restart it as `{argv}` \
             (astream: a flush failed (EIO) badly)"
        )
    );
    assert!(!alone.contains("so the supervisor reopens it"), "{alone}");
    assert!(!alone.contains("no supervisor"), "{alone}");
    assert!(!supervised.contains("restart it as"), "{supervised}");
    assert!(!supervised.contains(UNSUPERVISED_WORDS), "{supervised}");
    for line in [&supervised, &alone] {
        assert!(!line.contains('\n'), "{line}");
        assert!(
            line.starts_with(&format!(
                "aterm link broker: /r/bus.log: {POISONED_EXIT_WORDS}"
            )),
            "{line}"
        );
        assert!(
            line.ends_with("(astream: a flush failed (EIO) badly)"),
            "{line}"
        );
    }
}

/// A REPLACED BUILD'S EXIT LINE SAYS WHO STARTS THE NEW ONE (fix unit E5): under its
/// supervisor, that it starts the new build; with no `aterm fabric on` job running it, the
/// command to start it again, as the poisoned exit gives it, and never that a supervisor
/// will. NEGATIVE CONTROL (scratch): one wording for both arms fails the unsupervised one.
#[test]
fn a_replaced_builds_exit_line_names_who_starts_the_new_one() {
    let path = "/opt/x/aterm";
    let argv = "aterm link broker /r/bus.sock /r/bus.log --secret-file /r/mint.secret";
    let supervised = replaced_line(path, None);
    let alone = replaced_line(path, Some(argv));
    assert!(supervised.contains(SUPERVISOR_STARTS_WORDS), "{supervised}");
    assert!(!supervised.contains(UNSUPERVISED_WORDS), "{supervised}");
    assert!(!supervised.contains(argv), "{supervised}");
    assert!(alone.contains(UNSUPERVISED_WORDS), "{alone}");
    assert!(alone.contains(&format!("`{argv}`")), "{alone}");
    assert!(!alone.contains(SUPERVISOR_STARTS_WORDS), "{alone}");
    for line in [&supervised, &alone] {
        assert!(line.starts_with(path), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }
}

#[test]
fn a_supervisor_is_the_jobs_own_word() {
    use crate::enable::LABEL;
    let xpc = |s: &str| supervised_by(Some(OsStr::new(s)), None);
    let cgroup = |s: &str| supervised_by(None, Some(s));
    assert!(xpc(LABEL));
    assert!(xpc(&format!("{LABEL}.4294967295")));
    assert!(cgroup(&format!(
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/{LABEL}.service\n"
    )));
    assert!(cgroup(&format!(
        "12:pids:/user.slice\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/\
         {LABEL}.123.service\n"
    )));
    assert!(!xpc("0"));
    assert!(!xpc("application.systems.alab.aterm.12345.67890"));
    assert!(!xpc(&format!("{LABEL}-other")));
    assert!(!xpc(&format!("{LABEL}.")));
    assert!(!xpc(&format!("{LABEL}.x1")));
    assert!(!cgroup(
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/other.service\n"
    ));
    assert!(!cgroup(&format!(
        "0::/user.slice/user-1000.slice/session-2.scope/{LABEL}\n"
    )));
    assert!(!supervised_by(None, None));
}

#[test]
fn the_seam_arms_its_fault_once() {
    let first = Seam::parse("sync,escalate=/f/*/in/> stay escalate=/x bogus", false);
    assert!(first.sync);
    assert_eq!(
        first.escalate,
        vec!["/f/*/in/>".to_string(), "/x".to_string()]
    );
    assert_eq!(first.on_poison, OnPoison::Stay);
    assert_eq!(first.escalation().len(), 2);
    let again = Seam::parse("sync,escalate=/f/*/in/> stay", true);
    assert!(!again.sync, "a fired fault is not armed again");
    assert_eq!(again.escalate, vec!["/f/*/in/>".to_string()]);
    assert_eq!(again.on_poison, OnPoison::Stay);
    assert_eq!(Seam::parse("bogus syncs stays", false), Seam::default());
}

#[test]
fn the_armed_seam_poisons_a_relaxed_log_at_its_first_escalated_record() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("bhs")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let log = dir.path().join("b.log");
    let sock = dir.path().join("b.sock");
    let seam = Seam::parse("sync escalate=/e/>", false);
    let mut opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
    opts.escalation = astream_broker::Escalation::new(seam.escalation());
    let broker =
        astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    let served = broker.serve(&sock).expect("serve");
    let mut c = astream_broker::Client::connect(&sock).expect("connect");
    c.publish(1, 1, "/e/a", b"escalated, healthy")
        .expect("acked");
    assert_eq!(broker.poisoned(), None);
    seam.arm(&broker, &log);
    assert!(fired_marker(&log).exists(), "the marker is written");
    c.publish(1, 2, "/o/a", b"ordinary")
        .expect("a relaxed batch flushes nothing");
    assert_eq!(broker.poisoned(), None);
    let e = c
        .publish(1, 3, "/e/a", b"escalated")
        .expect_err("its full flush failed");
    assert!(e.to_string().contains("durability flush failed"), "{e}");
    let why = broker.poisoned().expect("the log is poisoned");
    assert!(!why.is_empty());
    let refused = c.publish(1, 4, "/o/b", b"after").expect_err("poisoned");
    assert!(
        astream_broker::broker_refusal(&refused).is_some_and(|r| r.is_poisoned()),
        "{refused}"
    );
    assert!(!Seam::parse("sync", fired_marker(&log).exists()).sync);
    drop(c);
    drop(served);
    assert_eq!(broker.poisoned().as_deref(), Some(why.as_str()));
}

#[test]
fn the_facts_name_what_astream_decided() {
    use astream_broker::{HealthWriter, ServedReason, Tier};
    let dir = aterm_tempfile::Builder::new()
        .prefix("bhf")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let log = dir.path().join("b.log");
    let sock = dir.path().join("b.sock");
    let seam = Seam::parse("escalate=/e/>", false);
    let mut opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
    opts.escalation = astream_broker::Escalation::new(seam.escalation());
    let broker =
        astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    broker
        .keep_health(HEALTH_EVERY)
        .expect("astream writes the sidecar at once");
    let served = broker.serve(&sock).expect("serve");
    let me = std::process::id();
    let mut c = astream_broker::Client::connect(&sock).expect("connect");
    c.publish(1, 1, "/o/a", b"before").expect("acked");
    let hello = |c: &mut astream_broker::Client<std::os::unix::net::UnixStream>| {
        c.hello().expect("hello");
        (
            c.broker_health()
                .cloned()
                .expect("this pin's broker sends entry 0x04"),
            c.served_durability().expect("and the tier"),
        )
    };
    let mut probe = astream_broker::Client::connect(&sock).expect("probe");

    let (healthy, tier) = hello(&mut probe);
    assert_eq!(healthy.poisoned, None);
    assert_eq!(healthy.flushes.failed, 0);
    assert_eq!(tier.served, Tier::Relaxed);
    assert!(in_band(&healthy).starts_with("ok, not poisoned — flushes since it opened: "));
    broker.write_health().expect("a snapshot after the publish");
    let read = astream_broker::read_health(&log)
        .expect("read")
        .expect("a sidecar");
    assert_eq!(read.writer, HealthWriter::Serving);
    assert_eq!(read.health.pid, me);
    assert_eq!(
        (
            read.health.retained_records,
            read.health.distinct_subjects,
            read.health.active_conns,
            read.health.poisoned.as_deref(),
        ),
        (1, 1, 2, None),
        "{read:?}"
    );
    assert!(is_current(&read, Some(me)));
    assert!(
        !is_current(&read, Some(me.wrapping_add(1))),
        "another pid serves the socket"
    );
    assert!(!is_current(&read, None), "no pid serves the socket");
    let row = snapshot_row(&read, Some(me), read.health.written_at_ms);
    assert!(
        row.starts_with("1 record(s) in ") && row.contains("2 connection(s)"),
        "{row}"
    );

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    c.publish(1, 2, "/e/a", b"escalated")
        .expect_err("its full flush failed");
    let (sick, tier) = hello(&mut probe);
    let why = sick.poisoned.as_deref().expect("poisoned, in band");
    assert!(why.contains("a flush failed"), "{why}");
    assert_eq!(sick.flushes.failed, 1);
    assert_eq!(
        (tier.served, tier.reason),
        (Tier::Strict, ServedReason::LatchedAfterFlushFailure)
    );
    let row = in_band(&sick);
    assert!(
        row.starts_with(&format!("{POISONED_ROW_WORD} (")) && row.contains(", 1 failed"),
        "{row}"
    );
    let read = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "astream's sidecar to name the poison",
        || {
            astream_broker::read_health(&log)
                .ok()
                .flatten()
                .filter(|r| r.health.poisoned.is_some())
        },
    );
    assert_eq!(read.health.served.served, Tier::Strict);
    assert_eq!(read.health.flushes.failed, 1);
    assert!(is_current(&read, Some(me)), "still served, poisoned");

    drop(c);
    drop(probe);
    drop(served);
    let read = astream_broker::read_health(&log)
        .expect("read")
        .expect("the last word stays");
    assert_eq!(read.writer, HealthWriter::Gone);
    assert!(
        !is_current(&read, Some(me)),
        "a gone writer is never current"
    );
    let row = snapshot_row(&read, Some(me), read.health.written_at_ms);
    assert!(
        row.starts_with(&format!(
            "- (astream's <log>.health is the last word of pid {me}, which no longer serves \
             the log, written 0s ago: "
        )) && row.contains(&format!("that broker's log was {POISONED_ROW_WORD} (")),
        "{row}"
    );
}

#[test]
fn a_snapshot_says_whose_word_it_is() {
    use astream_broker::{BrokerHealth, HealthRead, HealthWriter};
    assert_eq!(bytes(512), "512 B");
    assert_eq!(bytes(1024), "1.0 KiB");
    assert_eq!(bytes(3 * 1024 * 1024 + 100_000), "3.1 MiB");
    assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    let mut health = BrokerHealth::default();
    health.pid = 7;
    health.written_at_ms = 1_000;
    health.retained_records = 3;
    health.process_flushes = Some(astream_sys::FlushTally {
        full: 11,
        barrier: 13,
        plain: 2,
    });
    health.resident = Some(astream_broker::Residency {
        records: 2,
        bytes: 300,
    });
    health.process_flush_times = Some(astream_sys::FlushTimes {
        clock: astream_sys::FlushClock::Machine,
        full_ns: 41_000_000,
        barrier_ns: 2_600_000,
        plain_ns: 0,
        slow_count: 3,
        slow: vec![
            astream_sys::SlowFlush {
                index: 1,
                kind: astream_sys::FlushKind::Full,
                start_ns: 900_000_000,
                end_ns: 907_500_000,
            },
            astream_sys::SlowFlush {
                index: 2,
                kind: astream_sys::FlushKind::Barrier,
                start_ns: 910_000_000,
                end_ns: 911_250_000,
            },
        ],
        running: 0,
    });
    let read = |writer| HealthRead {
        health: health.clone(),
        writer,
    };
    assert!(is_current(&read(HealthWriter::Serving), Some(7)));
    assert!(!is_current(&read(HealthWriter::Serving), Some(8)));
    assert!(!is_current(&read(HealthWriter::Unknown), Some(7)));
    let unknown = snapshot_row(&read(HealthWriter::Unknown), Some(7), 61_000);
    assert!(unknown.starts_with("- "), "{unknown}");
    assert!(unknown.contains("pid 7"), "{unknown}");
    assert!(unknown.contains("resident=2/300 B of 3"), "{unknown}");
    assert!(
        unknown.contains("11 full, 13 barrier and 2 plain flush(es)"),
        "{unknown}"
    );
    assert!(
        unknown.contains(
            "; its flushes took 41.0 ms full, 2.6 ms barrier and 0.0 ms plain; 3 slower than \
             1 ms, 2 kept, the slowest a 7.5 ms full"
        ),
        "the broker's flush times and its ring: {unknown}"
    );
    // A ring that holds no flush (none slower than 1 ms yet) has no slowest to name.
    let quiet = flush_times(&astream_sys::FlushTimes {
        clock: astream_sys::FlushClock::Machine,
        full_ns: 400_000,
        barrier_ns: 0,
        plain_ns: 0,
        slow_count: 0,
        slow: Vec::new(),
        running: 0,
    });
    assert_eq!(
        quiet,
        "its flushes took 0.4 ms full, 0.0 ms barrier and 0.0 ms plain; 0 slower than 1 ms, \
         0 kept"
    );
    let other = snapshot_row(&read(HealthWriter::Serving), Some(8), 1_000);
    assert!(
        other.contains("is not the pid serving the socket"),
        "{other}"
    );
    assert!(!other.contains(POISONED_ROW_WORD), "{other}");
    // An older broker's snapshot: the same facts, its tally and its residency unknown,
    // never read as zero.
    health.process_flushes = None;
    health.resident = None;
    health.process_flush_times = None;
    let older = holds(&health);
    assert!(
        older.ends_with(&format!("{NO_PROCESS_FLUSHES}; {NO_FLUSH_TIMES}")),
        "{older}"
    );
    assert!(
        !older.contains(" full,") && !older.contains("ms full"),
        "{older}"
    );
    assert!(
        older.starts_with(&format!("3 record(s) in 0 B ({NO_RESIDENCY}), ")),
        "{older}"
    );
    assert!(!older.contains("resident="), "{older}");
}

#[test]
fn a_healthy_log_reads_unpoisoned_after_its_listeners_go() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("bhh")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let log = dir.path().join("b.log");
    let sock = dir.path().join("b.sock");
    let opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
    let broker =
        astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    let served = broker.serve(&sock).expect("serve");
    let mut c = astream_broker::Client::connect(&sock).expect("connect");
    c.publish(1, 1, "/o/a", b"ordinary").expect("acked");
    drop(c);
    drop(served);
    assert_eq!(broker.poisoned(), None);
}
