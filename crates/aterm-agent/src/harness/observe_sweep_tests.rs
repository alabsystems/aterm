// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine sweep (`docs/DESIGN-account-discovery-2026-11-09.md`, step 4): every login place
//! is read, and an idle login is noted with no tab, once a day and when its file names another
//! account. Every login file here is a made-up fixture.

use super::*;
use aterm_tempfile::TempDir;
use std::fs;

const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";
const STEGO: &str = "stegosaurus.m9@proton.example";
const T0: i64 = 1_790_000_000;

fn home(tag: &str) -> TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-sweep-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn profile(email: &str) -> String {
    format!(
        r#"{{"oauthAccount":{{"emailAddress":"{email}","accountUuid":"11111111-2222-3333-4444-555555555555"}}}}"#
    )
}

fn muse_auth(email: &str) -> String {
    format!(r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{email}"}}}}}}"#)
}

/// A key per (realm, address), so one address at two providers is two accounts.
fn keyer(realm: &str, email: &str) -> Option<String> {
    let realm = match realm {
        "claude" => 'a',
        "meta" => 'e',
        _ => return None,
    };
    let who = match email {
        UNICORN => 1,
        BRONTO => 2,
        STEGO => 3,
        _ => return None,
    };
    Some(format!("ac-00000000000000{realm}{who}"))
}

/// A Claude Code roster whose default row is `m22` and whose row `work` keeps its login in `own`.
fn claude_roster(own: &Path) -> Roster {
    Roster::parse_for(
        Cli::Claude,
        &format!(
            "[[account]]\nlabel = \"m22\"\nclass = \"dedicated\"\nemail = \"{BRONTO}\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"{STEGO}\"\n\
             dir = \"{}\"\n",
            own.display()
        ),
    )
    .unwrap()
}

/// The sweep's places are each CLI's default home and each roster row's own directory, read
/// with no tab: Claude Code's default profile, a row's profile, and Muse Code's auth file; a
/// CLI with no roster and nothing in its home has no place, and a rostered row whose directory
/// holds nothing is a place that is unread.
/// NEGATIVE CONTROL: with `rows` leaving out each row's directory, the row's profile is read
/// from the default home: `` left: [… ("claude", Some("work"), Some("unicorn…")) …] ``, where the
/// right side names stegosaurus.
#[test]
fn places_are_each_default_home_and_each_roster_directory() {
    let home = home("places");
    let own = home.path().join("logins/work");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    write(&profile::profile_in_home(&own), &profile(STEGO));
    write(
        &home.path().join(".config/muse/auth.json"),
        &muse_auth(BRONTO),
    );
    let rosters = vec![(Cli::Claude, claude_roster(&own))];
    let found = places(&rosters, home.path(), &OnToken::none());
    let said: Vec<_> = found
        .iter()
        .map(|place| {
            (
                place.cli.id(),
                place.row.map(|row| row.label.as_str()),
                place.seen.email(),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            ("claude", Some("m22"), Some(UNICORN)),
            ("claude", Some("work"), Some(STEGO)),
            ("muse", None, Some(BRONTO)),
        ]
    );
    assert_eq!(found[0].path(), home.path().join(".claude.json"));
    // No roster at all: the default home is still a place, which no row names.
    let bare = places(&[], home.path(), &OnToken::none());
    assert_eq!(bare[0].row, None);
    assert_eq!(bare[0].seen.email(), Some(UNICORN));
    // A row whose directory holds no profile is a place that is unread.
    let gone = home.path().join("logins/gone");
    let rosters = vec![(Cli::Claude, claude_roster(&gone))];
    let found = places(&rosters, home.path(), &OnToken::none());
    assert_eq!(found[1].seen, Observation::Unread);
    assert_eq!(found[1].path(), profile::profile_in_home(&gone));
}

/// An idle login is noted on its place's first sighting as a `seen` that names no tab and no
/// harness; then not again within the day, at once when the file names another account, and
/// once the day is over. It is handed to the registry as idle with no tab at every look, due
/// or not: what the registry took is the sink's to keep.
/// NEGATIVE CONTROL: with `IDLE_REFRESH_S` at 0, the sweep an hour later notes again: `` left:
/// 1 right: 0 ``.
#[test]
fn an_idle_login_is_noted_once_a_day_and_when_its_file_changes() {
    let home = home("day");
    let file = home.path().join(".claude.json");
    write(&file, &profile(UNICORN));
    journal::with_keyer(keyer, || {
        let mut swept = Swept::default();
        let first = sweep(&places(&[], home.path(), &OnToken::none()), &mut swept, T0);
        assert_eq!(first.notes.len(), 1);
        let note = &first.notes[0];
        assert_eq!(
            (note.kind, note.cli, note.src, note.why),
            ("seen", "claude", "profile", Some("first"))
        );
        assert_eq!(note.tab, None);
        assert_eq!(note.harness, None);
        assert_eq!(
            note.account.map(|key| key.as_str().to_owned()).as_deref(),
            Some("ac-00000000000000a1")
        );
        assert_eq!(
            first.logins,
            [ObservedLogin {
                cli: Cli::Claude.into(),
                realm: "claude".to_owned(),
                email: UNICORN.to_owned(),
                state: LoginState::Idle,
                tab: None,
                at: T0.unsigned_abs(),
                src: ObservedSrc::Profile,
            }]
        );
        let hour = sweep(
            &places(&[], home.path(), &OnToken::none()),
            &mut swept,
            T0 + 3_600,
        );
        assert_eq!(hour.notes.len(), 0);
        assert_eq!(hour.logins.len(), 1, "handed at every look");
        assert_eq!(hour.logins[0].at, (T0 + 3_600).unsigned_abs());
        // A `/login` rewrote the file: another account sits in the place.
        write(&file, &profile(BRONTO));
        let moved = sweep(
            &places(&[], home.path(), &OnToken::none()),
            &mut swept,
            T0 + 3_700,
        );
        assert_eq!(moved.notes[0].why, Some("changed"));
        assert_eq!(moved.logins[0].email, BRONTO);
        let before = T0 + 3_700 + IDLE_REFRESH_S - 1;
        let quiet = sweep(
            &places(&[], home.path(), &OnToken::none()),
            &mut swept,
            before,
        );
        assert_eq!((quiet.notes.len(), quiet.of.len()), (0, 0));
        let day = sweep(
            &places(&[], home.path(), &OnToken::none()),
            &mut swept,
            before + 1,
        );
        assert_eq!(day.notes[0].why, Some("seen"));
        assert_eq!(day.logins.len(), 1);
    });
}

/// A place that names no account is not noted and keeps what it last said: a profile caught
/// signed out or gone between two sweeps is no change when its account is back. An account two
/// places name is noted once a sweep, and an account no key is made for is left due.
/// NEGATIVE CONTROL: with `sweep` noting an account whether or not this sweep noted it, the two
/// places make two notes: `` left: 2 right: 1 ``.
#[test]
fn a_place_with_no_account_keeps_its_last_and_one_account_is_noted_once() {
    let home = home("gaps");
    let own = home.path().join("logins/work");
    let file = home.path().join(".claude.json");
    write(&file, &profile(UNICORN));
    write(&profile::profile_in_home(&own), &profile(UNICORN));
    let rosters = vec![(Cli::Claude, claude_roster(&own))];
    journal::with_keyer(keyer, || {
        let mut swept = Swept::default();
        let both = sweep(
            &places(&rosters, home.path(), &OnToken::none()),
            &mut swept,
            T0,
        );
        assert_eq!(both.notes.len(), 1, "one account in two places");
        assert_eq!(both.logins.len(), 1);
        write(&file, "{}");
        let out = sweep(
            &places(&rosters, home.path(), &OnToken::none()),
            &mut swept,
            T0 + 60,
        );
        assert_eq!(out.notes.len(), 0, "signed out");
        assert_eq!(out.logins.len(), 1, "the row's directory still names it");
        fs::remove_file(&file).unwrap();
        let out = sweep(
            &places(&rosters, home.path(), &OnToken::none()),
            &mut swept,
            T0 + 120,
        );
        assert_eq!(out.notes.len(), 0, "unread");
        write(&file, &profile(UNICORN));
        let back = sweep(
            &places(&rosters, home.path(), &OnToken::none()),
            &mut swept,
            T0 + 180,
        );
        assert_eq!(back.notes.len(), 0);
    });
    // No keyer: nothing is noted, and the place stays due for the sweep that can key it.
    let mut swept = Swept::default();
    let unkeyed = sweep(
        &places(&rosters, home.path(), &OnToken::none()),
        &mut swept,
        T0,
    );
    assert_eq!(unkeyed, Sweep::default());
    assert_eq!(swept, Swept::default());
}

/// What the sweep noted is kept in one private file under the harness's state, so another
/// process's sweep, or this one's after a restart, notes nothing within the day; a file that
/// does not read is no memory, never a failure.
/// NEGATIVE CONTROL: with `run_through` not saving, the second run notes again: `` left: 2
/// right: 0 ``.
#[cfg(unix)]
#[test]
fn the_day_is_the_macs_not_a_processs() {
    let home = home("file");
    let state = home.path().join("state");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    write(
        &home.path().join(".config/muse/auth.json"),
        &muse_auth(UNICORN),
    );
    let run = |now| {
        let places = places(&[], home.path(), &OnToken::none());
        run_through(&state, &places, now, &mut |_| true, &mut |_| ())
    };
    journal::with_keyer(keyer, || {
        // One address at two providers is two accounts.
        assert_eq!(run(T0), 2);
        assert_eq!(run(T0 + 60), 0);
        let kept = load(&state);
        assert_eq!(Swept::parse(kept.render().as_bytes()), kept);
        assert_eq!(kept.places.len(), 2);
        assert!(
            kept.places
                .values()
                .all(|(account, at)| account.ends_with(UNICORN) && *at == T0)
        );
        assert_eq!(run(T0 + IDLE_REFRESH_S), 2);
    });
    assert_eq!(Swept::parse(b"not json"), Swept::default());
    assert_eq!(
        Swept::parse(br#"{"claude /x":{"account":7,"at":1}}"#),
        Swept::default()
    );
}

/// A note the journal did not take (it is off, has no spool, its spool is full) is not kept as
/// noted: its place is due at the next look with the same `why`, a first sighting still `first`
/// and a day's refresh still against the day it was last noted. A note the journal took in the
/// same sweep stays noted. The logins go to the registry at every look, whatever the journal
/// took and whether or not anything was due.
/// NEGATIVE CONTROL: with `run_through` keeping what it sent whether or not it was taken, the
/// file keeps both places: `` left: 2 right: 1 ``.
#[cfg(unix)]
#[test]
fn a_note_the_journal_did_not_take_is_due_again() {
    let home = home("ack");
    let state = home.path().join("state");
    write(&home.path().join(".claude.json"), &profile(UNICORN));
    write(
        &home.path().join(".config/muse/auth.json"),
        &muse_auth(BRONTO),
    );
    // One look through a journal that takes `takes`' notes: what it was asked to note, and how
    // many logins went to the registry.
    let look = |now: i64, takes: fn(&Note) -> bool| {
        let places = places(&[], home.path(), &OnToken::none());
        let (mut asked, mut handed) = (Vec::new(), 0);
        let taken = run_through(
            &state,
            &places,
            now,
            &mut |note| {
                asked.push((note.cli, note.why));
                takes(note)
            },
            &mut |_| handed += 1,
        );
        (taken, asked, handed)
    };
    journal::with_keyer(keyer, || {
        // The journal takes Muse Code's note and drops Claude Code's.
        let (taken, asked, handed) = look(T0, |note| note.cli == "muse");
        assert_eq!(taken, 1);
        assert_eq!(asked, [("claude", Some("first")), ("muse", Some("first"))]);
        assert_eq!(handed, 2);
        assert_eq!(load(&state).places.len(), 1, "only the taken note is kept");
        let (taken, asked, handed) = look(T0 + 900, |_| true);
        assert_eq!(taken, 1);
        assert_eq!(asked, [("claude", Some("first"))]);
        assert_eq!(handed, 2, "handed though only one note was due");
        let (taken, asked, handed) = look(T0 + 1_800, |_| true);
        assert_eq!((taken, asked.len(), handed), (0, 0, 2));
        // A refresh the journal drops stays due against the day it was last noted.
        let day = T0 + 900 + IDLE_REFRESH_S;
        let (taken, asked, _) = look(day, |_| false);
        assert_eq!(taken, 0);
        assert_eq!(asked, [("claude", Some("seen")), ("muse", Some("seen"))]);
        let kept = load(&state);
        assert!(kept.places.values().all(|(_, at)| *at < day));
        let (taken, asked, _) = look(day + 900, |_| true);
        assert_eq!((taken, asked.len()), (2, 2));
    });
}

/// A Claude Code row on its vault token is read as its token's account, the row's own, whatever
/// profile its directory still holds from an earlier `/login`: the stale profile's account is
/// neither noted nor handed to the registry. While the vault cannot be read no row's place is
/// read, and a token row the lab cannot key is unread.
/// NEGATIVE CONTROL: with `claude_places` reading a token row's directory as it does a stored
/// login's, the row names the stale account: `` left: Some("unicorn…") right: Some("stego…") ``.
#[test]
fn a_row_on_its_vault_token_is_its_tokens_account_not_its_stale_profile() {
    let home = home("token");
    let own = home.path().join("logins/work");
    write(&home.path().join(".claude.json"), &profile(BRONTO));
    // `work` is stegosaurus's row; its directory still holds unicorn's profile.
    write(&profile::profile_in_home(&own), &profile(UNICORN));
    let roster = claude_roster(&own);
    let on_token = OnToken::Rows(BTreeSet::from(["work".to_owned()]));
    journal::with_keyer(keyer, || {
        let found = claude_places(&roster, home.path(), &on_token);
        assert_eq!(
            found[0].seen.email(),
            Some(BRONTO),
            "a row on its stored login"
        );
        assert_eq!(found[0].src, ObservedSrc::Profile);
        assert_eq!(found[1].seen.email(), Some(STEGO));
        assert_eq!(found[1].src, ObservedSrc::VaultToken);
        assert_eq!(
            found[1].seen.seen().map(|seen| seen.src),
            Some(ObservedSrc::VaultToken)
        );
        let mut swept = Swept::default();
        let out = sweep(&found, &mut swept, T0);
        let emails: Vec<_> = out
            .logins
            .iter()
            .map(|login| login.email.as_str())
            .collect();
        assert_eq!(emails, [BRONTO, STEGO]);
        assert_eq!(
            out.notes.iter().map(|note| note.src).collect::<Vec<_>>(),
            ["profile", "vault-token"]
        );
        assert!(
            swept
                .places
                .values()
                .all(|(account, _)| !account.ends_with(UNICORN))
        );
        // On its stored login the same directory names the account it holds.
        let stored = claude_places(&roster, home.path(), &OnToken::none());
        assert_eq!(stored[1].seen.email(), Some(UNICORN));
        // The vault is not read: no row's place is.
        let unread = claude_places(&roster, home.path(), &OnToken::Unread);
        assert!(unread.iter().all(|place| place.seen == Observation::Unread));
        assert_eq!(sweep(&unread, &mut Swept::default(), T0), Sweep::default());
        // The default home no row names has no token and is read whatever the vault says.
        let none = Roster::default();
        let bare = claude_places(&none, home.path(), &OnToken::Unread);
        assert_eq!(bare[0].seen.email(), Some(BRONTO));
    });
    // No keyer: the lab cannot key the row, so its token entry is unknown and it is unread.
    let unkeyed = claude_places(&roster, home.path(), &on_token);
    assert_eq!(unkeyed[1].seen, Observation::Unread);
}
