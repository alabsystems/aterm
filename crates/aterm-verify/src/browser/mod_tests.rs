// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn immediate_timing_refusal_stops_the_actual_session_dispatch_before_browser_drive() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let built = Built {
        site: scratch.path().join("site"),
        work: scratch.path().join("work"),
        cost: None,
        scratch: scratch.path().to_owned(),
        ceiling: Some(cdp::HANG_DETECTOR),
        quiet: Some(aterm_capacity::Admit::Now),
        no_worker: Vec::new(),
    };
    // No real browser or driver is eligible to start in this control.
    let engine = Engine::Safari(safari::Safari {
        driver: scratch.path().join("no-browser-driver"),
    });
    let mut report = Report::new("timing refusal control");
    assert!(session(&mut report, &built, &engine, None).is_none());
    let tally = crate::ladder::tally(&[report]);
    assert_eq!(tally.could_not_run.len(), 2);
    assert!(tally.gate_failures.is_empty());
    assert!(tally.could_not_run[0].contains("--admit now"));
    assert!(tally.could_not_run[0].contains("started nothing"));
    assert!(
        desk_claim(None, "explicitly outer-owned")
            .unwrap()
            .is_none()
    );
}

/// A normal session retires its held resource. A worker deliberately blocked beyond a
/// zero ceiling must retain it until released, even though the outer call returned.
#[test]
fn a_timed_out_session_holds_its_desk_until_the_worker_finishes() {
    use std::sync::mpsc;
    struct Held(mpsc::Sender<()>);
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let (dropped, seen) = mpsc::channel();
    assert_eq!(
        bounded_session(Held(dropped), Some(cdp::HANG_DETECTOR), |_| 42),
        Ok(42)
    );
    seen.recv_timeout(cdp::HANG_DETECTOR).unwrap();

    let (dropped, seen) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let result = bounded_session(Held(dropped), Some(Duration::ZERO), move |_| {
        ready.send(()).unwrap();
        wait.recv_timeout(cdp::HANG_DETECTOR).unwrap();
    });
    assert_eq!(
        result,
        Err(crate::exec::Unfinished::TimedOut(Duration::ZERO))
    );
    started.recv_timeout(cdp::HANG_DETECTOR).unwrap();
    let held = seen
        .try_recv()
        .is_err_and(|e| matches!(e, mpsc::TryRecvError::Empty));
    release.send(()).unwrap();
    seen.recv_timeout(cdp::HANG_DETECTOR).unwrap();
    assert!(
        held,
        "the timed-out worker must still own its desk while blocked"
    );
}

/// `readback_split` sums each step and `timing_line` reports untimed overhead.
#[test]
fn a_page_s_timings_split_its_readbacks_from_its_frames() {
    let t = json!({
        "frame_ms": [1.0, 2.0, 1.0, 1.0],
        "finish_ms": [3.0, 0.0, 0.0, 0.0, 0.0],
        "copy_ms": [0.5, 0.5, 0.5, 0.5, 1.0],
        "read_ms": [1.0, 1.0, 1.0, 1.0, 9.0],
        "encode_ms": [0.25, 0.25, 0.25, 0.25, 1.0],
        "frame_wait_ms": 10.0,
        "total_ms": 43.0,
    });
    assert_eq!(timed(&t, "read_ms"), Some(13.0));
    assert_eq!(timed(&t, "frame_wait_ms"), Some(10.0));
    assert_eq!(timed(&t, "probe_ms"), None);
    let split = readback_split(&t).expect("split");
    assert!(
        split.contains(
            "finish 3.0 ms (most 3.0 on parity-0), copy 3.0 ms, getImageData 13.0 ms (most \
             9.0 on diff-0)"
        ),
        "{split}"
    );
    let page = Page {
        answer: judge::Answer {
            module: "m".to_string(),
            adapter: "a".to_string(),
            user_agent: String::new(),
            frames: Vec::new(),
            diff: Vec::new(),
            readback: None,
            timings: t,
            init: None,
            drawn: None,
            glow: Vec::new(),
        },
        load_ms: None,
        evaluate_ms: 0.0,
    };
    assert!(
        timing_line("gpu", &page).ends_with("7.0 in no timed step)"),
        "{}",
        timing_line("gpu", &page)
    );
    assert!(readback_split(&json!({})).is_none());
}

/// `site_lacks` and `drive_main` reject incomplete sites and invalid CLI arguments.
#[test]
fn a_drive_needs_a_laid_site_and_named_sessions() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let site = dir.path().join("site");
    std::fs::create_dir_all(&site).expect("site");
    assert_eq!(site_lacks(&site), Some("cpu.html"));
    for f in SITE_FILES {
        if f == "cpu" || f == "gpu" || f == recorded::IN_SITE {
            std::fs::create_dir_all(site.join(f)).expect("glue or recording dir");
        } else {
            std::fs::write(site.join(f), "x").expect("file");
        }
    }
    assert_eq!(site_lacks(&site), None);
    std::fs::remove_dir_all(site.join("gpu")).expect("rm gpu");
    assert_eq!(site_lacks(&site), Some("gpu"));
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let site_arg = site.display().to_string();
    assert_eq!(drive_main(&s(&[&site_arg])), exit::USAGE);
    assert_eq!(
        drive_main(&s(&[&site_arg, "/o", "--engine", "opera"])),
        exit::USAGE
    );
    assert_eq!(drive_main(&s(&[&site_arg, "/o", "--fast"])), exit::USAGE);
    for engine in ["firefox", "webkit", "safari"] {
        assert_eq!(
            drive_main(&s(&[&site_arg, "/o", "--engine", engine])),
            exit::COULD_NOT_RUN,
            "{engine}: a site without its gpu glue"
        );
    }
    assert_eq!(site_main(&s(&[]), dir.path()), exit::USAGE);
    assert_eq!(
        drive_main(&s(&[
            &site_arg, "/o", "--engine", "safari", "--admit", "now"
        ])),
        exit::COULD_NOT_RUN,
        "--admit is browser-drive's option"
    );
    assert_eq!(
        drive_main(&s(&[&site_arg, "/o", "--admit", "soon"])),
        exit::USAGE
    );
}

/// `--admit` reaches the quiet claim of a session that takes the desk, wait by default, and
/// an unknown option or an `--admit` without `wait` or `now` still refuses.
#[test]
fn browser_drive_s_admit_reaches_its_desk_claim() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let quiet = |v: &[&str]| drive_args(&s(v)).map(|a| a.built(PathBuf::new()).quiet);
    assert_eq!(
        quiet(&["site", "out", "--engine", "safari"]),
        Ok(Some(aterm_capacity::Admit::Wait))
    );
    for now in [&["--admit", "now"][..], &["--admit=now"]] {
        let args = [&["site", "out", "--engine", "safari"][..], now].concat();
        assert_eq!(
            quiet(&args),
            Ok(Some(aterm_capacity::Admit::Now)),
            "{now:?}"
        );
    }
    assert_eq!(
        quiet(&["--admit", "now", "site", "--admit", "wait", "out"]),
        Ok(Some(aterm_capacity::Admit::Wait)),
        "the last --admit"
    );
    let asked = drive_args(&s(&["s", "o", "--engine=safari", "--engine", "safari"]));
    assert_eq!(asked.map(|a| a.wanted), Ok(vec!["safari".to_string()]));
    for refused in [
        &["s", "o", "--fast"][..],
        &["s", "o", "--admit"],
        &["s", "o", "--admit", "soon"],
        &["s", "o", "--admit=", "now"],
        &["s", "o", "--admit", "--engine", "safari"],
        &["s", "--admit", "now"],
    ] {
        assert!(drive_args(&s(refused)).is_err(), "{refused:?}");
    }
}

/// A posted answer parses, and a page's error is a finding naming it, unless the page marks
/// it the browser's lack, which the engine judges ([`Engine::worker_stop`]).
#[test]
fn a_posted_answer_or_error_is_read_back() {
    let ok = br#"{"answer": {"module": "aterm-wasm", "adapter": "cpu", "frames": []}}"#;
    assert!(matches!(posted("cpu", ok), Ok(a) if a.module == "aterm-wasm"));
    let failed = br#"{"error": "init_offscreen(canvas): no WebGL2"}"#;
    assert!(matches!(
        posted("worker", failed),
        Err(Stop::Fail(why)) if why == "the worker page: init_offscreen(canvas): no WebGL2"
    ));
    let lacks = br#"{"error": "the worker: no WebGL2 context", "unsupported": true}"#;
    assert!(matches!(
        posted("worker", lacks),
        Err(Stop::Lacks(why)) if why == "the worker page: the worker: no WebGL2 context"
    ));
    let not_marked = br#"{"error": "the worker: no WebGL2 context", "unsupported": "yes"}"#;
    assert!(
        matches!(posted("worker", not_marked), Err(Stop::Fail(_))),
        "only `unsupported: true` makes an error the browser's"
    );
    assert!(matches!(posted("gpu", b"{}"), Err(Stop::Fail(why)) if why.contains("no answer")));
    assert!(matches!(posted("gpu", b"<html>"), Err(Stop::Fail(why)) if why.contains("not JSON")));
}

/// A worker that posts `unsupported` fails its row in every engine whose worker WebGL2 the
/// lane has measured: Chromium's, Firefox's (on Linux too), Safari's and macOS's WebKit. Only
/// WPE on Linux cannot run it. Any other stop stays as it is.
#[test]
fn only_wpe_on_linux_lacking_worker_webgl2_cannot_run_its_row() {
    let lacks = || {
        let body = br#"{"error": "the worker: no WebGL2 context", "unsupported": true}"#;
        posted("worker", body).expect_err("an unsupported post is a stop")
    };
    let at = PathBuf::from("/absent");
    let measured = [
        Engine::Chromium(
            chromium::Browser {
                path: at.clone(),
                shell: true,
                source: String::new(),
            },
            chromium::Angle::SwiftShader,
        ),
        Engine::Firefox(firefox::Firefox {
            path: at.clone(),
            source: String::new(),
        }),
        Engine::Safari(safari::Safari { driver: at.clone() }),
    ];
    let wpe = Engine::WebKit(webkit::WebKit {
        path: at,
        source: String::new(),
    });
    for os in ["linux", "macos"] {
        for engine in &measured {
            assert!(
                matches!(engine.worker_stop(os, &Vec::new, lacks()), Stop::Fail(_)),
                "{} on {os}",
                engine.name()
            );
        }
    }
    assert!(matches!(
        wpe.worker_stop("macos", &Vec::new, lacks()),
        Stop::Fail(_)
    ));
    assert!(matches!(
        wpe.worker_stop("linux", &Vec::new, lacks()),
        Stop::CouldNotRun(why) if why.starts_with("the worker page: the worker: no WebGL2")
    ));
    assert!(matches!(
        measured[1].worker_stop("linux", &Vec::new, Stop::CouldNotRun("gone".into())),
        Stop::CouldNotRun(why) if why == "gone"
    ));
}

/// `--no-worker` names a session whose worker row the run leaves out, whose browser then
/// opens no worker page; the page chain is the cpu page's `next`.
#[test]
fn no_worker_leaves_the_worker_page_out_of_the_chain() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let asked = drive_args(&s(&[
        "s",
        "o",
        "--engine",
        "webkit",
        "--no-worker=webkit",
        "--no-worker",
        "webkit",
    ]))
    .expect("parses");
    assert_eq!(asked.wanted, ["webkit"]);
    assert_eq!(asked.built(PathBuf::new()).no_worker, ["webkit"]);
    assert!(drive_args(&s(&["s", "o", "--no-worker", "wpe"])).is_err());
    assert!(drive_args(&s(&["s", "o", "--no-worker"])).is_err());
    assert_eq!(
        first_page("http://127.0.0.1:9", true),
        "http://127.0.0.1:9/cpu.html?post=1&next=gpu,worker"
    );
    assert_eq!(
        first_page("http://127.0.0.1:9", false),
        "http://127.0.0.1:9/cpu.html?post=1&next=gpu"
    );
}
