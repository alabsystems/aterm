// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn minted_cap_output_requires_a_complete_write_and_flush() {
    let line = astream_cap::capfile::format_line("n-synthetic:r:/f/test", &[42; 32]);
    let mut ordinary = Vec::new();
    assert_eq!(minted_cap_to(&mut ordinary, &line), ExitCode::SUCCESS);
    assert_eq!(ordinary, format!("{line}\n").as_bytes());
    struct Refuses {
        bytes: Vec<u8>,
        accept: usize,
        kind: std::io::ErrorKind,
        flush: bool,
        zero: bool,
    }
    impl std::io::Write for Refuses {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.zero {
                return Ok(0);
            }
            let n = bytes
                .len()
                .min(self.accept.saturating_sub(self.bytes.len()));
            if n == 0 {
                return Err(self.kind.into());
            }
            self.bytes.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.flush {
                Err(self.kind.into())
            } else {
                Ok(())
            }
        }
    }
    let mut admitted = Vec::new();
    for (accept, kind, flush, zero) in [
        (0, std::io::ErrorKind::StorageFull, false, false),
        (3, std::io::ErrorKind::StorageFull, false, false),
        (0, std::io::ErrorKind::BrokenPipe, false, false),
        (usize::MAX, std::io::ErrorKind::StorageFull, true, false),
        (0, std::io::ErrorKind::WriteZero, false, true),
    ] {
        let mut out = Refuses {
            bytes: Vec::new(),
            accept,
            kind,
            flush,
            zero,
        };
        admitted.push(minted_cap_to(&mut out, &line) == ExitCode::FAILURE);
    }
    assert_eq!(
        admitted,
        vec![true; 5],
        "no partial, lost or unflushed capability result is success"
    );
}

fn argv(line: &str) -> Vec<String> {
    line.split(' ').map(str::to_string).collect()
}

#[test]
fn a_broker_ends_itself_only_when_its_launcher_or_its_path_is_gone() {
    use aterm_uds::lifeline::Cut;
    use aterm_uds::listener::Named;
    for path in [Some(Named::Same), None] {
        assert!(
            ends_itself(None, path).is_none(),
            "a live launcher, {path:?}: it serves on"
        );
    }
    for path in [Some(Named::Same), Some(Named::Nothing), None] {
        assert!(
            matches!(
                ends_itself(Some(Cut::Closed), path),
                Some(SelfEnd::LauncherGone(Cut::Closed))
            ),
            "a cut lifeline beside {path:?}"
        );
    }
    let failed = ends_itself(Some(Cut::Failed(std::io::Error::other("EIO"))), None);
    assert!(
        matches!(&failed, Some(SelfEnd::LauncherGone(Cut::Failed(_)))),
        "{failed:?}"
    );
    assert!(matches!(
        ends_itself(None, Some(Named::Nothing)),
        Some(SelfEnd::PathGone)
    ));
    assert!(matches!(
        ends_itself(None, Some(Named::Another)),
        Some(SelfEnd::PathTaken)
    ));
    let line = SelfEnd::LauncherGone(Cut::Closed).words("/r/bus.sock");
    assert!(line.starts_with(LIFELINE_CUT_WORDS), "{line}");
    let line = SelfEnd::PathGone.words("/r/bus.sock");
    assert!(
        line.starts_with(&format!("/r/bus.sock {PATH_GONE_WORDS}")),
        "{line}"
    );
}

#[test]
fn a_path_gone_before_the_first_look_ends_the_broker() {
    use aterm_uds::listener::Named;
    let gone = "/nonexistent-aterm-link-dir/b.sock";
    let unbound = path_named(gone, Some(false));
    assert!(
        matches!(ends_itself(None, unbound), Some(SelfEnd::PathGone)),
        "a path gone before the first look: {unbound:?}"
    );
    let unread = path_named(gone, None);
    assert!(
        ends_itself(None, unread).is_none(),
        "a listener whose identity astream could not read: {unread:?}"
    );
    assert!(
        ends_itself(None, Some(Named::Unread)).is_none(),
        "a wake's read that failed"
    );
}

/// The broker's path names its own socket once `serve` returned, through a symlink to
/// it too, as a client's connect reaches it, so the broker serves on; a socket renamed
/// over the path, as one renamed in just after `serve` returned is at the broker's
/// first look, is another socket, so the broker ends itself; and once nothing is
/// there, or a file that is no socket, its path is gone.
#[cfg(unix)]
#[test]
fn a_socket_renamed_over_the_path_as_the_broker_starts_ends_it() {
    use aterm_uds::listener::Named;
    let scratch = crate::test_scratch_dir("cli");
    let sock = scratch.path().join("b.sock");
    let theirs = scratch.path().join("t.sock");
    let held = scratch.path().join("held.sock");
    let broker = crate::test_broker(scratch.path().join("b.log")).expect("broker");
    let handle = broker.serve(&sock).expect("serve");
    let endpoint = sock.to_str().expect("a UTF-8 path");
    let named = path_named(endpoint, handle.names_listener());
    assert_eq!(named, Some(Named::Same), "its own socket");
    assert!(ends_itself(None, named).is_none(), "its own socket");
    std::fs::hard_link(&sock, &held).expect("held");
    std::fs::remove_file(&sock).expect("unlink");
    std::os::unix::fs::symlink(&held, &sock).expect("symlink");
    let named = path_named(endpoint, handle.names_listener());
    assert_eq!(named, Some(Named::Same), "a symlink to its own socket");
    std::fs::remove_file(&sock).expect("unlink the symlink");
    let _theirs = std::os::unix::net::UnixListener::bind(&theirs).expect("bind");
    std::fs::rename(&theirs, &sock).expect("rename over");
    let named = path_named(endpoint, handle.names_listener());
    assert!(
        matches!(ends_itself(None, named), Some(SelfEnd::PathTaken)),
        "another socket: {named:?}"
    );
    std::fs::remove_file(&sock).expect("unlink");
    let named = path_named(endpoint, handle.names_listener());
    assert!(
        matches!(ends_itself(None, named), Some(SelfEnd::PathGone)),
        "nothing there: {named:?}"
    );
    std::fs::write(&sock, "").expect("a plain file in its place");
    let named = path_named(endpoint, handle.names_listener());
    assert!(
        matches!(ends_itself(None, named), Some(SelfEnd::PathGone)),
        "a file that is no socket: {named:?}"
    );
}

#[test]
fn the_restart_a_person_runs_carries_no_lifeline() {
    let armed = argv("aterm link broker s l --lifeline-fd 0 --durability strict");
    assert_eq!(
        by_hand(armed, 6, Some(2)),
        argv("aterm link broker s l --durability strict")
    );
    let bare = argv("aterm link broker s l");
    assert_eq!(by_hand(bare.clone(), 2, None), bare);
    assert_eq!(by_hand(bare.clone(), 2, Some(0)), bare, "no flag there");
}

#[test]
fn a_broker_exits_only_for_a_settled_replacement_of_its_executable() {
    let scratch = crate::test_scratch_dir("exe");
    let dir = scratch.path();
    let exe = dir.join("aterm");
    std::fs::write(&exe, b"build one").expect("write");
    let start = FileId::of(&exe).expect("start");
    let image = RunningImage::File(start);
    let same = FileId::of(&exe);
    assert!(!replaced(&image, None, &same), "the same file");
    assert!(
        !replaced(&image, same.as_ref().ok(), &same),
        "the same file, looked at twice"
    );
    std::fs::write(dir.join("aterm.new"), b"build two").expect("write");
    std::fs::rename(dir.join("aterm.new"), &exe).expect("rename over");
    let first = FileId::of(&exe);
    assert!(
        !replaced(&image, Some(&start), &first),
        "seen once: it may still be being written"
    );
    let second = FileId::of(&exe);
    assert!(replaced(&image, first.as_ref().ok(), &second), "settled");
    let rewritten = FileId::of(&exe).expect("now");
    std::fs::write(&exe, b"build three, longer").expect("rewrite in place");
    let again = FileId::of(&exe);
    assert!(replaced(
        &RunningImage::File(rewritten),
        again.as_ref().ok(),
        &FileId::of(&exe)
    ));
    std::fs::remove_file(&exe).expect("remove");
    let gone = FileId::of(&exe);
    assert!(gone.is_err());
    assert!(!replaced(&image, second.as_ref().ok(), &gone));
    std::fs::create_dir(&exe).expect("a directory where the file was");
    assert!(FileId::of(&exe).is_err(), "not a regular file");
}

#[test]
fn an_executable_watch_follows_the_invocation_symlink_through_a_swap() {
    use std::ffi::OsStr;
    use std::os::unix::fs::{symlink, PermissionsExt};

    let scratch = crate::test_scratch_dir("cli");
    let dir = scratch.path();
    let old = dir.join("build-one");
    let new = dir.join("build-two");
    for file in [&old, &new] {
        std::fs::write(file, b"executable fixture").expect("write");
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).expect("executable");
    }
    let invocation = dir.join("aterm");
    symlink(&old, &invocation).expect("invocation symlink");
    let resolved = invocation_path(OsStr::new("aterm"), Some(dir.as_os_str()))
        .expect("bare argv0 resolved on PATH");
    assert_eq!(resolved, invocation, "retain the final symlink");
    assert_eq!(
        invocation_path(invocation.as_os_str(), None),
        Some(invocation.clone())
    );
    assert!(invocation_path(OsStr::new("aterm"), None).is_none());
    let original = FileId::of(&old).expect("running image");
    let mut watch = ExeWatch {
        path: resolved,
        start: RunningImage::File(original),
        seen: None,
    };
    assert!(!watch.poll());
    assert!(!watch.poll(), "an unchanged invocation keeps serving");

    symlink(&new, dir.join("aterm.next")).expect("new symlink");
    std::fs::rename(dir.join("aterm.next"), &invocation).expect("retarget atomically");
    assert!(!watch.poll(), "one observation is not settled");
    std::fs::remove_file(&invocation).expect("swap gap");
    assert!(
        !watch.poll(),
        "a missing invocation never requests a restart"
    );
    symlink(&new, &invocation).expect("finish swap");
    assert!(!watch.poll(), "the gap resets the settlement observation");
    assert!(watch.poll(), "restart onto the new symlink target");
    assert_eq!(FileId::of(&old).expect("old image untouched"), original);
}

#[test]
fn an_executable_watch_keeps_its_running_identity_across_readiness() {
    let scratch = crate::test_scratch_dir("cli");
    let invocation = scratch.path().join("aterm");
    std::fs::write(&invocation, b"running image").expect("old image");
    let original = FileId::of(&invocation).expect("image at startup");
    let mut watch = ExeWatch {
        path: invocation.clone(),
        start: RunningImage::File(original),
        seen: None,
    };

    let replacement = scratch.path().join("aterm.next");
    std::fs::write(&replacement, b"replacement image").expect("replacement");
    std::fs::rename(&replacement, &invocation).expect("replace before readiness");
    let after_ready = FileId::of(&invocation).expect("file after readiness");
    assert!(!watch.poll());
    assert!(watch.poll(), "the replacement cannot become the baseline");
    assert!(
        !replaced(
            &RunningImage::File(after_ready),
            Some(&after_ready),
            &Ok(after_ready)
        ),
        "negative control: a baseline captured after readiness misses the update"
    );

    let mut already_replaced = ExeWatch {
        path: invocation,
        start: RunningImage::capture().expect("this process's mapped executable"),
        seen: None,
    };
    assert!(!already_replaced.poll());
    assert!(already_replaced.poll());
}

#[test]
fn ls_prints_section_7s_columns_one_safe_token_each() {
    let (body, _) = crate::body::Body::decode(
        b"v=1 t=1 inc=4 epoch=ab12 gen=9:beef state=live hold=0 holder=h-andrew \
          attention=needs%20a%20key",
    );
    let (line, escalating) = row("/f/f1/pub/n-a/s-w/presence", &body);
    assert!(escalating, "a row carrying an attention IS an escalation");
    let cols: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(cols[0], "n-a", "{line}");
    assert_eq!(
        cols[1], "-",
        "§7 puts <host> positionally, and no session row has one: {line}"
    );
    assert_eq!(cols[2], "s-w", "{line}");
    let names: Vec<&str> = cols[3..]
        .iter()
        .map(|c| c.split('=').next().unwrap_or(c))
        .collect();
    assert_eq!(
        names,
        [
            "state",
            "inc",
            "role",
            "detail",
            "driving",
            "holder",
            "hold",
            "fabric",
            "attention",
            "gen",
            "observer",
            "epoch",
            "phase",
            "context",
            "title",
            "proto",
            "aterm"
        ],
        "the row must be §7's columns in §7's order, then the additions: {line}"
    );
    assert!(
        line.contains(" attention=needs%20a%20key ") && line.contains(" gen=9:beef "),
        "{line}"
    );
    for absent in [
        "role=-",
        "detail=-",
        "driving=-",
        "fabric=-",
        "phase=-",
        "context=-",
        "title=-",
        "proto=-",
        "aterm=-",
    ] {
        assert!(line.contains(absent), "{absent} missing from: {line}");
    }
    let (node_body, _) = crate::body::Body::decode(
        b"v=1 t=1 state=live inc=2 fabric=connected host=m7 pid=1 proto=2 aterm=0.5.0",
    );
    let (line, _) = row("/f/f1/pub/n-a/node/presence", &node_body);
    assert!(line.ends_with(" proto=2 aterm=0.5.0"), "{line}");
    let (body, _) = crate::body::Body::decode(
        b"v=1 t=1 inc=4 state=live hold=0 holder=- attention=- role=worker \
          detail=claude phase=busy context=12%25 title=satcomp%20run",
    );
    let (line, _) = row("/f/f1/pub/n-a/s-w/presence", &body);
    for present in [
        " role=worker ",
        " detail=claude ",
        " phase=busy ",
        " context=12%25 ",
        " title=satcomp%20run",
    ] {
        assert!(line.contains(present), "{present} missing from: {line}");
    }

    for quiet in [
        &b"v=1 t=1 state=live"[..],
        b"v=1 t=1 state=live attention=",
        b"v=1 t=1 state=live attention=-",
    ] {
        let (_, esc) = row(
            "/f/f1/pub/n-a/s-w/presence",
            &crate::body::Body::decode(quiet).0,
        );
        assert!(!esc, "{}", String::from_utf8_lossy(quiet));
    }

    let (hostile, _) = crate::body::Body::decode(
        "v=1 t=1 state=\u{1b}[2J\u{1b}[H attention=%1B%5B31mred".as_bytes(),
    );
    let (line, _) = row("/f/f1/pub/n-a/s-w/presence", &hostile);
    assert!(
        !line.contains('\u{1b}') && line.is_ascii() && !line.contains('\n'),
        "an untrusted presence body reached the terminal raw: {line:?}"
    );
    assert_eq!(
        line.split_whitespace().count(),
        20,
        "one token per column, always: {line:?}"
    );

    let long = format!("v=1 t=1 attention={}", "é".repeat(400));
    let (line, _) = row(
        "/f/f1/pub/n-a/s-w/presence",
        &crate::body::Body::decode(long.as_bytes()).0,
    );
    let printed = line
        .split_whitespace()
        .find_map(|t| t.strip_prefix("attention="))
        .expect("the column");
    assert!(
        printed.len() <= crate::glance::ATTENTION_CAP,
        "an unbounded token reached the roster: {} bytes",
        printed.len()
    );
    assert!(printed.ends_with(ELLIPSIS), "{printed}");
    assert!(
        crate::pct::decode(printed).ends_with('…'),
        "the cut landed inside a %XX escape: {printed}"
    );
}

/// `--token-file` refuses a symlink rather than following it.
#[cfg(unix)]
#[test]
fn a_symlinked_token_file_is_refused() {
    let scratch = crate::test_scratch_dir("tokfile");
    let dir = scratch.path();
    let elsewhere = dir.join("elsewhere");
    std::fs::write(&elsewhere, "feed1234\n").expect("a token-shaped file");
    let planted = dir.join("a.sock.token");
    std::os::unix::fs::symlink(&elsewhere, &planted).expect("plant");
    let args = argv(&format!(
        "--fleet f1 --broker /tmp/b.sock --state /tmp/s --sock /tmp/a.sock --token-file {}",
        planted.display()
    ));
    let refused = parse(&args).map(|p| p.cfg.token);
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("--token-file")),
        "the symlink was followed: {refused:?}"
    );
}
