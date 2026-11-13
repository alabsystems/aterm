// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

const ENTRY_TEXT: &str = "apple_id = release@example.test\nteam_id = TEAM123\npassword = fake-app-specific-password\nuse = aterm\n";
const CHANGED_TEXT: &str = "apple_id = release@example.test\nteam_id = TEAM123\npassword = another-fake-password\nuse = aterm\n";

/// A vault whose only machine is `m1`, holding `entries` (name, text) in `[everyone]`.
fn lab(entries: &[(&str, &str)]) -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("alab.vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    for (name, text) in entries {
        publish(&paths, name, text);
    }
    (root, paths)
}

fn publish(paths: &Paths, name: &str, text: &str) {
    let admin = admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, name, text.as_bytes())
        .unwrap();
    admin_publish(paths, &admin, &doc).unwrap();
}

fn configured(paths: &Paths) -> Config {
    let config = config(paths).unwrap().unwrap();
    assert_eq!(config.host, "m1", "the vault name this Mac asked under");
    config
}

/// The release ring every vault member holds (`~/.aterm/release-ring/`, m22's `machine.toml`
/// and profile) alongside a separate host-owned `~/.aterm/machine.toml`, and a roster that names
/// `listed` and revokes `revoked`.
fn ring(paths: &Paths, listed: &[&str], revoked: &[&str]) {
    let _ = ring_with_master(paths, listed, revoked);
}

fn ring_with_master(
    paths: &Paths,
    listed: &[&str],
    revoked: &[&str],
) -> ring::signature::Ed25519KeyPair {
    use aterm_update_core::roster::{Machine, Roster, SUPPORTED_SCHEMA};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let rng = ring::rand::SystemRandom::new();
    let key = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
    let master_der = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let master = Ed25519KeyPair::from_pkcs8(master_der.as_ref()).unwrap();
    let aterm = paths.home.join(".aterm");
    let release_ring = aterm.join("release-ring");
    std::fs::create_dir_all(release_ring.join("roster")).unwrap();
    fsx::write_atomic(
        &release_ring.join("release-credentials.toml"),
        format!("signing_key = \"{}\"\nmachine_id = \"m22\"\nmachine_roster = \"~/.aterm/release-ring/roster/aterm-machines.toml\"\n", crate::b64::encode_padded(key.as_ref())).as_bytes(),
        0o600,
    )
    .unwrap();
    fsx::write_atomic(&release_ring.join("machine.key"), key.as_ref(), 0o600).unwrap();
    fsx::write_atomic(
        &aterm.join("test-master"),
        crate::b64::encode_padded(master.public_key().as_ref()).as_bytes(),
        0o600,
    )
    .unwrap();
    fsx::write_atomic(&release_ring.join("machine.toml"), b"id = \"m22\"\n", 0o600).unwrap();
    fsx::write_atomic(&aterm.join("machine.toml"), b"id = \"m99\"\n", 0o600).unwrap();
    let roster = Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: 1,
        valid_until: "9999-12-31T00:00:00Z".into(),
        revoked: revoked.iter().map(|id| (*id).to_owned()).collect(),
        machines: listed
            .iter()
            .map(|id| {
                let other_der = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
                let other = Ed25519KeyPair::from_pkcs8(other_der.as_ref()).unwrap();
                Machine {
                    id: (*id).to_owned(),
                    pubkey: crate::b64::encode_padded(if *id == "m22" {
                        pair.public_key().as_ref()
                    } else {
                        other.public_key().as_ref()
                    }),
                    added_at: "1970-01-01T00:00:00Z".into(),
                    not_after: None,
                }
            })
            .collect(),
    }
    .to_toml()
    .unwrap();
    fsx::write_atomic(
        &release_ring.join("roster/aterm-machines.toml"),
        roster.as_bytes(),
        0o644,
    )
    .unwrap();
    fsx::write_atomic(
        &release_ring.join("roster/aterm-machines.toml.sig"),
        master.sign(roster.as_bytes()).as_ref(),
        0o644,
    )
    .unwrap();
    master
}

#[test]
fn the_newest_signed_roster_wins_over_path_order_and_old_grants() {
    use aterm_update_core::roster::{Roster, verify_roster};
    let (_root, paths) = lab(&[]);
    let master = ring_with_master(&paths, &["m22"], &[]);
    let canonical = paths.home.join(VAULT_ROSTER);
    // A roster the profile names, at the 0.145 path: profile data, read because it is named.
    let legacy = paths.home.join(".aterm/roster/aterm-machines.toml");
    let raw = std::fs::read(&canonical).unwrap();
    let signature = std::fs::read(canonical.with_extension("toml.sig")).unwrap();
    fsx::ensure_private_dir(legacy.parent().unwrap()).unwrap();
    fsx::write_atomic(&legacy, &raw, 0o600).unwrap();
    fsx::write_atomic(&legacy.with_extension("toml.sig"), &signature, 0o600).unwrap();
    let profile = std::fs::read_to_string(paths.home.join(RELEASE_PROFILE))
        .unwrap()
        .replace(".aterm/release-ring/roster", ".aterm/roster");
    fsx::write_atomic(&paths.home.join(RELEASE_PROFILE), profile.as_bytes(), 0o600).unwrap();
    let anchor = std::fs::read_to_string(paths.home.join(".aterm/test-master")).unwrap();
    let verified = verify_roster(&[&anchor], raw, &signature).unwrap();
    let mut roster = Roster::parse(&verified).unwrap();
    let write = |path: &Path, roster: &Roster| {
        let raw = roster.to_toml().unwrap();
        fsx::write_atomic(path, raw.as_bytes(), 0o600).unwrap();
        fsx::write_atomic(
            &path.with_extension("toml.sig"),
            master.sign(raw.as_bytes()).as_ref(),
            0o600,
        )
        .unwrap();
    };
    roster.roster_seq = 2;
    write(&canonical, &roster);
    fsx::write_atomic(&paths.dir.join("release-ring-roster-floor"), b"2\n", 0o600).unwrap();
    assert!(
        test_authorized_ring(&paths),
        "the named old roster must not mask the current canonical authority"
    );
    roster.roster_seq = 3;
    roster.revoked.push("m22".into());
    write(&legacy, &roster);
    assert!(
        !test_authorized_ring(&paths),
        "a newer signed revocation must not fall back to the canonical old grant"
    );
    roster.revoked.clear();
    roster.valid_until = "1970-01-01T00:00:01Z".into();
    write(&legacy, &roster);
    assert!(
        !test_authorized_ring(&paths),
        "a newer signed expiry must not fall back to an old grant"
    );
}

fn test_authorized_ring(paths: &Paths) -> bool {
    let Ok(anchor) = std::fs::read_to_string(paths.home.join(".aterm/test-master")) else {
        return false;
    };
    authorized_ring_with(paths, &[&anchor], 1_000)
}

fn clt() -> Option<PathBuf> {
    Some(PathBuf::from(CLT_DEVELOPER))
}

fn no_clt() -> Option<PathBuf> {
    None
}

/// What the tools were asked, and the answers `history` gives in turn.
#[derive(Default)]
struct Script {
    answers: RefCell<VecDeque<crate::Result<bool>>>,
    histories: Cell<u32>,
    stored: RefCell<Vec<String>>,
    /// Whether the login keychain would ask for its password.
    locked: Cell<bool>,
}

impl Script {
    fn answering(answers: Vec<crate::Result<bool>>) -> Self {
        Self {
            answers: RefCell::new(answers.into()),
            ..Self::default()
        }
    }

    /// One round with `developer_dir`, the tools answering from this script.
    fn step(
        &self,
        rounds: &mut NotaryRounds,
        paths: &Paths,
        developer_dir: &dyn Fn() -> Option<PathBuf>,
    ) -> Step {
        self.with_tools(developer_dir, |tools| {
            rounds.step(paths, &configured(paths), tools)
        })
    }

    fn with_tools<T>(
        &self,
        developer_dir: &dyn Fn() -> Option<PathBuf>,
        run: impl FnOnce(&mut Tools<'_>) -> T,
    ) -> T {
        let mut history = || {
            self.histories.set(self.histories.get() + 1);
            self.answers
                .borrow_mut()
                .pop_front()
                .expect("notarytool ran more often than this test allows")
        };
        let mut store = |notary: &Notary| {
            assert_eq!(notary.apple_id.as_str(), "release@example.test");
            assert_eq!(notary.team_id.as_str(), "TEAM123");
            self.stored
                .borrow_mut()
                .push(notary.password.as_str().to_owned());
            Ok(())
        };
        let mut tools = Tools {
            authorized_ring: &test_authorized_ring,
            developer_dir,
            keychain_answers: &|| !self.locked.get(),
            history: &mut history,
            store: &mut store,
        };
        run(&mut tools)
    }

    /// The runs of notarytool so far: history checks plus stores.
    fn runs(&self) -> usize {
        self.histories.get() as usize + self.stored.borrow().len()
    }
}

#[test]
fn parser_binds_required_fields_and_rejects_bad_input() {
    let parsed = parse_notary(ENTRY_TEXT.as_bytes()).unwrap();
    assert_eq!(parsed.apple_id.as_str(), "release@example.test");
    assert_eq!(parsed.team_id.as_str(), "TEAM123");
    assert_eq!(parsed.password.as_str(), "fake-app-specific-password");
    for bad in [
        "apple_id = a\n",
        "apple_id = a\nteam_id = t\npassword = p\n",
        "apple_id = a\nteam_id = t\nuse = aterm\n",
        "apple_id = a\nteam_id = t\npassword = p\nuse = other\n",
        "apple_id = a\nteam_id = t\npassword = p\nuse = aterm\nextra = x\n",
        "apple_id = a\nteam_id = t\npassword = \"p\"\nuse = aterm\n",
    ] {
        assert!(parse_notary(bad.as_bytes()).is_err(), "{bad}");
    }
}

/// On a release host with the developer tools, a new entry is stored once and live-checked;
/// the same entry reuses that check in this subscriber, and a changed entry runs notarytool
/// again. A new subscriber always revalidates the profile.
#[test]
fn an_entry_reuses_only_this_subscribers_recent_successful_check() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Ok(false), Ok(true)]);
    let mut rounds = NotaryRounds::default();
    assert_eq!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok("notary profile stored and live-checked"))
    );
    assert_eq!(
        *script.stored.borrow(),
        ["fake-app-specific-password"],
        "stored once, from the entry"
    );
    assert_eq!(script.histories.get(), 2);

    assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    assert_eq!(script.runs(), 3, "an unchanged entry runs nothing");

    let restarted = Script::answering(vec![Ok(true)]);
    assert_eq!(
        restarted.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Ran(Ok("notary profile already authenticates"))
    );
    assert!(restarted.stored.borrow().is_empty());

    publish(&paths, ENTRY, CHANGED_TEXT);
    let script = Script::answering(vec![Ok(true)]);
    let mut rounds = NotaryRounds::default();
    assert_eq!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok("notary profile already authenticates"))
    );
    assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    assert_eq!(script.runs(), 1, "a changed entry is checked once");
}

#[test]
fn a_saved_digest_does_not_hide_a_missing_profile_after_restart() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let first = Script::answering(vec![Ok(false), Ok(true)]);
    assert!(matches!(
        first.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Ran(Ok(_))
    ));
    // A marker retained from an older build is evidence of a past success only.
    fsx::write_atomic(
        &paths.dir.join("release-notary.applied"),
        materialize::digest_hex(ENTRY_TEXT.as_bytes()).as_bytes(),
        0o600,
    )
    .unwrap();
    let restarted = Script::answering(vec![Ok(false), Ok(true)]);
    assert!(matches!(
        restarted.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Ran(Ok(_))
    ));
    assert_eq!(restarted.stored.borrow().len(), 1);
}

#[test]
fn an_expired_successful_check_recovers_a_missing_profile_without_restart() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Ok(true), Ok(false), Ok(true)]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok(_))
    ));
    let expired = rounds.checked.as_ref().unwrap().1 + NOTARY_RECHECK;
    assert!(matches!(
        script.with_tools(&clt, |tools| rounds.step_with_clock(
            &paths,
            tools,
            || expired,
            aterm_time::unix_secs
        )),
        Step::Ran(Ok(_))
    ));
    assert_eq!(script.stored.borrow().len(), 1);
}

#[test]
fn a_failed_rotation_does_not_reuse_an_earlier_success() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Ok(true), Ok(false), Ok(false), Ok(true)]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok(_))
    ));
    publish(&paths, ENTRY, CHANGED_TEXT);
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(_))
    ));
    publish(&paths, ENTRY, ENTRY_TEXT);
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok(_))
    ));
    assert_eq!(script.histories.get(), 4);
}

#[test]
fn explicit_notary_sync_uses_only_an_authorized_verified_entry() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    let none = Script::answering(Vec::new());
    assert!(
        none.with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap()
            .is_none()
    );
    assert_eq!(none.runs(), 0);
    ring(&paths, &["m22"], &["m22"]);
    assert!(
        none.with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap()
            .is_none()
    );
    assert_eq!(none.runs(), 0);
    ring(&paths, &["m22"], &[]);
    assert!(
        none.with_tools(&no_clt, |tools| sync_notary_with(&paths, tools))
            .is_err()
    );
    assert_eq!(none.runs(), 0);

    let available = Script::answering(vec![Ok(false), Ok(true), Ok(true)]);
    assert!(
        available
            .with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap()
            .is_some()
    );
    assert!(
        available
            .with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap()
            .is_some()
    );
    assert_eq!(available.stored.borrow().len(), 1);

    fsx::write_atomic(
        &paths.home.join(".secrets/release-notary"),
        b"untrusted",
        0o600,
    )
    .unwrap();
    let unread = Script::answering(Vec::new());
    assert!(matches!(
        unread.with_tools(&clt, |tools| sync_notary_with(&paths, tools)),
        Ok(None) | Err(_)
    ));
    assert_eq!(unread.runs(), 0);
}

#[test]
fn notary_authentication_cannot_install_a_superseded_entry_or_use_revoked_authority() {
    for revoke in [false, true] {
        let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
        ring(&paths, &["m22"], &[]);
        let mut history = || {
            if revoke {
                ring(&paths, &["m22"], &["m22"]);
            } else {
                publish(&paths, ENTRY, CHANGED_TEXT);
            }
            Ok(false)
        };
        let mut store = |_: &Notary| panic!("a stale credential was installed");
        let mut tools = Tools {
            authorized_ring: &test_authorized_ring,
            developer_dir: &clt,
            keychain_answers: &|| true,
            history: &mut history,
            store: &mut store,
        };
        assert!(sync_notary_with(&paths, &mut tools).is_err());
    }
}

/// No developer directory with notarytool, or a Mac that is no release host, runs nothing:
/// not the profile alone, not a roster that does not name this Mac's vault name, not one that
/// revoked it, and never `machine.toml`'s m22, which every member holds.
/// NEGATIVE CONTROL: a round without the developer-directory check runs xcrun with no tools.
#[test]
fn no_developer_tools_or_no_release_host_runs_nothing() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    let script = Script::answering(Vec::new());
    let mut rounds = NotaryRounds::default();
    assert_eq!(
        script.step(&mut rounds, &paths, &clt),
        Step::Waits(NOT_RELEASE_HOST),
        "no profile"
    );
    for (listed, revoked) in [(&["m1", "m3"][..], &[][..]), (&["m22"][..], &["m22"][..])] {
        ring(&paths, listed, revoked);
        assert_eq!(
            script.step(&mut rounds, &paths, &clt),
            Step::Waits(NOT_RELEASE_HOST),
            "listed {listed:?}, revoked {revoked:?}"
        );
    }
    ring(&paths, &["m22"], &[]);
    assert_eq!(
        script.step(&mut rounds, &paths, &no_clt),
        Step::Waits(no_developer_dir())
    );
    assert_eq!(script.runs(), 0, "no xcrun");
}

/// The name 0.145 reads, `notary`, is never read.
#[test]
fn the_old_notary_name_is_never_read() {
    let (_root, paths) = lab(&[("notary", ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(Vec::new());
    assert_eq!(
        script.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Quiet
    );
    assert_eq!(script.runs(), 0);
}

/// An absent entry runs nothing, release host or not.
#[test]
fn an_absent_entry_runs_nothing() {
    let (_root, paths) = lab(&[]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(Vec::new());
    assert_eq!(
        script.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Quiet
    );
    assert_eq!(script.runs(), 0);
}

/// An unavailable history replaces no profile, and a store that does not authenticate is no
/// success. A failed entry backs off, then recovers in the same subscriber without a restart.
#[test]
fn a_failure_never_reads_as_stored_and_retries_after_backoff() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Err(Error::new("Apple is unavailable"))]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(why)) if why.contains("Apple is unavailable")
    ));
    assert!(script.stored.borrow().is_empty(), "nothing replaced");
    assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    assert_eq!(script.runs(), 1, "tried once by this subscriber");
    let retried_at = Failed::read(&paths).unwrap().at + 61;
    let recovered = Script::answering(vec![Ok(true)]);
    assert!(matches!(
        recovered.with_tools(&clt, |tools| rounds.step_with_clock(
            &paths,
            tools,
            Instant::now,
            || Some(retried_at)
        )),
        Step::Ran(Ok(_))
    ));
    assert_eq!(recovered.runs(), 1);
    assert_eq!(Failed::read(&paths), None, "a success ends the backoff");
    let script = Script::answering(vec![Ok(false), Ok(false)]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(why)) if why.contains("did not authenticate")
    ));
    assert!(rounds.checked.is_none(), "no success recorded");
}

/// One round at wall second `at` of a subscriber that has just started.
fn restarted_at(script: &Script, paths: &Paths, at: u64) -> Step {
    script.with_tools(&clt, |tools| {
        NotaryRounds::default().step_with_clock(paths, tools, Instant::now, || Some(at))
    })
}

/// A failed attempt is on disk: a subscriber that restarts inside the backoff runs nothing,
/// and each further failure of the same entry doubles the wait. Control: the same restarted
/// subscriber runs notarytool once the wait has passed, and a success removes the record.
#[test]
fn a_failure_backs_off_across_restarts_and_longer_each_time() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let unavailable = || Err(Error::new("Apple is unavailable"));
    let script = Script::answering(vec![unavailable(), unavailable(), Ok(true)]);
    let start = 1_000_000;
    assert!(matches!(
        restarted_at(&script, &paths, start),
        Step::Ran(Err(_))
    ));
    let first = Failed::read(&paths).expect("the failure is on disk");
    assert_eq!(
        (first.at, first.failures, first.unanswered),
        (start, 1, false)
    );
    assert_eq!(restarted_at(&script, &paths, start + 59), Step::Quiet);
    assert_eq!(
        script.runs(),
        1,
        "a restart inside the backoff runs nothing"
    );
    assert!(matches!(
        restarted_at(&script, &paths, start + 60),
        Step::Ran(Err(_))
    ));
    assert_eq!(Failed::read(&paths).unwrap().failures, 2);
    assert_eq!(restarted_at(&script, &paths, start + 60 + 119), Step::Quiet);
    assert_eq!(script.runs(), 2, "the second failure waits twice as long");
    assert!(matches!(
        restarted_at(&script, &paths, start + 60 + 120),
        Step::Ran(Ok(_))
    ));
    assert_eq!(Failed::read(&paths), None);
    // However often it fails, the wait is at most a day.
    let often = Failed {
        failures: u32::MAX,
        ..first
    };
    assert_eq!(often.wait(), NOTARY_RETRY_MAX);
}

/// A run cut off at its deadline is the mark of a dialog nobody answered, not of a network:
/// it waits the whole day at once. Control: an ordinary failure at the same moment is tried
/// again after a minute.
#[test]
fn a_run_nobody_answered_waits_a_day() {
    let start = 1_000_000;
    let day = NOTARY_RETRY_MAX.as_secs();
    let cut_off = || {
        Err(Error::new(
            "notarytool authentication did not complete within its minute",
        ))
    };
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![cut_off(), Ok(true)]);
    assert!(matches!(
        restarted_at(&script, &paths, start),
        Step::Ran(Err(_))
    ));
    assert!(Failed::read(&paths).unwrap().unanswered);
    assert_eq!(restarted_at(&script, &paths, start + day - 1), Step::Quiet);
    assert_eq!(script.runs(), 1);
    assert!(matches!(
        restarted_at(&script, &paths, start + day),
        Step::Ran(Ok(_))
    ));

    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Err(Error::new("Apple is unavailable")), Ok(true)]);
    assert!(matches!(
        restarted_at(&script, &paths, start),
        Step::Ran(Err(_))
    ));
    assert!(matches!(
        restarted_at(&script, &paths, start + 60),
        Step::Ran(Ok(_))
    ));
}

/// While the login keychain would ask for its password, neither a round nor the explicit sync
/// runs notarytool: the round says what it waits for, the sync refuses. Control: the same
/// round with the keychain unlocked runs it.
#[test]
fn a_locked_login_keychain_runs_no_notarytool() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![Ok(true)]);
    script.locked.set(true);
    let mut rounds = NotaryRounds::default();
    assert_eq!(
        script.step(&mut rounds, &paths, &clt),
        Step::Waits(WAITS_FOR_KEYCHAIN)
    );
    assert_eq!(
        script
            .with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap_err()
            .to_string(),
        WAITS_FOR_KEYCHAIN
    );
    assert_eq!(script.runs(), 0);
    assert_eq!(Failed::read(&paths), None, "a wait is no failed attempt");
    script.locked.set(false);
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Ok(_))
    ));
    assert_eq!(script.runs(), 1);
}

/// The sync a release asks for always runs, whatever a round recorded, and what it finds is
/// what the next round reads: its failure backs the rounds off, its success ends a backoff.
#[test]
fn the_explicit_sync_ignores_a_backoff_and_records_its_own_outcome() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let script = Script::answering(vec![
        Err(Error::new("Apple is unavailable")),
        Err(Error::new("Apple is unavailable")),
        Ok(true),
    ]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(_))
    ));
    assert!(
        script
            .with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .is_err()
    );
    assert_eq!(script.runs(), 2, "the explicit sync ran inside the backoff");
    assert_eq!(Failed::read(&paths).unwrap().failures, 2);
    assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    assert!(
        script
            .with_tools(&clt, |tools| sync_notary_with(&paths, tools))
            .unwrap()
            .is_some()
    );
    assert_eq!(Failed::read(&paths), None);
}

/// The subscriber's live round never accepts a fixture's master as a release anchor; it says
/// it waits once, however many rounds pass, and runs no external tools.
#[test]
fn the_live_round_says_a_wait_once_and_runs_nothing_in_a_test_build() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m22"], &[]);
    let said = RefCell::new(Vec::new());
    let log = |line: &str| said.borrow_mut().push(line.to_owned());
    let mut rounds = NotaryRounds::default();
    for _ in 0..3 {
        rounds.after_round(&paths, &configured(&paths), &log);
    }
    assert_eq!(*said.borrow(), [NOT_RELEASE_HOST]);
}

/// xcrun's developer directory, read as xcrun selects it: `DEVELOPER_DIR`, else the choice
/// `xcode-select -s` made, else Xcode's, else the Command Line Tools'; only one holding
/// notarytool counts, and a choice that holds none is never passed over for another.
#[test]
fn the_developer_directory_is_the_one_xcrun_selects() {
    let xcode = Path::new(XCODE_DEVELOPER);
    let tools = Path::new(CLT_DEVELOPER);
    let read = |env: Option<&str>, link: Option<&str>, dirs: &[&Path], tools_in: &[&Path]| {
        developer_dir_with(
            env.map(OsString::from),
            &|path| {
                assert_eq!(path, Path::new(XCODE_SELECT_LINK));
                link.map(PathBuf::from)
            },
            &|path| dirs.contains(&path),
            &|path| {
                tools_in
                    .iter()
                    .any(|dir| path == dir.join("usr/bin/notarytool"))
            },
        )
    };
    assert_eq!(read(None, None, &[], &[]), None, "nothing installed");
    assert_eq!(read(None, None, &[], &[tools]), Some(tools.to_path_buf()));
    assert_eq!(
        read(None, None, &[xcode], &[xcode, tools]),
        Some(xcode.to_path_buf())
    );
    assert_eq!(
        read(None, None, &[xcode], &[tools]),
        None,
        "Xcode is the default, and holds none"
    );
    assert_eq!(
        read(
            None,
            Some("/Gone/Xcode.app/Contents/Developer"),
            &[],
            &[tools]
        ),
        None,
        "a choice that holds none"
    );
    assert_eq!(
        read(None, Some(CLT_DEVELOPER), &[xcode], &[xcode, tools]),
        Some(tools.to_path_buf())
    );
    assert_eq!(read(None, Some("relative"), &[], &[tools]), None);
    assert_eq!(
        read(Some("/Elsewhere"), None, &[], &[tools]),
        None,
        "DEVELOPER_DIR outranks the rest"
    );
    let bundle = Path::new("/Applications/Xcode-beta.app");
    let bundle_developer = bundle.join("Contents/Developer");
    assert_eq!(
        read(
            Some("/Applications/Xcode-beta.app"),
            None,
            &[bundle, &bundle_developer],
            &[&bundle_developer]
        ),
        Some(bundle_developer),
        "xcode-select(1) also accepts the application bundle"
    );
    assert_eq!(
        read(Some("/Gone/Xcode.app"), None, &[xcode], &[xcode, tools]),
        None,
        "an invalid selected bundle never falls back to other tools"
    );
    assert_eq!(
        read(Some(""), None, &[], &[tools]),
        Some(tools.to_path_buf()),
        "an empty DEVELOPER_DIR is unset"
    );
}

/// The roster is the one the profile names (a leading `~/` under the home), else the one the
/// vault puts beside it.
#[test]
fn the_roster_is_the_one_the_profile_names_else_the_vaults() {
    let home = Path::new("/Users//someone");
    assert_eq!(
        named_roster(
            "signing_key = \"x\"\nmachine_roster = \"~/r/m.toml\"\n",
            home
        ),
        Some(home.join("r/m.toml"))
    );
    assert_eq!(
        named_roster("machine_roster=\"/abs/m.toml\"", home),
        Some(PathBuf::from("/abs/m.toml"))
    );
    assert_eq!(named_roster("machine_rosterx = \"/a\"\n", home), None);
    assert_eq!(named_roster("signing_key = \"x\"\n", home), None);
    assert_eq!(
        named_roster(
            "signing_key = \"x\"\nmachine_roster = '~/r/m.toml' # inline comment\n[other]\nmachine_roster = \"/wrong.toml\"\n",
            home
        ),
        Some(home.join("r/m.toml"))
    );
    assert_eq!(
        named_roster("[other]\nmachine_roster = \"/wrong.toml\"\n", home),
        None
    );
    for bad in [
        "machine_roster = \"\"\n",
        "machine_roster = \"~/\"\n",
        "machine_roster = \"relative/m.toml\"\n",
        "machine_roster = 42\n",
        "machine_roster = \"/a\"\nmachine_roster = \"/b\"\n",
    ] {
        assert!(parse_named_roster(bad, home).is_err(), "{bad}");
    }

    let (_root, paths) = lab(&[]);
    ring(&paths, &["m22"], &[]);
    let elsewhere = paths.home.join("dist");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let original = std::fs::read_to_string(paths.home.join(RELEASE_PROFILE)).unwrap();
    for name in ["aterm-machines.toml", "aterm-machines.toml.sig"] {
        std::fs::copy(
            paths.home.join(".aterm/release-ring/roster").join(name),
            elsewhere.join(name),
        )
        .unwrap();
    }
    let profile = |roster: &Path| {
        let text = original
            .lines()
            .map(|line| {
                if line.starts_with("machine_roster") {
                    format!("machine_roster = \"{}\"", roster.display())
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fsx::write_atomic(&paths.home.join(RELEASE_PROFILE), text.as_bytes(), 0o600).unwrap();
    };
    profile(&elsewhere.join("aterm-machines.toml"));
    assert!(test_authorized_ring(&paths), "the profile's signed roster");
    profile(&elsewhere.join("missing.toml"));
    assert!(test_authorized_ring(&paths), "the vault's signed roster");
    profile(&elsewhere.join("aterm-machines.toml"));
    std::fs::write(elsewhere.join("aterm-machines.toml.sig"), [0; 64]).unwrap();
    assert!(
        !test_authorized_ring(&paths),
        "no fallback from an invalid named roster"
    );

    let commented = original
        .lines()
        .map(|line| {
            if line.starts_with("machine_roster") {
                format!(
                    "machine_roster = '{}' # inline comment",
                    elsewhere.join("aterm-machines.toml").display()
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fsx::write_atomic(
        &paths.home.join(RELEASE_PROFILE),
        commented.as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(
        !test_authorized_ring(&paths),
        "single-quoted or commented machine_roster must not drop the invalid named roster"
    );

    let relative = original
        .lines()
        .map(|line| {
            if line.starts_with("machine_roster") {
                "machine_roster = \"relative/aterm-machines.toml\"".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fsx::write_atomic(
        &paths.home.join(RELEASE_PROFILE),
        relative.as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(
        !test_authorized_ring(&paths),
        "relative machine_roster fails closed instead of falling back to canonical roster"
    );
}

/// THE 0.145 COPIES UNDER `~/.aterm/` ARE NEVER READ: duplicates of the release ring, recorded for
/// retirement (ATERM-KI-1112-980). With only them on disk this Mac holds no ring; the canonical
/// copies alone authorize it (the control).
#[cfg(unix)]
#[test]
fn the_legacy_ring_copies_are_never_read() {
    let (_root, paths) = lab(&[]);
    ring(&paths, &["m22"], &[]);
    assert!(
        test_authorized_ring(&paths),
        "CONTROL: the canonical ring authorizes"
    );
    let aterm = paths.home.join(".aterm");
    std::fs::create_dir_all(aterm.join("roster")).unwrap();
    let profile = std::fs::read_to_string(paths.home.join(RELEASE_PROFILE))
        .unwrap()
        .replace(".aterm/release-ring/roster", ".aterm/roster");
    fsx::write_atomic(
        &aterm.join("release-credentials.toml"),
        profile.as_bytes(),
        0o600,
    )
    .unwrap();
    std::fs::copy(
        aterm.join("release-ring/machine.key"),
        aterm.join("machine.key"),
    )
    .unwrap();
    for name in ["aterm-machines.toml", "aterm-machines.toml.sig"] {
        std::fs::copy(
            aterm.join("release-ring/roster").join(name),
            aterm.join("roster").join(name),
        )
        .unwrap();
    }
    std::fs::remove_dir_all(aterm.join("release-ring")).unwrap();
    assert!(
        !test_authorized_ring(&paths),
        "the 0.145 copies alone authorize nothing"
    );
}
