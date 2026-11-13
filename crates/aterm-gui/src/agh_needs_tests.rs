// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

fn need(key: &str) -> AghNeed {
    AghNeed {
        key: key.to_string(),
        what: format!("what {key}"),
        why: format!("why {key}"),
        then: format!("then {key}"),
        at_secs: 7,
    }
}

#[cfg(unix)]
fn json(key: &str, v: u64) -> String {
    format!(
        "{{\"at_secs\":7,\"key\":\"{key}\",\"then\":\"then {key}\",\"v\":{v},\
         \"what\":\"what {key}\",\"why\":\"why {key}\"}}"
    )
}

#[cfg(unix)]
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("agh-needs-")
        .tempdir()
        .expect("a scratch directory")
}

const STALE: &str = "agh.intent.42-0123456789ab.stale";
const MISSING: &str = "agh.config.missing";
const INVALID: &str = "agh.config.invalid";

#[test]
fn each_need_shape_states_one_entry_under_its_own_key() {
    for key in [STALE, MISSING, INVALID] {
        let made = entries(&[need(key)]);
        assert_eq!(made.len(), 1, "{key}");
        assert_eq!(made[0].key(), key);
        assert_eq!(made[0].family(), Family::Agh);
        assert!(made[0].message().is_ok(), "{key}: {:?}", made[0].message());
    }
}

/// `agh.config.invalid` has two causes with two steps (agh's fault, `agh policy check`; a
/// file that says `vault = true`, which that check accepts, removing the line), so its
/// entry carries the need's own `then` rather than one fixed step.
#[test]
fn an_invalid_config_entry_carries_the_need_s_own_step() {
    let mut vault = need(INVALID);
    vault.then = "remove the vault line".to_string();
    for need in [need(INVALID), vault] {
        let shown = format!("{:?}", entries(std::slice::from_ref(&need))[0].message());
        assert!(shown.contains(&need.then), "{shown}");
    }
}

#[test]
fn a_need_of_another_shape_states_nothing() {
    for key in [
        "agh.other.thing",
        "agh.intent.42-0123456789AB.stale",
        "agh.intent.x-0123456789ab.stale",
        "agh.intent.42-0123456789a.stale",
    ] {
        assert!(entries(&[need(key)]).is_empty(), "{key}");
    }
}

/// Each control breaks one rule alone, so dropping that rule from `read` lets a need
/// through: a symlink to a good need, a good need padded past [`NEED_FILE_MAX`], a good
/// need whose name does not end `.json` (its stem is its key), a FIFO, which must not
/// block, then a `v: 2` need and one whose stem is not its key.
#[cfg(unix)]
#[test]
fn read_skips_what_is_not_a_need() {
    use std::os::unix::ffi::OsStrExt as _;
    let root = scratch();
    let dir = root.path().join("needs");
    let outside = root.path().join("outside");
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();
    std::fs::write(outside.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
    std::os::unix::fs::symlink(
        outside.join(format!("{INVALID}.json")),
        dir.join(format!("{INVALID}.json")),
    )
    .unwrap();
    let big = format!(
        "{{\"at_secs\":7,\"key\":\"agh.big\",\"pad\":\"{}\",\"then\":\"t\",\"v\":1,\
         \"what\":\"w\",\"why\":\"y\"}}",
        "p".repeat(9 << 10)
    );
    std::fs::write(dir.join("agh.big.json"), big).unwrap();
    std::fs::write(dir.join("agh.text.txt"), json("agh.text.txt", 1)).unwrap();
    let fifo = dir.join(format!("{STALE}.json"));
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("a FIFO path");
    // SAFETY: `fifo_c` is a live NUL-terminated pathname in this test's own scratch
    // directory, and mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    std::fs::write(dir.join("agh.two.json"), json("agh.two", 2)).unwrap();
    std::fs::write(dir.join("agh.stem.json"), json("agh.other", 1)).unwrap();
    // A hang detector, not a budget: a read that opened the FIFO would block forever.
    let (tx, rx) = std::sync::mpsc::channel();
    let looked = dir.clone();
    std::thread::spawn(move || tx.send(read(&looked)));
    let needs = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("read returned: a FIFO never blocks it");
    assert_eq!(needs, vec![need(MISSING)]);
    assert_eq!(read(&dir.join("absent")), Vec::new());
}

/// A text past [`TEXT_MAX`] is cut, ending `…`, and its need is still read and stated.
#[cfg(unix)]
#[test]
fn a_long_text_is_cut_and_its_need_still_stated() {
    let root = scratch();
    let dir = root.path();
    let long = "y".repeat(1000);
    let text = format!(
        "{{\"at_secs\":7,\"key\":\"{INVALID}\",\"then\":\"t\",\"v\":1,\"what\":\"w\",\
         \"why\":\"{long}\"}}"
    );
    std::fs::write(dir.join(format!("{INVALID}.json")), text).unwrap();
    let needs = read(dir);
    assert_eq!(needs.len(), 1, "{needs:?}");
    assert_eq!(needs[0].why.chars().count(), TEXT_MAX);
    assert!(needs[0].why.ends_with('…'), "{}", needs[0].why);
    let made = entries(&needs);
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].key(), INVALID);
    assert!(made[0].message().is_ok(), "{:?}", made[0].message());
}

#[cfg(unix)]
#[test]
fn needs_are_stated_at_most_once_a_minute_and_end_when_their_files_go() {
    let root = scratch();
    let dir = root.path().to_path_buf();
    let mut app = crate::App::headless_for_test();
    app.agh_needs = AghNeedsLook::at(dir.clone());
    let under = |app: &crate::App| app.messages.digest_keys_under("agh.");
    let t0 = Instant::now();

    std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();
    app.hand_agh_needs(t0);
    assert_eq!(under(&app), vec![MISSING.to_string()]);

    std::fs::write(dir.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
    app.hand_agh_needs(t0 + Duration::from_secs(30));
    assert_eq!(under(&app).len(), 1, "a look is due once a minute");
    app.hand_agh_needs(t0 + Duration::from_secs(61));
    assert_eq!(under(&app).len(), 2);

    std::fs::remove_file(dir.join(format!("{MISSING}.json"))).unwrap();
    std::fs::remove_file(dir.join(format!("{INVALID}.json"))).unwrap();
    app.hand_agh_needs(t0 + Duration::from_secs(122));
    assert_eq!(
        under(&app).len(),
        2,
        "a level entry ends on the digest's clock"
    );
    let due = app.messages.digest_deadline().expect("an end is due");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert_eq!(under(&app), Vec::<String>::new());
}

/// NEGATIVE CONTROL: Non-need `.json` files (`00.json`..`39.json`) and invalid `agh.*.json`
/// files sorting lexicographically before real needs (`agh.config.invalid.json`,
/// `agh.config.missing.json`) must not consume [`NEEDS_MAX`] slots and hide real needs.
#[cfg(unix)]
#[test]
fn non_need_and_corrupt_json_files_sorting_first_do_not_starve_real_needs() {
    let root = scratch();
    let dir = root.path();
    for i in 0..40_u32 {
        std::fs::write(dir.join(format!("{i:02}.json")), "{}").unwrap();
        std::fs::write(dir.join(format!("agh.{i:02}.json")), b"{corrupt").unwrap();
    }
    std::fs::write(dir.join(format!("{INVALID}.json")), json(INVALID, 1)).unwrap();
    std::fs::write(dir.join(format!("{MISSING}.json")), json(MISSING, 1)).unwrap();

    let found = read(dir);
    assert_eq!(found, vec![need(INVALID), need(MISSING)]);
}
