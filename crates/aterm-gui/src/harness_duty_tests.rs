// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn endpoint(statuses: Vec<&'static str>) -> (String, JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader};
    let dir = crate::test_scratch::short_dir("private duty adapter fake peer");
    let path = dir.path().join("a.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let socket = path.to_string_lossy().into_owned();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut replies = statuses.into_iter();
        let mut seen = Vec::new();
        for line in BufReader::new(stream).lines() {
            let line = line.unwrap();
            if line.starts_with("AUTH ") {
                continue;
            }
            let reply = if line == "@s-duty status" {
                replies.next().expect("one planned authority query")
            } else {
                "OK"
            };
            writer.write_all(format!("{reply}\n").as_bytes()).unwrap();
            seen.push(line);
        }
        assert!(
            replies.next().is_none(),
            "all planned observations happened"
        );
        drop(dir);
        seen
    });
    (socket, server)
}

#[test]
#[cfg(unix)]
fn actual_private_adapter_fences_kept_input_and_never_relabels_stale_or_owned_requests() {
    let status = "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=41";
    let (socket, server) = endpoint(vec![
        status,
        status,
        "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=42",
        "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=approvals supervisor_revision=43",
    ]);
    let mut ctl = HostCtl::with_token(
        Endpoint::Socket(socket),
        Some("private-test-token".to_string()),
    );
    ctl.prefer = true;
    ctl.duty_sid = Some("s-duty".to_string());
    assert_eq!(ctl.builtin_authority("@s-duty").revision(), Some(41));
    assert!(
        ctl.call_for_duty(
            Scope::Approvals,
            41,
            &["@s-duty", "key", "if=permission", "1"]
        )
        .unwrap()
        .unwrap()
        .ok()
    );
    assert!(
        !ctl.call_for_duty(Scope::Approvals, 41, &["@s-duty", "key", "enter"])
            .unwrap()
            .unwrap()
            .ok()
    );
    assert!(
        !ctl.call_for_duty(Scope::Approvals, 43, &["@s-duty", "key", "1"])
            .unwrap()
            .unwrap()
            .ok()
    );
    assert!(
        !ctl.call(&["@s-duty", "send", "as=person", "x"])
            .unwrap()
            .ok()
    );
    drop(ctl);
    let seen = server.join().unwrap();
    let writes: Vec<_> = seen
        .iter()
        .filter(|word| !word.ends_with(" status"))
        .collect();
    assert_eq!(
        writes,
        [&format!(
            "@s-duty key if-supervisor={} if-supervisor-revision=41 harness-duty=approvals if=permission 1",
            holder()
        )]
    );
}

#[test]
fn v2_watch_only_and_foreign_scoped_native_standdown_are_preserved() {
    let mut ctl = HostCtl::new(Endpoint::Socket("unused-private-socket".to_string()));
    ctl.duty_sid = Some("s-duty".to_string());
    ctl.watch_only = Some("codex-alab@42".to_string());
    assert!(
        !ctl.call_for_duty(Scope::Approvals, 41, &["@s-duty", "key", "1"])
            .unwrap()
            .unwrap()
            .ok()
    );
    assert!(foreign_scoped_coordination(
        "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=41"
    ));
    assert!(!foreign_scoped_coordination(
        "OK supervisor=- supervisor_contract=- supervisor_scope=- supervisor_revision=42"
    ));
}
