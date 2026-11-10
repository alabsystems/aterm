// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::codex_renewal;

fn listening_peer(opts: &Opts) -> std::thread::JoinHandle<usize> {
    let listener = std::os::unix::net::UnixListener::bind(opts.sock.as_deref().unwrap()).unwrap();
    std::thread::spawn(move || {
        let mut connections = 0;
        loop {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            stream
                .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            let mut line = String::new();
            let bytes = std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            if bytes == 0 || line == "stop fixture\n" {
                return connections;
            }
            connections += 1;
            let _closed = stream.write_all(b"ERR fixture refuses unexpected connection\n");
        }
    })
}

fn prepared(st: &St) -> Box<Prepare> {
    let Request::Prepare(prepare) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("canonical preparation")
    };
    prepare
}

fn corrupt(st: &mut St, case: &str) {
    match case {
        "absent" => st.native_prepare.clear(),
        "malformed" => st.native_prepare = "retained but unreadable debt".into(),
        "other-request" => {
            st.native_prepare = Request::Query(st.native.clone().unwrap()).encode().unwrap();
        }
        "noncanonical" => st.native_prepare.insert(0, ' '),
        "binding" => st.native.as_mut().unwrap().id = "ff".repeat(16),
        "deadline" | "human" | "id" => {
            let mut prepare = prepared(st);
            match case {
                "deadline" => prepare.deadline_us += 1,
                "human" => prepare.expected_human_seq = Some(1),
                "id" => prepare.id = "fe".repeat(16),
                _ => unreachable!(),
            }
            st.native_prepare = Request::Prepare(prepare).encode().unwrap();
        }
        _ => unreachable!(),
    }
}

#[test]
fn invalid_caller_preparation_never_becomes_a_record_path() {
    let seed = aterm_tempfile::tempdir().unwrap();
    let st = bound_record(&opts(seed.path()));
    for case in ["id", "epoch"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let mut prepare = prepared(&st);
        if case == "id" {
            prepare.id = "../../unrelated".into();
        } else {
            prepare.host_epoch = "invalid-epoch".into();
        }
        assert!(Request::Prepare(prepare.clone()).encode().is_none());
        assert_eq!(
            codex_renewal::prepare_restart(&opts, SID, &prepare),
            Err("restart-plan")
        );
        codex_renewal::reconcile_restart(&opts, SID, &prepare);
        assert!(codex_renewal::drive_restart(&opts, SID, &prepare).is_none());
        assert!(!opts.state.exists());
    }
}

#[test]
fn renewal_entry_points_reject_a_different_retained_recipe_without_rewriting_it() {
    for case in [
        "absent",
        "malformed",
        "other-request",
        "noncanonical",
        "binding",
        "deadline",
        "human",
        "id",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let mut st = bound_record(&opts);
        let expected = prepared(&st);
        let key = format!("renewal-{}", expected.id);
        corrupt(&mut st, case);
        upgrade_drive::save_checked(&opts, &key, &st).unwrap();
        let path = opts.state.join("upgrade").join(format!("{key}.json"));
        let before = std::fs::read(&path).unwrap();
        let listener = listening_peer(&opts);
        // Prove this endpoint accepts a connection; absence of a server must
        // not masquerade as a caller's refusal to connect.
        let mut positive = UnixStream::connect(opts.sock.as_deref().unwrap()).unwrap();
        positive.write_all(b"positive control\n").unwrap();
        assert_eq!(
            codex_renewal::prepare_restart(&opts, SID, &expected),
            Err("restart-plan"),
            "{case}: the recipe is checked before connecting"
        );
        codex_renewal::reconcile_restart(&opts, SID, &expected);
        assert!(codex_renewal::drive_restart(&opts, SID, &expected).is_none());
        assert!(codex_renewal::drive_resume_only(&opts, SID, &expected).is_none());
        let (client, worker) = endpoint(|_| panic!("invalid renewal reached the wire"));
        assert!(codex_renewal::drive_connection(&opts, SID, &expected, st, client, true).is_none());
        assert!(worker.join().unwrap().is_empty(), "{case}");
        // Keep both client ends open until the server has read them: macOS
        // refuses timeout setup on an accepted socket whose peer already closed.
        let mut stop = UnixStream::connect(opts.sock.as_deref().unwrap()).unwrap();
        stop.write_all(b"stop fixture\n").unwrap();
        assert_eq!(
            listener.join().unwrap(),
            1,
            "{case}: only the positive control connected"
        );
        drop((positive, stop));
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "{case}: debt survives"
        );
    }
}

#[test]
fn malformed_binding_pairs_cannot_consume_terminal_observations() {
    for case in [
        "absent",
        "malformed",
        "other-request",
        "noncanonical",
        "binding",
        "id",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let mut st = bound_record(&opts);
        corrupt(&mut st, case);
        let original = st.clone();
        let (mut client, worker) = endpoint(|_| panic!("invalid pair queried a terminal reply"));
        assert!(!complete(&opts, "conversation", &mut client, SID, &mut st));
        assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
        assert_eq!(
            launch_unissued(&mut client, SID, &mut st),
            Err("native-plan")
        );
        assert_eq!(
            resume_observed(&mut client, SID, &mut st),
            Err("native-plan")
        );
        assert_eq!(
            refresh_retirement_hold(&mut client, SID, &mut st),
            Err("native-plan")
        );
        assert_eq!(st, original, "{case}: no in-memory debt disappears");
        drop(client);
        assert!(worker.join().unwrap().is_empty());
    }
}

#[test]
fn matching_pair_can_still_observe_completion_or_retirement() {
    for retirement in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let mut st = bound_record(&opts);
        let binding = st.native.clone().unwrap();
        let kept = prepared(&st);
        let (mut client, worker) = endpoint(move |line| {
            let seen = request(line);
            if retirement && seen == Request::Prepare(kept.clone()) {
                // Retirement replays the kept prepare first; its admission keeps the binding.
                return Some(format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    binding.id, binding.capability
                ));
            }
            assert_eq!(
                seen,
                if retirement {
                    Request::Reconcile(binding.clone())
                } else {
                    Request::Query(binding.clone())
                }
            );
            Some(if retirement {
                "OK native-act state=retired reason=no-exec-grant\n".into()
            } else {
                "OK native-act state=committed claim=builtin revision=-\n".into()
            })
        });
        if retirement {
            assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
            assert!(st.native_retired_unissued);
            assert!(st.native.is_some() && !st.native_prepare.is_empty());
        } else {
            assert!(complete(&opts, "conversation", &mut client, SID, &mut st));
            assert!(st.native.is_none() && st.native_prepare.is_empty());
        }
        drop(client);
        assert_eq!(
            worker.join().unwrap().len(),
            1 + usize::from(retirement),
            "retirement reconciles after its replay"
        );
    }
}

#[test]
fn renewal_signal_requires_this_attempts_barrier_after_reload() {
    let home = aterm_tempfile::tempdir().unwrap();
    let opts = opts(home.path());
    let st = bound_record(&opts);
    let prepare = prepared(&st);
    let key = format!("renewal-{}", prepare.id);
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::save_checked(&opts, &key, &st).is_err());
    let loaded = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    let expected = Request::Signal(loaded.native.clone().unwrap());
    let mut dry = opts.clone();
    dry.dry_run = true;
    for current_opts in [&dry, &opts] {
        let (client, worker) = endpoint(|_| panic!("unpublished signal reached the wire"));
        assert!(
            codex_renewal::drive_connection(
                current_opts,
                SID,
                &prepare,
                loaded.clone(),
                client,
                true
            )
            .is_none()
        );
        assert!(worker.join().unwrap().is_empty());
    }
    drop(fault);
    // The refused cloned writer still renamed its own generation. Its older
    // sibling cannot acknowledge that write; a fresh reader gets the current one.
    let loaded = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    let (client, worker) = endpoint(move |line| {
        assert_eq!(request(line), expected);
        None
    });
    assert!(codex_renewal::drive_connection(&opts, SID, &prepare, loaded, client, true).is_none());
    assert_eq!(
        worker.join().unwrap().len(),
        1,
        "the repaired barrier admits the exact signal"
    );
}

#[test]
fn renewal_signal_cannot_overwrite_a_newer_record() {
    let home = aterm_tempfile::tempdir().unwrap();
    let opts = opts(home.path());
    let st = bound_record(&opts);
    let prepare = prepared(&st);
    let key = format!("renewal-{}", prepare.id);
    upgrade_drive::save_checked(&opts, &key, &st).unwrap();
    let stale = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    let mut current = upgrade_drive::load_record(&opts, &key).unwrap().unwrap();
    current.wait = "newer renewal writer".into();
    upgrade_drive::save_checked(&opts, &key, &current).unwrap();
    let (client, worker) = endpoint(|_| panic!("stale signal reached the wire"));
    assert!(codex_renewal::drive_connection(&opts, SID, &prepare, stale, client, true).is_none());
    assert!(worker.join().unwrap().is_empty());
    assert_eq!(
        upgrade_drive::load_record(&opts, &key)
            .unwrap()
            .unwrap()
            .wait,
        current.wait
    );
}

#[test]
fn renewal_signal_preserves_both_supported_legacy_cold_routes() {
    for stored in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let mut st = bound_record(&opts);
        let (old_request, legacy) = super::legacy::old_prepare(&st, stored);
        let Request::Prepare(prepare) = old_request else {
            unreachable!()
        };
        st.native_prepare = legacy.clone();
        let expected = Request::Signal(st.native.clone().unwrap());
        let (client, worker) = endpoint(move |line| {
            assert_eq!(request(line), expected);
            None
        });
        assert!(codex_renewal::drive_connection(&opts, SID, &prepare, st, client, true).is_none());
        assert_eq!(worker.join().unwrap().len(), 1);
        let saved = upgrade_drive::load_record(&opts, &format!("renewal-{}", prepare.id))
            .unwrap()
            .unwrap();
        assert_eq!(saved.native_prepare, legacy);
    }
}
