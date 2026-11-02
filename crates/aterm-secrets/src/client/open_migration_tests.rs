// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A loopback vault server for the duration of a test.
fn server(root: &Path) -> crate::server::Running {
    crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.to_owned(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .unwrap()
}

/// The vault at `paths` republished with its open note written back as the `open = yes` line an
/// aterm before 0.148 wrote.
fn reopened_by_an_earlier_aterm(paths: &Paths) {
    let admin = admin_open(paths).unwrap();
    let rendered = admin.published_doc.render();
    let text = std::str::from_utf8(rendered.as_slice()).unwrap();
    let note = format!("# {}", crate::text::OPEN_NOTE);
    assert!(text.contains(&note), "a new vault is open by its note");
    let doc = Document::parse(&text.replace(&note, "open = yes")).unwrap();
    assert!(doc.open_line());
    admin_publish(paths, &admin, &doc).unwrap();
}

/// Run `flow` with the vault at `paths` read as the pinned default ALab vault.
fn as_alab<T>(paths: &Paths, flow: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let pin = configured(paths).unwrap().signer.unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(pin),
        });
    });
    let _reset = Reset;
    flow()
}

/// The pinned ALab vault's admin approver writes an `open = yes` line (an earlier aterm's) as the
/// open note, once: the vault stays open to this aterm, an earlier aterm reads past the note,
/// and the next version, which holds no line, publishes nothing. A vault that is not the pinned
/// ALab one is left as it is.
///
/// NEGATIVE CONTROL: with `try_step` reading no line (`quieted` returning `Ok(None)`) the step
/// publishes nothing and this test fails at its first assert.
#[test]
fn the_alab_approver_writes_an_open_line_as_the_note_an_earlier_aterm_reads_past() {
    let root = aterm_tempfile::tempdir().unwrap();
    let running = server(&root.path().join("server"));
    let paths = Paths::for_home(&root.path().join("admin"));
    let url = format!("http://{}/v1/{}", running.addr, crate::ALAB_VAULT);
    create(&paths, &url, None, Some("m22")).unwrap();
    reopened_by_an_earlier_aterm(&paths);
    let said = std::cell::RefCell::new(Vec::new());
    let log = |line: &str| said.borrow_mut().push(line.to_owned());
    let config = configured(&paths).unwrap();
    let seq = admin_open(&paths).unwrap().seq;
    // CONTROL: read as any other vault, the line stays.
    assert!(!Rounds::default().step(&paths, &config, seq, &log));
    assert!(admin_open(&paths).unwrap().published_doc.open_line());
    let mut rounds = Rounds::default();
    as_alab(&paths, || {
        assert!(rounds.step(&paths, &config, seq, &log), "{said:?}");
        let admin = admin_open(&paths).unwrap();
        assert!(admin.seq > seq);
        assert!(!admin.published_doc.open_line());
        assert!(admin.published_doc.open());
        assert_eq!(Approval::of(&admin.published_doc), Some(Approval::Open));
        let rendered = admin.published_doc.render();
        let text = std::str::from_utf8(rendered.as_slice()).unwrap();
        assert!(
            !text
                .lines()
                .any(|line| line.trim_start().starts_with("open")),
            "no line an earlier aterm reads as open"
        );
        assert!(!rounds.step(&paths, &config, admin.seq, &log));
        assert_eq!(admin_open(&paths).unwrap().seq, admin.seq);
    });
    running.stop();
}
