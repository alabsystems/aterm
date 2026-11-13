// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};

struct Wire {
    replies: Cursor<Vec<u8>>,
    sent: Arc<Mutex<Vec<u8>>>,
}

impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.replies.read(bytes)
    }
}

impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.sent.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn owned_mac_setup_password_requires_current_receipt_child_and_fenced_prompt() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join(aterm_types::clt_mark::FILE);
    let directory = children(&mark).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let exe = root.path().join("aterm");
    let name = "s-0123456789abcdef0123-42-700";
    let text = b"v2\ns-0123456789abcdef0123\n42\n700\nclaimed 1.0.0+gfixture\n";
    aterm_uds::private_file::write_private(
        root.path(),
        aterm_types::clt_mark::FILE,
        b"opened 1.0.0+gfixture\n",
    )
    .unwrap();
    aterm_uds::private_file::write_private(&directory, name, text).unwrap();
    let child = Process {
        identity: Identity {
            pid: 42,
            birth: aterm_uds::process::Birth(700),
            idversion: Some(3),
        },
        image: exe.clone(),
        argv: std::iter::once(exe.to_str().unwrap())
            .chain(COMMAND_LINE_TOOLS_ARGS)
            .map(|arg| arg.as_bytes().to_vec())
            .collect(),
    };
    let admitted = || {
        Binding::read(&mark, name, &exe, &|pid| {
            (pid == child.identity.pid).then(|| child.clone())
        })
    };
    let target = admitted().expect("real owned whole receipt and exact child");
    for changed in ["pid", "birth", "argv", "image"] {
        let mut wrong = child.clone();
        match changed {
            "pid" => wrong.identity.pid += 1,
            "birth" => wrong.identity.birth.0 += 1,
            "argv" => wrong.argv = vec![b"aterm".to_vec(), b"shell".to_vec()],
            _ => wrong.image = root.path().join("another-app"),
        }
        assert!(Binding::read(&mark, name, &exe, &|_| Some(wrong.clone())).is_none());
    }
    aterm_uds::private_file::write_private(
        root.path(),
        aterm_types::clt_mark::FILE,
        b"opened another-owner\n",
    )
    .unwrap();
    assert!(
        admitted().is_none(),
        "a receipt is no authority over another claim"
    );
    aterm_uds::private_file::write_private(
        root.path(),
        aterm_types::clt_mark::FILE,
        b"opened 1.0.0+gfixture\n",
    )
    .unwrap();
    aterm_uds::private_file::write_private(
        &directory,
        name,
        b"v2\ns-different\n42\n700\nclaimed 1.0.0+gfixture\n",
    )
    .unwrap();
    assert!(
        admitted().is_none(),
        "the file name binds the whole SID/child record"
    );
    aterm_uds::private_file::write_private(&directory, name, text).unwrap();

    const STATUS: &str = "OK gen=1.2 hash=0123456789abcdef human_seq=7\n";
    let run = |after: &str, ack: &str, current: &dyn Fn() -> bool| {
        let sent = Arc::new(Mutex::new(Vec::new()));
        // The saved failed VM's actual display wrap, including "Ma" / "c", then
        // the prompt and an empty display row. This is not an idealized secure field.
        let replies = format!(
            "{STATUS}OK 5\naterm is installing Apple's Command Line Tools for building software, turning on\ndeveloper mode, and setting up its local network helper. macOS asks for your Ma\nc password once.\nMac password:\n\n{after}{ack}"
        );
        let mut client = RelayClient::new(Wire {
            replies: Cursor::new(replies.into_bytes()),
            sent: sent.clone(),
        });
        let mut attempted = BTreeSet::new();
        let delivered = answer(
            &mut client,
            &target,
            "nux-fixture-password",
            current,
            &mut attempted,
        )
        .unwrap();
        let before = sent.lock().unwrap().clone();
        if !attempted.is_empty() {
            assert!(
                answer(
                    &mut client,
                    &target,
                    "nux-fixture-password",
                    current,
                    &mut attempted
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(
                *sent.lock().unwrap(),
                before,
                "one attempt even after uncertain delivery"
            );
        }
        (delivered, String::from_utf8(before).unwrap())
    };
    let (delivered, sent) = run(STATUS, "OK seq=9\n", &|| true);
    assert_eq!(delivered, Some(9));
    assert!(sent.ends_with(
        "send if-gen=1.2 if-fp=0123456789abcdef if-human=7 -- nux-fixture-password\\n\n"
    ));
    let (delivered, sent) = run(
        "OK gen=1.3 hash=0123456789abcdef human_seq=7\n",
        "",
        &|| true,
    );
    assert!(delivered.is_none());
    assert!(
        !sent.contains(" send "),
        "a changed prompt frame gets no credential bytes"
    );
    let calls = Cell::new(0);
    let (delivered, sent) = run(STATUS, "", &|| {
        calls.set(calls.get() + 1);
        calls.get() == 1
    });
    assert!(delivered.is_none());
    assert!(
        !sent.contains(" send "),
        "retired child custody gets no credential bytes"
    );
    for ack in [
        "OK skipped reason=person seq=9\n",
        "ERR write failed\n",
        "OK seq=9 dup=1\n",
        "OK seq=9 seq=10\n",
        "OK seq=9 unknown=1\n",
        "OK seq=18446744073709551616\n",
        "OK\n",
    ] {
        let (delivered, _) = run(STATUS, ack, &|| true);
        assert!(delivered.is_none(), "no full-delivery receipt: {ack}");
    }
}
