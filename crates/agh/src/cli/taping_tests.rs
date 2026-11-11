// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::super::args::{Parsed, parse};
use super::*;
use crate::fx::Mode;
use crate::wire::scripted::ScriptedTransport;

#[test]
fn begin_with_replays_the_tape_it_was_handed_and_reads_no_file() {
    let dir = crate::test_fixtures::scratch_dir("taping");
    let path = dir.path().join("handed.atape");
    let argv: Vec<String> = ["repo", "view", "alabsystems/aterm"]
        .map(String::from)
        .to_vec();
    let header = TapeHeader::for_run(&argv, &Endpoints::github_com());
    std::fs::write(&path, tape::render(&header, &[])).expect("the tape");
    let mut args = vec!["--replay".to_string(), path.display().to_string()];
    args.extend(argv.iter().cloned());
    let Ok(Parsed::Run(_, inv)) = parse(&args) else {
        panic!("{args:?} parses as a run");
    };
    let taped = read_taped(&inv.global).expect("the tape reads");
    std::fs::remove_file(&path).expect("the tape removed");
    let ctx = || {
        Ctx::for_tests(
            dir.path(),
            Arc::new(ScriptedTransport::new()),
            Endpoints::github_com(),
            MapEnv::new(),
        )
        .0
    };
    let mut handed = ctx();
    begin_with(&mut handed, &inv, &argv, Some(taped)).expect("replays what it was handed");
    assert_eq!(handed.fx.mode(), Mode::Replaying);
    assert!(handed.dirs.is_none());
    let mut unhanded = ctx();
    let refused = begin_with(&mut unhanded, &inv, &argv, None).expect_err("no tape on disk");
    assert_eq!(refused.sub, crate::error::Sub::Local);
}

#[cfg(unix)]
#[test]
fn end_prints_the_sinks_own_error_after_a_latched_refusal() {
    use std::os::unix::fs::PermissionsExt;

    use crate::wire::{Deadlines, Method, WireBody, WireRequest};

    let dir = crate::test_fixtures::scratch_dir("taping");
    let sealed = dir.path().join("write-only");
    std::fs::create_dir(&sealed).expect("the parent");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o300))
        .expect("write and search only");
    struct Readable<'a>(&'a std::path::Path);
    impl Drop for Readable<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _readable = Readable(&sealed);
    if std::fs::read_dir(&sealed).is_ok() {
        // Skip when running as root.
        return;
    }
    let path = sealed.join("new.atape");
    let e = Endpoints::github_com();
    let argv: Vec<String> = ["api", "/x"].map(String::from).to_vec();
    let (mut ctx, captured) = Ctx::for_tests(
        dir.path(),
        Arc::new(ScriptedTransport::new()),
        e.clone(),
        MapEnv::new(),
    );
    ctx.fx = Arc::new(Fx::record(&path, &TapeHeader::for_run(&argv, &e)).expect("a recording"));
    let mut req = WireRequest::new(
        Method::Post,
        "https://api.github.com/x",
        Vec::new(),
        1 << 20,
        Deadlines::JSON,
    );
    req.body = WireBody::Bytes(b"{}");
    let transport = ScriptedTransport::new();
    let refused = ctx.fx.gh(&transport, &e, req).expect_err("a body");
    assert!(matches!(refused.sub, Sub::Replay(_)), "{refused:?}");
    let before = captured.stderr().len();
    assert_eq!(end(&mut ctx, "api", 3, true), 3);
    let stderr = String::from_utf8(captured.stderr()).expect("UTF-8");
    assert!(stderr.len() > before, "end printed nothing");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(transport.requests().is_empty());
}
