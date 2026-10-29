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
    let aterm = paths.home.join(".aterm");
    let release_ring = aterm.join("release-ring");
    std::fs::create_dir_all(release_ring.join("roster")).unwrap();
    fsx::write_atomic(
        &release_ring.join("release-credentials.toml"),
        b"signing_key = \"c3ludGhldGlj\"\nmachine_id = \"m22\"\nmachine_roster = \"~/.aterm/release-ring/roster/aterm-machines.toml\"\n",
        0o600,
    )
    .unwrap();
    fsx::write_atomic(&release_ring.join("machine.toml"), b"id = \"m22\"\n", 0o600).unwrap();
    fsx::write_atomic(&aterm.join("machine.toml"), b"id = \"m99\"\n", 0o600).unwrap();
    let mut roster = format!(
        "schema = 1\nroster_seq = 1\nvalid_until = \"9999-12-31T00:00:00Z\"\nrevoked = [{}]\n",
        revoked
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for id in listed {
        roster.push_str(&format!(
            "\n[[machine]]\nid = \"{id}\"\npubkey = \"{id}-key\"\n"
        ));
    }
    fsx::write_atomic(
        &release_ring.join("roster/aterm-machines.toml"),
        roster.as_bytes(),
        0o644,
    )
    .unwrap();
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
            developer_dir,
            history: &mut history,
            store: &mut store,
        };
        rounds.step(paths, &configured(paths), &mut tools)
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
/// the same entry again runs nothing, in this subscriber or the next one, and a changed entry
/// runs notarytool again.
/// NEGATIVE CONTROL: a round without the digest check runs notarytool on every apply.
#[test]
fn an_entry_becomes_the_profile_once_and_an_unchanged_one_runs_nothing() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m3", "m1"], &[]);
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

    for mut rounds in [rounds, NotaryRounds::default()] {
        assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    }
    assert_eq!(script.runs(), 3, "an unchanged entry runs nothing");

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
    for (listed, revoked) in [(&["m22", "m3"][..], &[][..]), (&["m1"][..], &["m1"][..])] {
        ring(&paths, listed, revoked);
        assert_eq!(
            script.step(&mut rounds, &paths, &clt),
            Step::Waits(NOT_RELEASE_HOST),
            "listed {listed:?}, revoked {revoked:?}"
        );
    }
    ring(&paths, &["m1"], &[]);
    assert_eq!(
        script.step(&mut rounds, &paths, &no_clt),
        Step::Waits(WAITS_FOR_CLT)
    );
    assert_eq!(script.runs(), 0, "no xcrun");
}

/// The name 0.145 reads, `notary`, is never read.
#[test]
fn the_old_notary_name_is_never_read() {
    let (_root, paths) = lab(&[("notary", ENTRY_TEXT)]);
    ring(&paths, &["m1"], &[]);
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
    ring(&paths, &["m1"], &[]);
    let script = Script::answering(Vec::new());
    assert_eq!(
        script.step(&mut NotaryRounds::default(), &paths, &clt),
        Step::Quiet
    );
    assert_eq!(script.runs(), 0);
}

/// An unavailable history replaces no profile, and a store that does not authenticate is no
/// success. A failed entry is not tried again by the same subscriber (no notarytool run every
/// round), but is by the next one, and nothing is recorded as stored.
#[test]
fn a_failure_never_reads_as_stored_and_is_tried_once_per_subscriber() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m1"], &[]);
    let script = Script::answering(vec![Err(Error::new("Apple is unavailable"))]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(why)) if why.contains("Apple is unavailable")
    ));
    assert!(script.stored.borrow().is_empty(), "nothing replaced");
    assert_eq!(script.step(&mut rounds, &paths, &clt), Step::Quiet);
    assert_eq!(script.runs(), 1, "tried once by this subscriber");

    let script = Script::answering(vec![Ok(false), Ok(false)]);
    let mut rounds = NotaryRounds::default();
    assert!(matches!(
        script.step(&mut rounds, &paths, &clt),
        Step::Ran(Err(why)) if why.contains("did not authenticate")
    ));
    assert_eq!(applied(&paths), None, "no success recorded");
}

/// The subscriber's live round, in a test build, reads no developer directory: it says it waits
/// for the tools once, however many rounds pass, and runs nothing.
#[test]
fn the_live_round_says_a_wait_once_and_runs_nothing_in_a_test_build() {
    let (_root, paths) = lab(&[(ENTRY, ENTRY_TEXT)]);
    ring(&paths, &["m1"], &[]);
    let said = RefCell::new(Vec::new());
    let log = |line: &str| said.borrow_mut().push(line.to_owned());
    let mut rounds = NotaryRounds::default();
    for _ in 0..3 {
        rounds.after_round(&paths, &configured(&paths), &log);
    }
    assert_eq!(*said.borrow(), [WAITS_FOR_CLT]);
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

    let (_root, paths) = lab(&[]);
    ring(&paths, &["m22"], &[]);
    let elsewhere = paths.home.join("dist");
    std::fs::create_dir_all(&elsewhere).unwrap();
    fsx::write_atomic(
        &elsewhere.join("aterm-machines.toml"),
        b"schema = 1\n\n[[machine]]\nid = \"m1\"\npubkey = \"k\"\n",
        0o644,
    )
    .unwrap();
    let profile = |roster: &Path| {
        fsx::write_atomic(
            &paths.home.join(RELEASE_PROFILE),
            format!("machine_roster = \"{}\"\n", roster.display()).as_bytes(),
            0o600,
        )
        .unwrap();
    };
    profile(&elsewhere.join("aterm-machines.toml"));
    assert!(release_host(&paths.home, "m1"), "the profile's roster");
    assert!(!release_host(&paths.home, "m22"));
    profile(&elsewhere.join("missing.toml"));
    assert!(release_host(&paths.home, "m22"), "the vault's roster");
    assert!(!release_host(&paths.home, "m1"));
}

/// While an older vault has not yet rotated its path to `~/.aterm/release-ring/`, `release_host`
/// still reads the 0.145 `~/.aterm/release-credentials.toml` when the release-ring profile is
/// absent — and fails closed (never falling back) when the release-ring profile exists with
/// unsafe permissions.
/// NEGATIVE CONTROL: falling back on `Err` would read the legacy profile when the release-ring
/// profile is mode `0644`.
#[cfg(unix)]
#[test]
fn legacy_profile_is_read_only_when_release_ring_profile_is_absent_and_fails_closed_on_bad_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_root, paths) = lab(&[]);
    let aterm = paths.home.join(".aterm");
    std::fs::create_dir_all(aterm.join("roster")).unwrap();
    fsx::write_atomic(
        &paths.home.join(LEGACY_RELEASE_PROFILE),
        b"signing_key = \"c3ludGhldGlj\"\nmachine_id = \"m22\"\nmachine_roster = \"~/.aterm/roster/aterm-machines.toml\"\n",
        0o600,
    )
    .unwrap();
    fsx::write_atomic(
        &paths.home.join(LEGACY_VAULT_ROSTER),
        b"schema = 1\n\n[[machine]]\nid = \"m1\"\npubkey = \"m1-key\"\n",
        0o644,
    )
    .unwrap();
    assert!(
        release_host(&paths.home, "m1"),
        "absent release-ring profile falls back to 0.145 profile"
    );

    let ring_profile = paths.home.join(RELEASE_PROFILE);
    std::fs::create_dir_all(ring_profile.parent().unwrap()).unwrap();
    std::fs::write(&ring_profile, b"signing_key = \"c3ludGhldGlj\"\n").unwrap();
    std::fs::set_permissions(&ring_profile, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        !release_host(&paths.home, "m1"),
        "unsafe release-ring profile fails closed without falling back to legacy profile"
    );
}
