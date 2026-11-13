// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The observed-account record over fixture config homes: what a login file's change, a read's
//! answer and a person's ask leave on record, what the gate then admits, and what the journal
//! is told. Every address is synthetic; no vendor program runs.

use super::*;
use crate::harness::observe::Observed;

const OWNER: &str = "owner@meta.example";
const OTHER: &str = "other@meta.example";
const NOW: i64 = 1_792_800_000;

/// A scratch harness state and one Muse config home under it.
struct Fixture {
    _tmp: aterm_tempfile::TempDir,
    store: Store,
    place: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let tmp = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-observed-account-{tag}-"))
        .tempdir()
        .expect("temp dir");
    let config = tmp.path().join("home/.config/muse");
    std::fs::create_dir_all(&config).expect("config home");
    Fixture {
        store: Store::at(&tmp.path().join("state"), ObservedCli::Muse),
        place: config.join("auth.json"),
        _tmp: tmp,
    }
}

impl Fixture {
    fn seen(&self, email: &str, mtime: i64) -> Observation {
        Observation::Seen(Observed {
            cli: ObservedCli::Muse,
            email: email.to_owned(),
            key: None,
            region: None,
            src: ObservedSrc::AuthFile,
            path: self.place.clone(),
            mtime: Some(mtime),
        })
    }

    fn gone(&self) -> Observation {
        Observation::SignedOut {
            path: self.place.clone(),
        }
    }

    fn record(&self, email: &str) -> Record {
        self.store.record(&self.place, email).expect("its record")
    }
}

/// A test keyer: one key per (realm, address), as the lab's would give.
fn keyer(realm: &str, email: &str) -> Option<String> {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(realm.as_bytes());
    hasher.update([0]);
    hasher.update(email.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Some(format!("ac-{hex}"))
}

fn kinds(told: &Told) -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    told.notes
        .iter()
        .map(|note| (note.kind, note.src, note.why))
        .collect()
}

/// THE FIRST FINDING: a Muse Code login has a record. Its first sighting makes one (nothing
/// known of its sign-in yet), a second pass rewrites nothing, and the record is found by the
/// login file and the address, whatever the address's case.
/// NEGATIVE CONTROL: with `observed` making no record for a seen login, there is none:
/// `` its record ``.
#[test]
fn a_seen_login_gets_a_record_once() {
    let fx = fixture("first");
    let (record, told) = fx.store.observe_with(&fx.seen(OWNER, NOW - 50), NOW);
    let record = record.expect("the login's record");
    assert_eq!(
        (record.email.as_str(), record.auth, record.seen_at),
        (OWNER, Auth::Unseen, Some(NOW))
    );
    assert_eq!(
        told,
        Told::default(),
        "a first sighting is the tab's to note"
    );
    let file = fx.store.file();
    let written = std::fs::metadata(&file).unwrap().modified().unwrap();
    let again = fx
        .store
        .observe(&fx.seen("Owner@Meta.Example", NOW - 50), NOW + 30);
    assert_eq!(again, Some(record));
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        written,
        "an unchanged login rewrites nothing"
    );
    assert_eq!(fx.store.kept().len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the record names addresses");
    }
}

/// THE SECOND FINDING: a login file that stops naming a login signs its account out on record,
/// and the journal hears it once, sharply, keyed by the account and never by its address.
/// NEGATIVE CONTROL: with `observed` ignoring `Observation::SignedOut`, the record still says
/// unseen: `` left: Unseen right: SignedOut ``.
#[test]
fn a_login_file_that_names_no_login_signs_its_account_out() {
    journal::with_keyer(keyer, || {
        let fx = fixture("gone");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let (record, told) = fx.store.observe_with(&fx.gone(), NOW + 10);
        assert_eq!(record, None, "no login to show");
        let record = fx.record(OWNER);
        assert_eq!(
            (record.auth, record.auth_at),
            (Auth::SignedOut, Some(NOW + 10))
        );
        assert_eq!(kinds(&told), [("signed-out", "auth-file", None)]);
        let note = told.notes[0];
        assert_eq!(note.cli, "muse");
        assert_eq!(note.account, account_key(ObservedCli::Muse, OWNER));
        assert_eq!(
            told.logins
                .iter()
                .map(|login| (login.realm.as_str(), login.email.as_str(), login.state))
                .collect::<Vec<_>>(),
            [("meta", OWNER, LoginState::SignedOut)]
        );
        assert!(
            !format!("{:?}", told.notes).contains(OWNER),
            "a note names no address"
        );
        // Said once: the next pass of the same file changes nothing.
        assert_eq!(
            fx.store.observe_with(&fx.gone(), NOW + 20).1,
            Told::default()
        );
    });
}

/// A login that moved to another account in the same store signs the first one out and gives
/// the second its own record.
#[test]
fn a_store_that_names_another_account_signs_the_first_out() {
    journal::with_keyer(keyer, || {
        let fx = fixture("moved");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let (record, told) = fx.store.observe_with(&fx.seen(OTHER, NOW + 5), NOW + 10);
        assert_eq!(record.map(|record| record.email), Some(OTHER.to_owned()));
        assert_eq!(fx.record(OWNER).auth, Auth::SignedOut);
        assert_eq!(fx.record(OTHER).auth, Auth::Unseen);
        assert_eq!(kinds(&told), [("signed-out", "auth-file", None)]);
        assert_eq!(told.notes[0].account, account_key(ObservedCli::Muse, OWNER));
    });
}

/// THE GATE: a login on record as signed out is admitted no read, by anyone, and nothing is
/// stamped for it; a login nothing is known of, or known to work, is admitted one a minute.
/// NEGATIVE CONTROL: with `Record::standing` not reading `auth`, the signed-out login is
/// admitted: `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn a_signed_out_login_is_admitted_no_read() {
    let fx = fixture("gate");
    assert_eq!(
        fx.store.admit(&fx.place, OWNER, false, NOW).unwrap_err(),
        Refusal::Unlisted,
        "a login no file was observed for"
    );
    fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
    let pass = fx
        .store
        .admit(&fx.place, OWNER, false, NOW)
        .expect("CONTROL");
    assert_eq!((pass.at(), pass.email()), (NOW, OWNER));
    assert_eq!(
        fx.store
            .admit(&fx.place, OWNER, true, NOW + 59)
            .unwrap_err(),
        Refusal::Recent,
        "one read a minute, whoever asks"
    );
    fx.store.landed(pass, Landing::SignedOut, NOW + 3);
    let stamped = fx.record(OWNER).read_at;
    for asked in [false, true] {
        let refused = fx
            .store
            .admit(&fx.place, OWNER, asked, NOW + 7 * 86_400)
            .unwrap_err();
        assert_eq!(
            refused,
            Refusal::SignedOut {
                email: OWNER.to_owned(),
                since: Some(NOW + 3),
            }
        );
        let words = refused.words(ObservedCli::Muse);
        assert!(
            words.starts_with("signed-out: muse's login owner@meta.example"),
            "{words}"
        );
    }
    assert_eq!(
        fx.record(OWNER).read_at,
        stamped,
        "a refusal stamps nothing"
    );
}

/// THE SECOND FINDING, the expired token: a read that ends on the vendor's sign-in words
/// records the sign-out and tells the fleet, and the next session's read is refused instead of
/// starting the vendor's program again.
/// NEGATIVE CONTROL: with `landed` treating `Landing::SignedOut` as `Landing::Nothing`, the
/// journal hears an error, not a sign-out: `` left: [("error", "probe", Some("unreadable"))]
/// right: [("signed-out", "probe", None)] ``.
#[test]
fn a_read_that_meets_sign_in_words_signs_the_account_out() {
    journal::with_keyer(keyer, || {
        let fx = fixture("expired");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let pass = fx.store.admit(&fx.place, OWNER, false, NOW).unwrap();
        let told = fx.store.landed_with(pass, Landing::SignedOut, NOW + 4);
        assert_eq!(kinds(&told), [("signed-out", "probe", None)]);
        assert_eq!(told.logins.len(), 1);
        assert_eq!(told.logins[0].state, LoginState::SignedOut);
        // Another session, a day on, the same file: no read.
        let later = NOW + 86_400;
        let record = fx.store.observe(&fx.seen(OWNER, NOW - 50), later).unwrap();
        assert_eq!(record.auth, Auth::SignedOut);
        assert!(record.standing(false, later).is_err());
        assert!(fx.store.admit(&fx.place, OWNER, false, later).is_err());
    });
}

/// A Muse Code tab stopped at its own sign-in wall signs its login's account out, though its
/// file still names the login: the journal hears it from the CLI's output, the next session
/// starts no read on it, a second wall tells nothing more, and a sign-in that rewrites the file
/// is read again. A login not on record, or a wall with no login, is left.
/// NEGATIVE CONTROL: with `walled_with` changing nothing, the journal hears no sign-out:
/// `` left: [] right: [("signed-out", "cli-output", None)] ``.
#[test]
fn a_sign_in_wall_on_screen_signs_the_account_out() {
    journal::with_keyer(keyer, || {
        let fx = fixture("walled");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let told = fx.store.walled_with(&fx.seen(OWNER, NOW - 50), NOW + 2);
        assert_eq!(kinds(&told), [("signed-out", "cli-output", None)]);
        assert_eq!(told.logins.len(), 1);
        assert_eq!(told.logins[0].state, LoginState::SignedOut);
        let later = NOW + 600;
        let record = fx.store.observe(&fx.seen(OWNER, NOW - 50), later).unwrap();
        assert_eq!(record.auth, Auth::SignedOut);
        assert!(fx.store.admit(&fx.place, OWNER, false, later).is_err());
        assert_eq!(
            fx.store.walled_with(&fx.seen(OWNER, NOW - 50), later),
            Told::default(),
            "an account already signed out tells nothing more"
        );
        // `muse login` rewrote the file: a new sign-in, read once more.
        let back = fx
            .store
            .observe(&fx.seen(OWNER, later + 5), later + 10)
            .unwrap();
        assert_eq!(back.auth, Auth::Unseen);
        assert!(fx.store.admit(&fx.place, OWNER, false, later + 10).is_ok());
        assert_eq!(
            fx.store.walled_with(&fx.seen(OTHER, NOW), NOW),
            Told::default(),
            "a login not on record"
        );
        assert_eq!(
            fx.store.walled_with(&fx.gone(), NOW),
            Told::default(),
            "no login"
        );
    });
}

/// "Some other hook should handle this": a login file written since the sign-out was recorded
/// is a new sign-in, of which nothing is known until one read says; a reading then records it
/// as working, with its week.
/// NEGATIVE CONTROL: with `observed` never leaving `SignedOut`, the new sign-in stays refused:
/// `` left: SignedOut right: Unseen ``.
#[test]
fn a_login_file_written_since_the_sign_out_is_read_once_more() {
    journal::with_keyer(keyer, || {
        let fx = fixture("again");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let pass = fx.store.admit(&fx.place, OWNER, false, NOW).unwrap();
        fx.store.landed(pass, Landing::SignedOut, NOW + 4);
        // CONTROL: the same file, unwritten since, changes nothing.
        let (still, told) = fx.store.observe_with(&fx.seen(OWNER, NOW - 50), NOW + 100);
        assert_eq!(still.unwrap().auth, Auth::SignedOut);
        assert_eq!(told, Told::default());
        let (record, told) = fx.store.observe_with(&fx.seen(OWNER, NOW + 200), NOW + 210);
        let record = record.unwrap();
        assert_eq!((record.auth, record.read_at), (Auth::Unseen, None));
        assert_eq!(kinds(&told), [("auth", "auth-file", Some("unseen"))]);
        let pass = fx.store.admit(&fx.place, OWNER, false, NOW + 211).unwrap();
        let week = WeeklyFigure {
            used_x10: 460,
            resets_at: NOW + 3 * 86_400,
            at: NOW + 212,
        };
        let told = fx.store.landed_with(
            pass,
            Landing::Read {
                plan: Some("Muse Code Max Usage".into()),
                week: Some(week),
            },
            NOW + 213,
        );
        assert_eq!(
            kinds(&told),
            [("auth", "probe", Some("ok")), ("quota", "probe", None)]
        );
        let quota = told.notes[1];
        assert_eq!(
            (quota.window, quota.pct_x10, quota.reset, quota.res),
            (
                Some("7d"),
                Some(460),
                u64::try_from(NOW + 3 * 86_400).ok(),
                Some("exact")
            )
        );
        let record = fx.record(OWNER);
        assert_eq!(
            (record.auth, record.week, record.plan.as_deref()),
            (Auth::Ok, Some(week), Some("Muse Code Max Usage"))
        );
    });
}

/// A read that gave nothing and no sign-in words keeps the last good week, is on record across
/// sessions, and holds the next background read for an hour; a person's ask is not held.
/// NEGATIVE CONTROL: with `Record::standing` not reading `failed_at`, the next session reads
/// again at once: `` left: Ok(()) right: Err(Failed) ``.
#[test]
fn a_failed_read_holds_the_next_background_read() {
    journal::with_keyer(keyer, || {
        let fx = fixture("failed");
        fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
        let week = WeeklyFigure {
            used_x10: 300,
            resets_at: NOW + 86_400,
            at: NOW,
        };
        let pass = fx.store.admit(&fx.place, OWNER, false, NOW).unwrap();
        fx.store.landed(
            pass,
            Landing::Read {
                plan: None,
                week: Some(week),
            },
            NOW + 1,
        );
        let pass = fx
            .store
            .admit(&fx.place, OWNER, false, NOW + 1_000)
            .unwrap();
        let told = fx.store.landed_with(pass, Landing::Nothing, NOW + 1_005);
        assert_eq!(kinds(&told), [("error", "probe", Some("unreadable"))]);
        // A new session reads the same file's record.
        let record = Store::at(fx.store.dir.parent().unwrap(), ObservedCli::Muse)
            .record(&fx.place, OWNER)
            .unwrap();
        assert_eq!((record.auth, record.week), (Auth::Ok, Some(week)));
        assert_eq!(
            record.standing(false, NOW + 1_005 + 61),
            Err(Refusal::Failed)
        );
        assert_eq!(
            record.standing(true, NOW + 1_005 + 61),
            Ok(()),
            "a person's ask"
        );
        assert_eq!(
            record.standing(false, NOW + 1_005 + FAILED_HOLD_S),
            Ok(()),
            "CONTROL: past the hold"
        );
        // The ask is on record until a read is admitted for it.
        fx.store.ask(OWNER);
        assert!(fx.record(OWNER).asked);
        let pass = fx
            .store
            .admit(&fx.place, OWNER, false, NOW + 1_005 + 61)
            .unwrap();
        assert!(!fx.record(OWNER).asked);
        // A reading clears the failure.
        fx.store.landed(
            pass,
            Landing::Read {
                plan: None,
                week: None,
            },
            NOW + 1_100,
        );
        assert_eq!(fx.record(OWNER).failed_at, None);
        assert_eq!(
            fx.record(OWNER).week,
            Some(week),
            "no figure keeps the last one"
        );
    });
}

/// With no lab (no keyer), the record is kept all the same and nothing is told.
#[test]
fn without_a_journal_the_record_is_still_kept() {
    let fx = fixture("no-lab");
    fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
    let (_, told) = fx.store.observe_with(&fx.gone(), NOW + 10);
    assert_eq!(told, Told::default());
    assert_eq!(fx.record(OWNER).auth, Auth::SignedOut);
}

/// A record whose config home is gone is dropped at the next write, and a file that is not
/// this schema's reads as no records.
#[test]
fn a_record_of_a_removed_home_goes_and_a_foreign_file_reads_empty() {
    let fx = fixture("pruned");
    fx.store.observe(&fx.seen(OWNER, NOW - 50), NOW);
    let elsewhere = fx._tmp.path().join("other/.config/muse");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let there = Observation::Seen(Observed {
        cli: ObservedCli::Muse,
        email: OTHER.to_owned(),
        key: None,
        region: None,
        src: ObservedSrc::AuthFile,
        path: elsewhere.join("auth.json"),
        mtime: Some(NOW),
    });
    fx.store.observe(&there, NOW);
    assert_eq!(fx.store.kept().len(), 2);
    std::fs::remove_dir_all(fx.place.parent().unwrap()).unwrap();
    fx.store
        .observe(&there, NOW + 2 * crate::harness::observe::SEEN_REFRESH_S);
    assert_eq!(
        fx.store
            .kept()
            .iter()
            .map(|record| record.email.as_str())
            .collect::<Vec<_>>(),
        [OTHER]
    );
    std::fs::write(
        fx.store.file(),
        r#"{"schema":99,"logins":[{"email":"x@y.example"}]}"#,
    )
    .unwrap();
    assert_eq!(fx.store.kept(), Vec::new());
}
