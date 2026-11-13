// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A SETTING THAT DOES NOT ANSWER HOLDS THIS LOOK: a person's `false` may be behind it, and
/// installing over that is the worse mistake, so the read answers off within its bound. While
/// it stays out the next asks answer at once and start no other, and once it returns the next
/// look asks again. CONTROLS: a read that answers is read, either way.
#[test]
fn a_setting_that_does_not_answer_holds_this_look() {
    use std::time::{Duration, Instant};
    static READS: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("aterm-update-test-read");
    const OFF: &str = "[update]\nauto_apply = false\n";
    const ON: &str = "[update]\nauto_apply = true\n";
    let says = |text: &'static str| move || Ok(Some(text.to_string()));
    let plenty = Duration::from_secs(60);
    assert!(
        !super::auto_apply_within(&READS, plenty, says(OFF)),
        "CONTROL: a person's false holds"
    );
    assert!(
        super::auto_apply_within(&READS, plenty, says(ON)),
        "CONTROL: an answer of on is read"
    );
    let (release, released) = std::sync::mpsc::channel::<()>();
    assert!(
        !super::auto_apply_within(&READS, Duration::from_millis(50), move || {
            let _ = released.recv();
            Ok(Some(ON.to_string()))
        }),
        "the read stays out: this look holds"
    );
    assert!(
        !super::auto_apply_within(&READS, plenty, says(ON)),
        "while it stays out, the next ask answers at once"
    );
    assert_eq!(READS.running(), 1, "no second read");
    drop(release);
    let by = Instant::now() + plenty;
    while READS.running() != 0 {
        assert!(Instant::now() < by, "the stuck read returns");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        super::auto_apply_within(&READS, plenty, says(ON)),
        "once it returns, the next look asks again"
    );
}

/// A FAULT THAT CAN PASS HOLDS THIS LOOK, AND ONE THAT STANDS DOES NOT: a read that failed for a
/// moment said a person's `false` was on, while no permission on the file for good must not stop
/// updates for good.
#[test]
fn a_fault_that_can_pass_holds_this_look_and_one_that_stands_does_not() {
    use std::io::{Error, ErrorKind};
    use std::time::Duration;
    static READS: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("aterm-update-fault-read");
    let reads_on = |error: Error| {
        super::auto_apply_within(&READS, Duration::from_secs(60), move || Err(error))
    };
    assert!(!reads_on(Error::from(ErrorKind::TimedOut)));
    assert!(!reads_on(Error::from(ErrorKind::OutOfMemory)));
    assert!(reads_on(Error::from(ErrorKind::PermissionDenied)));
    #[cfg(unix)]
    {
        for passing in [
            libc::EIO,
            libc::ESTALE,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENXIO,
            libc::ENODEV,
            libc::EHOSTDOWN,
            libc::ENETDOWN,
            libc::ENETUNREACH,
            libc::ENOTCONN,
            libc::ECONNRESET,
        ] {
            assert!(!reads_on(Error::from_raw_os_error(passing)), "{passing}");
        }
        #[cfg(target_os = "macos")]
        for passing in [libc::EDEVERR, libc::EPWROFF] {
            assert!(!reads_on(Error::from_raw_os_error(passing)), "{passing}");
        }
        for standing in [
            libc::EACCES,
            libc::EPERM,
            libc::ELOOP,
            libc::ENOTDIR,
            libc::ENAMETOOLONG,
            libc::EINVAL,
            libc::EISDIR,
            libc::EOPNOTSUPP,
            libc::ENOTSUP,
            libc::EILSEQ,
            libc::EOVERFLOW,
        ] {
            assert!(reads_on(Error::from_raw_os_error(standing)), "{standing}");
        }
    }
}

/// A STRAY BYTE HIDES NO SETTING: a comment in Latin-1 made the file unreadable, which read as
/// on against a `false` beside it. CONTROLS: a file with no text to read has none, and is no
/// error.
#[test]
fn a_stray_byte_in_the_file_hides_no_setting() {
    let dir = crate::test_scratch_dir("upd-settings-text");
    let path = dir.path().join("aterm.toml");
    std::fs::write(&path, b"[update]\nauto_apply = false\n# caf\xe9\n").unwrap();
    let text = read_config_file(&path).expect("its text");
    assert!(!parse_update_auto_apply(&text));
    assert!(matches!(
        read_config_text(&dir.path().join("absent.toml")),
        Ok(None)
    ));
    assert!(matches!(read_config_text(dir.path()), Ok(None)));
    let huge = usize::try_from(MAX_CONFIG_BYTES).unwrap() + 1;
    std::fs::write(&path, vec![b'#'; huge]).unwrap();
    assert!(matches!(read_config_text(&path), Ok(None)));
}

/// A SOCKET AT THE NAME IS NO FILE: its open fails with an error a volume that went away also
/// gives, and reading that as a fault that can pass would hold every update for as long as it
/// stood there.
#[cfg(unix)]
#[test]
fn a_socket_at_the_config_s_name_has_no_text() {
    let dir = crate::test_scratch_dir("sock");
    let socket = dir.path().join("s");
    let _listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert!(matches!(read_config_text(&socket), Ok(None)));
}

#[test]
fn absent_is_on_and_false_is_off() {
    assert!(parse_update_enabled(""));
    assert!(parse_update_enabled("font_px = 12.0\n"));
    assert!(parse_update_enabled("[update]\nauto_apply = false\n"));
    assert!(parse_update_enabled("[update]\nenabled = true\n"));
    assert!(!parse_update_enabled("[update]\nenabled = false\n"));
}

#[test]
fn auto_apply_is_on_unless_the_table_says_false() {
    assert!(parse_update_auto_apply(""));
    assert!(parse_update_auto_apply("[update]\nenabled = false\n"));
    assert!(!parse_update_auto_apply("[update]\nauto_apply = false\n"));
    assert!(
        !parse_update_auto_apply("[update]\nauto_apply = \"no\"\n"),
        "a wrong type is not true, and may be a person's false"
    );
    assert!(parse_update_auto_apply("[keeper]\nauto_apply = \"no\"\n"));
    assert!(
        !parse_update_auto_apply("[update\nauto_apply = false\n"),
        "text that is not TOML and names it may be a person's false"
    );
    for on in [
        "[update\nauto_apply = true\n",
        "[update\n# auto_apply = false\n",
        "[update\nbroken",
        "update = { auto_apply = true }\nbroken",
        "[update]\nrepo = \"auto_apply\"\nbroken",
        "[update]\nrepo = \"a#b auto_apply-fork\"\nbroken",
        "[update]\nauto_apply_note = 1\nbroken",
        "[update]\nauto_apply: true\n",
        "[update]\nauto_apply true\n",
        "{\"update\": {\"auto_apply\": true}}\n",
        "[keeper]\nnote = \"don't use auto_apply\"\nbroken",
        "[update]\nauto_apply = True\n",
        "[update]\nauto_apply = true;\n",
        "[update]\nauto_apply = true // keep updating\n",
        "[update]\nauto_apply = true ; note\n",
        "[update]\nauto_apply == true\n",
        "[update]\nrepo = \"auto_apply\n",
        "[update]\nnote = \"set auto_apply = false here\"\nbroken",
        "[update]\nnote = \"auto_apply' = false\"\nbroken",
        "[update]\nauto_apply =\u{3000}true\n",
        "auto_apply\u{a0}=\u{a0}true\n",
        "update = { auto_apply = true repo = \"me/aterm\" }\n",
        "update = { auto_apply = true enabled = true }\n",
        "[update]\nauto_apply = true - keep it\n",
        "// auto_apply = true is the default\nbroken",
    ] {
        assert!(parse_update_auto_apply(on), "CONTROL: {on:?}");
    }
    for off in [
        "update = { note = \"it's mine\", auto_apply = false }\nbroken",
        "update = {\n  auto_apply = false\n}\nbroken",
        "update = { repo = \"me/aterm, auto_apply = false }\n",
        "update = { note = don't, auto_apply = false }\n",
        "\"auto_apply = false\n",
        "'auto_apply\" = false\n",
        "[update]\nauto_apply = maybe\n",
        "[update]\nauto_apply = true false\n",
        "[update]\nauto_apply = true FALSE\n",
        "update = { enabled = true auto_apply = false }\n",
        "[update]\nauto_apply\n",
    ] {
        assert!(!parse_update_auto_apply(off), "{off:?}");
    }
    assert!(
        !parse_update_auto_apply("update = { repo = \"a#b\", auto_apply = false }\nbroken"),
        "a `#` in a string starts no comment"
    );
    assert!(!parse_update_enabled(
        "[update]\nenabled = false\nauto_apply = \"no\"\n"
    ));
    assert!(!parse_update_auto_apply(
        "[update]\nenabled = \"no\"\nauto_apply = false\n"
    ));
    assert!(
        parse_update_enabled("[update]\nauto_apply = \"no\"\n"),
        "a typo in one never flips the other"
    );
    assert!(parse_update_auto_apply("[update]\nenabled = \"no\"\n"));
}

/// A PERSON'S `enabled = false` HOLDS IN TEXT THAT IS NOT TOML, as `auto_apply = false` does: a typo
/// on another line let a Mac with no window check and install against it, and so did the same typo
/// on the line itself. A wrong type in text that is TOML holds too. CONTROLS: `enabled` is a key of
/// many tables, so another table's says nothing, and neither does a comment or `true`.
#[test]
fn a_person_s_enabled_false_holds_in_text_that_is_not_toml() {
    for text in [
        "[update]\nenabled = false\nbroken",
        "broken\n[update]\nenabled=false",
        "[update\nenabled = false\n",
        "update.enabled = false\nbroken",
        "[update]\nenabled = \"no\"\nbroken",
        "[update]\nenabled = \"no\"\n",
        "[update]\nenabled: false\n",
        "[update]\nenabled false\n",
        "update = { enabled = false }\nbroken",
        "[update]\n\"enabled\" = false\nbroken",
        "update . enabled = false\nbroken",
        "\u{feff}[update]\nenabled = false\nbroken",
        "[update] enabled = false\nbroken",
        "[[update]]\nenabled = false\nbroken",
        "[\"update\"]\nenabled = false\nbroken",
        "\"update\".enabled = false\nbroken",
        "update]\nenabled = false\n",
        "[update.enabled]\nvalue = false\nbroken",
        "update = { repo = \"a#b\", enabled = false }\nbroken",
        "[update]\nlist = [\n1\n]\nenabled = false\nbroken",
        "[update]\nlist = [\n  [1],\n]\nenabled = false\nbroken",
        "[update]\nlist = [\n  [1]\n]\nenabled = false\nbroken",
        "list = [1, 2\n[update]\nenabled = false\n",
        "update = {\n  enabled = false\n}\nbroken",
        "update = { note = \"it's mine\", enabled = false }\nbroken",
        "[update]\n]\nenabled = false\nbroken",
        "[update}\nenabled = false\n",
        "[update[\nenabled = false\n",
        "[update\\\nenabled = false\n",
        "[update.enabled = false]\n",
        "update = { repo = \"me/aterm, enabled = false }\n",
        "[update]\n\"enabled = false\n",
        "[update]\nenabled = true false\n",
        "[update.enabled: false]\n",
    ] {
        assert!(!parse_update_enabled(text), "{text:?}");
        assert_eq!(
            saved_enabled_of(Ok(Some(text.to_owned()))),
            Some(false),
            "{text:?}"
        );
    }
    for text in [
        "[keeper]\nenabled = false\nbroken",
        "[update]\n[keeper]\nenabled = false\nbroken",
        "[update]\n# enabled = false\nbroken",
        "[update]\nenabled = true\nbroken",
        "[update.other]\nenabled = false\nbroken",
        "[keeper]\nenabled = \"no\"\n",
        "update = { enabled = true }\nbroken",
        "[update]\nrepo = \"enabled-fork\"\nbroken",
        "[update]\nrepo = \"enabled\"\nbroken",
        "[update]\nenabled_when = 1\nbroken",
        "[update]\nenabled: true\n",
        "[update]\nenabled true\n",
        "[update]\nnote = \"don't touch enabled\"\nbroken",
        "update = {\n  enabled = true\n}\nbroken",
        "update = {\n  repo = \"x\"\n}\n[keeper]\nenabled = false\nbroken",
        "[keeper]\nlist = [\n  [1],\n]\nenabled = false\nbroken",
        "list = [1, 2\n[update]\nrepo = \"x\"\n[keeper]\nenabled = false\n",
        "update = {\n  repo = \"x\"\n}\nenabled = false\nbroken",
        "list = [\n  enabled = false\n]\nbroken",
        "[update]\nnote = \"[\"\n[keeper]\nenabled = false\nbroken",
        "[update]\nenabled = True\n",
        "[update]\nenabled = true;\n",
        "[update]\nenabled == true\n",
        "[update}\nenabled = true\n",
        "[keeper}\nenabled = false\n",
        "[update]\nenabled-fork = 1\nbroken",
        "update = { enabled = true auto_apply = true }\n",
        "update = { enabled = true repo = \"me/aterm\" }\n",
        "[update]\nenabled = true -- keep updating\n",
        "[update.enabled = true]\n",
        "[update.enabled: true]\n",
        "[update]\nenabled =\u{a0}true\n",
        "[update]\nenabled\u{a0}= true\n",
        "[update]\n\"enabled\"\u{a0}= true\n",
    ] {
        assert!(parse_update_enabled(text), "CONTROL: {text:?}");
    }
}

/// AN `update` THAT IS NO TABLE MAY BE A PERSON'S `false`: `update = false` and `[[update]]` were read
/// as saying nothing while the file parsed, so fixing a typo elsewhere turned the same text from a
/// hold into an install, and a typo elsewhere does not make it say nothing. CONTROLS: `true` says
/// nothing against it, an empty array says nothing, an array's table says nothing of a key it does not
/// name, and neither does a longer word or a table's own `update` key.
#[test]
fn an_update_that_is_no_table_may_be_a_person_s_false() {
    let (enabled, auto_apply) = (parse_update_enabled, parse_update_auto_apply);
    for text in [
        "update = false\n",
        "update = 5\n",
        "update = \"off\"\n",
        "update = [false]\n",
        "update = false\nbroken",
        "update = 5\nbroken",
        "update = \"off\"\nbroken",
        "update = [false]\nbroken",
        "update = [true, false]\nbroken",
        "\"update\" = false\nbroken",
        "font_px = big\nupdate = false\n",
        "\u{feff}update = false\nbroken",
        "update false\n",
    ] {
        assert!(!enabled(text), "{text:?}");
        assert!(!auto_apply(text), "{text:?}");
    }
    assert!(!enabled("[[update]]\nenabled = false\n"));
    assert!(auto_apply("[[update]]\nenabled = false\n"));
    assert!(!auto_apply("[[update]]\nauto_apply = false\n"));
    assert!(!auto_apply("update = [{ auto_apply = false }]\n"));
    assert!(!auto_apply(
        "[[update]]\nauto_apply = true\n[[update]]\nauto_apply = false\n"
    ));
    for text in [
        "update = true\n",
        "update = []\n",
        "[[update]]\n",
        "update = true\nbroken",
        "update = true # not false\nbroken",
        "update = [true]\nbroken",
        "update = [ true ]\nbroken",
        "update = []\nbroken",
        "update = [ { repo = \"x\" } ]\nbroken",
        "update_note 1\nbroken",
        "update notes\nbroken",
        "updatefalse\nbroken",
        "update = { repo = \"x\" }\nbroken",
        "update_note = 1\nbroken",
        "[keeper]\nupdate = false\nbroken",
    ] {
        assert!(enabled(text), "CONTROL: {text:?}");
        assert!(auto_apply(text), "CONTROL: {text:?}");
    }
    assert!(auto_apply("[[update]]\nauto_apply = true\n"), "CONTROL");
}

#[test]
fn only_the_update_table_is_read() {
    assert!(!parse_update_enabled(
        "[packages]\nenabled = \"yes\"\n[update]\nenabled = false\n"
    ));
    assert!(!parse_update_enabled("[update]\nenabled = \"no\"\n"));
    assert!(parse_update_enabled("[packages]\nenabled = \"no\"\n"));
    assert!(parse_update_enabled("[update\nbroken"));
    assert!(parse_update_enabled(
        "[update]\nrepo = \"x\"\n[packages]\nenabled = \"no\"\n"
    ));
}

#[test]
fn a_missing_or_foreign_file_is_on() {
    let dir = crate::test_scratch_dir("upd-settings");
    assert!(update_enabled_at(&dir.path().join("absent.toml")));
    assert!(
        update_enabled_at(dir.path()),
        "a directory is not a config file"
    );
    let off = dir.path().join("aterm.toml");
    std::fs::write(&off, "[update]\nenabled = false\n").unwrap();
    assert!(!update_enabled_at(&off));
}

/// THE SAVED SETTING IS READ AGAIN AT EACH CALL, AS THE PARSER READS IT: it follows the file
/// as the file changes, and reads every text, one that does not parse included, as
/// `parse_update_enabled` does (which alone warns).
#[test]
fn the_saved_setting_follows_the_file_and_reads_as_the_parser_does() {
    for text in [
        "",
        "[update]\nenabled = false\n",
        "[update]\nenabled = true\n",
        "[update]\nenabled = \"no\"\n",
        "[update\nbroken",
    ] {
        assert_eq!(
            update_enabled_in(text).unwrap_or(true),
            parse_update_enabled(text),
            "{text:?}"
        );
    }
    let dir = crate::test_scratch_dir("upd-settings-saved");
    let path = dir.path().join("aterm.toml");
    assert_eq!(
        update_enabled_saved_at(None),
        Some(true),
        "no config path reads as on"
    );
    let saved_here = || update_enabled_saved_at(Some(&path));
    assert_eq!(saved_here(), Some(true), "no file reads as on");
    std::fs::write(&path, "[update]\nenabled = false\n").unwrap();
    assert_eq!(saved_here(), Some(false), "turned off");
    std::fs::write(&path, "[update]\nenabled = true\n").unwrap();
    assert_eq!(saved_here(), Some(true), "turned on again");
    std::fs::write(&path, "[update\nbroken").unwrap();
    assert_eq!(saved_here(), Some(true), "a broken file reads as on");
}

/// A FAULT THAT CAN PASS SAYS NOTHING OF `[update] enabled`: a process comparing it would read
/// a change nobody made, over a person's `false`. CONTROL: one that stands reads as a file with
/// no text.
#[cfg(unix)]
#[test]
fn a_fault_that_can_pass_says_nothing_of_the_saved_setting() {
    let failed = |code| Err(std::io::Error::from_raw_os_error(code));
    assert_eq!(saved_enabled_of(failed(libc::EIO)), None);
    assert_eq!(
        saved_enabled_of(failed(libc::EACCES)),
        Some(true),
        "CONTROL: no permission stands"
    );
}

/// THE LAUNCH SETTING KEEPS ONLY A READ THAT ANSWERED: a fault that can pass at a process's
/// first ask kept nothing, so a person's `false` read once the fault passed holds for the rest of
/// the process, as it would have from the start. CONTROLS: one that stands reads as a file with
/// no text, and once kept, later reads change nothing.
#[cfg(unix)]
#[test]
fn the_launch_setting_keeps_only_a_read_that_answered() {
    let failed = |code| Err(std::io::Error::from_raw_os_error(code));
    assert_eq!(launch_enabled_of(failed(libc::EIO)), None);
    assert_eq!(
        launch_enabled_of(failed(libc::EACCES)),
        Some(true),
        "CONTROL: no permission stands"
    );
    assert_eq!(
        launch_enabled_of(Ok(Some("[update]\nenabled = false\n".into()))),
        Some(false)
    );
    assert_eq!(launch_enabled_of(Ok(None)), Some(true));
    let kept = std::sync::OnceLock::new();
    assert_eq!(
        kept_or_read(&kept, || launch_enabled_of(failed(libc::EIO))),
        None
    );
    assert_eq!(kept.get(), None, "a fault that can pass keeps nothing");
    assert_eq!(
        kept_or_read(&kept, || launch_enabled_of(Ok(Some(
            "[update]\nenabled = false\n".into()
        )))),
        Some(false)
    );
    assert_eq!(
        kept_or_read(&kept, || panic!(
            "CONTROL: once kept, nothing reads it again"
        )),
        Some(false),
        "CONTROL: once kept, the launch's answer stands"
    );
}

/// ONLY A LAUNCH'S FIRST ASK READS THE FILE: a first read that hit a fault that can pass leaves
/// every later ask answering `None` with no read (a stalled volume costs one stall), until a
/// re-read keeps an answer, which every ask then gives. CONTROL: a first read that answers keeps
/// it, and the next ask reads nothing.
#[cfg(unix)]
#[test]
fn only_a_launchs_first_ask_reads_the_file() {
    let failed = |code| Err(std::io::Error::from_raw_os_error(code));
    let kept = std::sync::OnceLock::new();
    let read_once = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        kept_or_first_read(&kept, &read_once, || launch_enabled_of(failed(libc::EIO))),
        None
    );
    assert_eq!(
        kept_or_first_read(&kept, &read_once, || panic!("a later ask reads nothing")),
        None
    );
    assert_eq!(
        kept_or_read(&kept, || launch_enabled_of(Ok(Some(
            "[update]\nenabled = false\n".into()
        )))),
        Some(false),
        "the waiter's re-read keeps the answer"
    );
    assert_eq!(
        kept_or_first_read(&kept, &read_once, || panic!("a kept answer reads nothing")),
        Some(false)
    );

    let kept = std::sync::OnceLock::new();
    let read_once = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        kept_or_first_read(&kept, &read_once, || Some(true)),
        Some(true),
        "CONTROL: a first read that answers keeps it"
    );
    assert_eq!(
        kept_or_first_read(&kept, &read_once, || panic!("CONTROL: kept, nothing reads")),
        Some(true)
    );
}
