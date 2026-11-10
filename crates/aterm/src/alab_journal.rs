// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab journal's glue (`docs/DESIGN-alab-lab-model-2026-11-01.md` §5, §8, step 8): the
//! harness, which does not link `aterm-db`, keys its accounts and notes what it sees through
//! `aterm_agent::harness::journal::Sink`; this is that sink. An account's key is
//! `aterm_db::typed::keys::account_pk` of the claims realm and email; a note becomes an
//! `aterm_db::alab::journal::Event` (every word checked again on the way) and one bounded
//! append to this user's spool, which the Commons owner's flusher publishes. The same sink fills
//! step 9's provider-read door (`aterm_agent::harness::provider_read`), so each usage read is a
//! `probe` of its account. The once-per-Mac backfill runs only in a process that starts a Commons
//! runtime (a window, a session lane, `aterm node serve`), never in a short command, and is
//! marked done only once every note is in the spool. The flusher's holdings report (what this Mac
//! runs that it does not prefer, published as `place` rows) is filled here too, from the lab this
//! process holds and the rosters beside `aterm.toml` ([`holdings_in`]); a process that holds no
//! lab (every test, a debug build) reports nothing, so no place row is written.

use aterm_agent::harness::{
    accounts::{self, Cli, Roster},
    alab_roster::{self, Lab},
    journal::{self, Note, Sink},
    provider_read::{self, ProbeEvent},
};
use aterm_db::alab::journal::{
    Appended, Event, Kind, Pk, Publishing, Res, Source, Spool, Window, Word,
    place::{self, Holding, Why},
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct Glue {
    /// This user's spool, its path made once (`Spool::local`).
    spool: Option<Spool>,
    /// Whether the journal is on (`[alab] journal`): a note made while it is off is never
    /// appended, so turning it on again publishes nothing from the time it was off.
    publishing: Publishing,
}

impl Sink for Glue {
    fn account_pk(&self, realm: &str, email: &str) -> Option<String> {
        keyer(realm, email)
    }

    fn note(&self, note: &Note) {
        if !self.publishing.now() {
            return;
        }
        if let (Some(spool), Some(event)) = (&self.spool, event(note)) {
            // The spool drops what it cannot take; a note never waits or fails its caller.
            let _ = spool.append(&event);
        }
    }

    fn observed(&self, login: &journal::ObservedLogin) {
        // The registry's `account-obs` rows: a thread of its own, installed only where this
        // process holds a lab (`crate::alab_observed`).
        crate::alab_observed::take_up(login);
    }
}

/// The journal event a harness note is, when every word is in the journal's vocabulary.
fn event(note: &Note) -> Option<Event> {
    fn word<T>(word: Option<&str>, parse: impl Fn(&str) -> Option<T>) -> Option<Option<T>> {
        match word {
            None => Some(None),
            Some(word) => parse(word).map(Some),
        }
    }
    // Only a realm-attributed probe names no account (`journal::probe_note`).
    if note.account.is_none() && (note.kind != "probe" || note.sub.is_some()) {
        return None;
    }
    Some(Event {
        t: (note.t > 0).then_some(note.t)?,
        kind: Kind::parse(note.kind)?,
        account: word(note.account.as_ref().map(journal::Key::as_str), Pk::new)?,
        cli: Word::new(note.cli)?,
        sub: word(note.sub.as_ref().map(journal::SubKey::as_str), Word::new)?,
        window: word(note.window, Window::parse)?,
        pct_x10: match note.pct_x10 {
            None => None,
            Some(p) if p <= 1000 => Some(p),
            Some(_) => return None,
        },
        reset: note.reset,
        res: word(note.res, Res::parse)?,
        until: note.until,
        plan: None,
        src: Source::parse(note.src)?,
        why: word(note.why, Word::new)?,
        to: word(note.to.as_ref().map(journal::Key::as_str), Pk::new)?,
        tab: word(note.tab.as_ref().map(journal::Tab::as_str), Word::new)?,
        harness: word(note.harness, Word::new)?,
    })
    // Only a kind about one tab's run names a tab or a harness, as every reader admits it.
    .filter(|event| (event.tab.is_none() && event.harness.is_none()) || event.kind.names_tab())
}

static GLUE: OnceLock<Glue> = OnceLock::new();

fn glue() -> &'static Glue {
    GLUE.get_or_init(|| Glue {
        spool: Spool::local(),
        publishing: Publishing::new(aterm_types::dirs::aterm_config_path()),
    })
}

/// Install the journal sink for this process, fill the provider-read door with it, and give the
/// flusher this Mac's holdings.
pub(super) fn install() {
    journal::install(glue());
    let _ = provider_read::install(Box::new(probe));
    let _ = place::install_holdings(Box::new(|| {
        let lab = alab_roster::current()?;
        holdings_in(&lab, &aterm_types::dirs::aterm_config_path()?)
    }));
}

/// What this Mac runs that it does not prefer (`alab_roster::holdings`), over every pooled CLI's
/// roster beside the `aterm.toml` at `config`: `None` where the lab names no machine for this Mac
/// or a roster that is there does not read, so a passing failure ends no place row. A CLI with no
/// roster holds nothing.
fn holdings_in(lab: &Lab, config: &Path) -> Option<Vec<Holding>> {
    lab.me.as_ref()?;
    let mut out = Vec::new();
    for cli in Cli::ALL {
        let Some(path) = accounts::roster_beside_for(config, cli) else {
            continue;
        };
        let roster = Roster::load_for(cli, &path).ok()?;
        for (pk, why) in alab_roster::holdings(lab, cli, &roster) {
            out.push(Holding {
                account: Pk::new(&pk)?,
                why: Why::parse(why)?,
            });
        }
    }
    Some(out)
}

/// One provider read the door reported, noted as its account's `probe`
/// ([`journal::probe_note`]); nothing where the account cannot be keyed.
fn probe(event: &ProbeEvent) {
    let config = aterm_types::dirs::aterm_config_path();
    let roster = |cli: Cli| roster_of(config.as_deref(), cli);
    if let Some(note) = journal::probe_note(&roster, &keyer, event) {
        journal::note(&note);
    }
}

/// The user's home, when it is absolute.
fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// `cli`'s roster beside the aterm config at `config`.
fn roster_of(config: Option<&Path>, cli: Cli) -> Option<Roster> {
    let path = accounts::roster_beside_for(config?, cli)?;
    Roster::load_for(cli, &path).ok()
}

/// The lab key of a claims realm and email (design §5).
fn keyer(realm: &str, email: &str) -> Option<String> {
    aterm_db::typed::keys::account_pk(realm, email).ok()
}

/// Start the once-per-Mac backfill on a thread of its own when its marker is absent (one `stat`
/// otherwise). Called where a Commons runtime started, a process that lives on: a short command
/// would load every CLI's store and ledger at each start and could end before the notes are in.
pub(super) fn start_backfill() {
    let Some(spool) = glue().spool.clone() else {
        return;
    };
    let Ok(state) = aterm_agent::harness::cli::harness_state_dir() else {
        return;
    };
    if journal::backfilled(&state) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("alab-backfill".into())
        .spawn(move || backfill(&spool, &state));
}

/// Note what this Mac's local ledgers and account states say, once.
fn backfill(spool: &Spool, state: &Path) {
    let Some(home) = user_home() else {
        return;
    };
    let config = aterm_types::dirs::aterm_config_path();
    let now = aterm_agent::supervise::limit::unix_now();
    if let Err(error) = backfill_at(spool, state, &home, config.as_deref(), now) {
        aterm_log::debug!("alab backfill left for the next start: {error}");
    }
}

/// Run the backfill at `now` when `state` already exists and the clock is past the epoch; leave
/// it unmarked otherwise so the next start looks again once a harness has run. With the journal
/// off (`[alab] journal = false`, `aterm_db::alab::journal::publishing`) it is marked done and
/// notes nothing: the owner turned publishing off for exactly those events, so turning it on
/// again publishes none of them.
fn backfill_at(
    spool: &Spool,
    state: &Path,
    home: &Path,
    config: Option<&Path>,
    now: i64,
) -> io::Result<()> {
    if !state.is_dir() || now <= 0 {
        return Ok(());
    }
    if !aterm_db::alab::journal::publishing(config) {
        return journal::mark_backfilled(state);
    }
    let roster = |cli: Cli| roster_of(config, cli);
    let notes = journal::backfill(state, home, &roster, &keyer, now);
    record(spool, state, &notes)
}

/// Append each of `notes` to `spool`, then mark the backfill done: an append that fails, or a
/// full spool, leaves it unmarked for the next start. A note outside the journal's words, or one
/// no event can hold, is skipped: it never fits, so it never holds the marker back.
fn record(spool: &Spool, state: &Path, notes: &[Note]) -> io::Result<()> {
    for event in notes.iter().filter_map(event) {
        match spool.append(&event)? {
            Appended::Written | Appended::Refused => {}
            Appended::Full => return Err(io::Error::other("the journal spool is full")),
        }
    }
    journal::mark_backfilled(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word a harness note can carry is a word of the journal's vocabulary and the widest
    /// note of each shape the harness makes fits one event, so no note is dropped at the glue;
    /// and the two realm tables agree: the harness's claims key and the lab's account key name
    /// one account (design §5).
    /// NEGATIVE CONTROL: with `event` dropping the other account (`to: None`), the switch names
    /// no account: `` called `Option::unwrap()` on a `None` value ``.
    #[test]
    fn every_harness_word_is_a_journal_word() {
        let account = journal::Key::new("ac-0123456789abcdef").unwrap();
        for kind in journal::KINDS {
            assert!(
                Kind::parse(kind).is_some(),
                "a harness kind the journal does not take: {kind:?}"
            );
        }
        for window in journal::WINDOWS {
            assert!(Window::parse(window).is_some(), "{window}");
        }
        for src in journal::SOURCES {
            assert!(Source::parse(src).is_some(), "{src}");
        }
        for res in journal::RESES {
            assert!(Res::parse(res).is_some(), "{res}");
        }
        for why in journal::WHYS {
            assert!(Word::<16>::new(why).is_some(), "{why}");
        }
        for cli in Cli::ALL {
            assert!(Word::<10>::new(cli.id()).is_some(), "{}", cli.id());
        }
        // The widest note of each shape the harness makes (`observe`): a reading, a wall, a
        // switch, an error with its reason. Each fits one event (192 B).
        let base = Note {
            t: 9_999_999_999,
            kind: "quota",
            account: Some(account),
            cli: "claude",
            sub: None,
            window: Some("7d-model"),
            pct_x10: Some(1000),
            reset: Some(9_999_999_999),
            res: Some("exact"),
            until: None,
            src: "session-usage",
            why: None,
            to: None,
            tab: None,
            harness: None,
        };
        let shapes = [
            base,
            Note {
                kind: "wall",
                pct_x10: None,
                reset: None,
                res: None,
                until: Some(9_999_999_999),
                src: "cli-output",
                ..base
            },
            Note {
                kind: "switch",
                window: None,
                pct_x10: None,
                reset: None,
                res: None,
                src: "cli-output",
                to: journal::Key::new("ac-fedcba9876543210"),
                ..base
            },
            Note {
                kind: "sign-in-failed",
                window: None,
                pct_x10: None,
                reset: None,
                res: None,
                src: "signin-flow",
                why: Some("reset-failed"),
                ..base
            },
            // A tab's sighting, with its tab and harness and the believed account of a
            // wrong-account beside it.
            journal::seen_note(
                Cli::Codex,
                account,
                journal::Tab::of("machine", "s-41a2"),
                "codex-alab-agent",
                journal::ObservedSrc::VaultToken,
                journal::SeenWhy::Changed,
                9_999_999_999,
            ),
            journal::wrong_account_note(
                Cli::Claude,
                account,
                journal::Key::new("ac-fedcba9876543210"),
                journal::Tab::of("machine", "s-41a2"),
                journal::ObservedSrc::Profile,
                9_999_999_999,
            ),
        ];
        let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
        for note in &shapes {
            let event = event(note).unwrap();
            assert_eq!(event.kind.word(), note.kind);
            assert!(event.encode(&mut buf).is_some(), "{note:?} fits an event");
        }
        let switch = event(&shapes[2]).unwrap();
        assert_eq!(switch.kind, Kind::Switch);
        assert_eq!(switch.to.unwrap().as_str(), "ac-fedcba9876543210");
        for realm in [
            "claude",
            "anthropic",
            "codex",
            "openai",
            "grok",
            "xai",
            "kimi",
            "kimi:mainland-cn",
        ] {
            let lease =
                aterm_agent::harness::claims::LeaseKey::new(realm, "Ann@Example.invalid").unwrap();
            assert_eq!(
                aterm_db::typed::keys::account_pk(&lease.pool, &lease.email).unwrap(),
                aterm_db::typed::keys::account_pk(realm, "ann@example.invalid").unwrap(),
                "{realm}"
            );
        }
    }

    /// The backfill is marked done only once its notes are in the spool: a spool that cannot
    /// be written leaves it unmarked, so the next start backfills again and no note is lost.
    /// NEGATIVE CONTROL: with `record` marking first (`journal::mark_backfilled(state)?` before
    /// the appends), the failed backfill is marked done: `` the failed backfill is not marked ``.
    #[test]
    fn a_backfill_is_marked_only_once_its_notes_are_spooled() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().join("harness");
        std::fs::create_dir_all(&state).unwrap();
        let blocked = scratch.path().join("alab");
        std::fs::write(&blocked, b"").unwrap();
        let spool = Spool::at(blocked.clone());
        let note = Note {
            t: 1_800_000_000,
            kind: "signed-in",
            account: journal::Key::new("ac-0123456789abcdef"),
            cli: "claude",
            sub: None,
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            until: None,
            src: "cli-output",
            why: None,
            to: None,
            tab: None,
            harness: None,
        };
        assert!(record(&spool, &state, &[note]).is_err());
        assert!(
            !journal::backfilled(&state),
            "the failed backfill is not marked"
        );
        std::fs::remove_file(&blocked).unwrap();
        record(&spool, &state, &[note]).unwrap();
        assert!(journal::backfilled(&state));
        let spooled =
            std::fs::read_to_string(blocked.join(aterm_db::alab::journal::spool::SPOOL)).unwrap();
        assert_eq!(spooled.lines().count(), 1);
    }

    /// Before any harness has created its state directory, or when the clock is not yet past the
    /// Unix epoch, `backfill_at` leaves the backfill unmarked instead of creating `<state>/alab`
    /// and sealing an empty pass; and `event` rejects zero-timestamp or over-100% quota notes
    /// rather than stripping `pct_x10`.
    /// NEGATIVE CONTROL: with `backfill` calling `record` unconditionally on `!state.is_dir()`,
    /// `journal::mark_backfilled` creates `state` and marks it done before any harness runs.
    #[test]
    fn missing_harness_state_or_pre_epoch_clock_leaves_backfill_unmarked_and_bad_notes_drop() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let spool = Spool::at(scratch.path().join("alab"));
        let missing_state = scratch.path().join("no-harness-yet");
        backfill_at(&spool, &missing_state, scratch.path(), None, 1_800_000_000).unwrap();
        assert!(!missing_state.exists());
        assert!(!journal::backfilled(&missing_state));

        let present_state = scratch.path().join("harness");
        std::fs::create_dir_all(&present_state).unwrap();
        backfill_at(&spool, &present_state, scratch.path(), None, 0).unwrap();
        assert!(!journal::backfilled(&present_state));

        backfill_at(&spool, &present_state, scratch.path(), None, 1_800_000_000).unwrap();
        assert!(journal::backfilled(&present_state));

        let account = journal::Key::new("ac-0123456789abcdef").unwrap();
        let zero_time = Note {
            t: 0,
            kind: "signed-in",
            account: Some(account),
            cli: "claude",
            sub: None,
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            until: None,
            src: "cli-output",
            why: None,
            to: None,
            tab: None,
            harness: None,
        };
        assert!(event(&zero_time).is_none());
        let over_pct = Note {
            t: 1_800_000_000,
            kind: "quota",
            window: Some("5h"),
            pct_x10: Some(1001),
            ..zero_time
        };
        assert!(event(&over_pct).is_none());
    }

    /// A note's workspace and a realm-only probe reach the journal as they are: the workspace as
    /// the event's `s`, an agy read with no login as a `probe` naming no account; a note of any
    /// other kind without an account is dropped at the glue, as every reader would drop it.
    /// NEGATIVE CONTROL: with `event` mapping no workspace (`sub: None`), the reading's `s` is
    /// lost: `` left: None right: Some(0123456789ab) ``.
    #[test]
    fn a_workspace_and_a_realm_only_probe_reach_the_journal() {
        let reading = Note {
            t: 1_800_000_000,
            kind: "quota",
            account: journal::Key::new("ac-0123456789abcdef"),
            cli: "codex",
            sub: journal::SubKey::new("0123456789ab"),
            window: Some("7d"),
            pct_x10: Some(250),
            reset: Some(1_800_003_600),
            res: Some("exact"),
            until: None,
            src: "rollout",
            why: None,
            to: None,
            tab: None,
            harness: None,
        };
        assert_eq!(event(&reading).unwrap().sub, Word::new("0123456789ab"));
        let probe = Note {
            kind: "probe",
            account: None,
            cli: "agy",
            sub: None,
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            src: "probe",
            why: Some("launching"),
            ..reading
        };
        let mut buf = [0; aterm_db::alab::journal::event::MAX_EVENT];
        let noted = event(&probe).unwrap();
        let len = noted.encode(&mut buf).unwrap();
        let line = &buf[..len];
        assert_eq!(Event::from_line(line), Some(noted), "every reader takes it");
        assert_eq!(noted.account, None);
        let signin = Note {
            kind: "signed-in",
            src: "signin-flow",
            why: None,
            ..probe
        };
        assert_eq!(event(&signin), None);
    }

    /// A note made while `[alab] journal = false` is never spooled, so no pass can publish it
    /// once the journal is on again; one made after it is turned on is.
    /// NEGATIVE CONTROL: with `Glue::note` not asking `publishing` (its check removed), the
    /// sign-in noted while off is spooled: `` nothing is spooled while off ``.
    #[test]
    fn a_note_made_while_the_journal_is_off_is_never_spooled() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
        let glue = Glue {
            spool: Some(Spool::at(scratch.path().join("alab"))),
            publishing: Publishing::new(Some(config.clone())),
        };
        let note = Note {
            t: 1_800_000_000,
            kind: "signed-in",
            account: journal::Key::new("ac-0123456789abcdef"),
            cli: "claude",
            sub: None,
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            until: None,
            src: "signin-flow",
            why: None,
            to: None,
            tab: None,
            harness: None,
        };
        let spooled = scratch
            .path()
            .join("alab")
            .join(aterm_db::alab::journal::spool::SPOOL);
        glue.note(&note);
        assert!(!spooled.exists(), "nothing is spooled while off");
        std::fs::write(&config, "[alab]\njournal = true\n").unwrap();
        glue.note(&note);
        assert!(spooled.exists(), "on again, the note is spooled");
    }

    /// With `[alab] journal = false` the backfill is marked done and notes nothing, so turning
    /// the journal on again publishes nothing from before.
    /// NEGATIVE CONTROL: with `backfill_at` not reading `publishing` (its check removed), the
    /// ledger's sign-in is spooled: `` nothing is spooled while off ``.
    #[test]
    fn a_backfill_with_the_journal_off_is_marked_and_notes_nothing() {
        use aterm_agent::harness::accounts::{Event as Row, Kind as RowKind, Store};
        let scratch = aterm_tempfile::tempdir().unwrap();
        let state = scratch.path().join("harness");
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let now = 1_800_000_000;
        let store = Store::at_for(&state, Cli::Claude, &home);
        store
            .append(&[Row::new(now - 600, RowKind::SignedIn, "ann")])
            .unwrap();
        let mut keyed = accounts::State::default();
        keyed.accounts.insert(
            "ann".into(),
            accounts::Account {
                pk: Some("ac-0123456789abcdef".into()),
                ..accounts::Account::default()
            },
        );
        aterm_uds::private_file::write_private(
            store.state_file().parent().unwrap(),
            "state.json",
            (keyed.to_json() + "\n").as_bytes(),
        )
        .unwrap();
        let config = scratch.path().join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
        let spool = Spool::at(scratch.path().join("alab"));
        backfill_at(&spool, &state, &home, Some(&config), now).unwrap();
        assert!(journal::backfilled(&state));
        assert!(
            !spool
                .dir()
                .join(aterm_db::alab::journal::spool::SPOOL)
                .exists(),
            "nothing is spooled while off"
        );
        // On, the same backfill notes the sign-in: the check above is what kept it out.
        let on = scratch.path().join("on");
        std::fs::create_dir_all(&on).unwrap();
        std::fs::rename(state.join("alab"), on.join("mark")).unwrap();
        backfill_at(&spool, &state, &home, None, now).unwrap();
        assert!(
            spool
                .dir()
                .join(aterm_db::alab::journal::spool::SPOOL)
                .exists()
        );
    }

    /// The flusher's holdings come from every pooled CLI's roster beside `aterm.toml`, keyed by
    /// the lab's account key with the harness's reason; a lab that names no machine for this Mac,
    /// or a roster that is there and does not read, reports nothing known (`None`), so no place
    /// row ends on a passing failure; a CLI with no roster holds nothing.
    /// NEGATIVE CONTROL: with `holdings_in` skipping a roster that does not read (`continue`
    /// instead of `?`), the broken roster reports `Some([])` and every place row would end
    /// (`left: Some([]) right: None`).
    #[test]
    fn holdings_come_from_the_rosters_beside_the_config() {
        use aterm_agent::harness::{
            accounts::{Row, Vendor},
            alab_roster::{LabAccount, MachineRef},
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        let email = "ann@example.test";
        let key = aterm_db::typed::keys::account_pk("claude", email).unwrap();
        let lab = Lab {
            me: Some(MachineRef::from_registry("fixture-m22", Some("m22".into()))),
            accounts: vec![LabAccount {
                pk: key.clone(),
                cli: Cli::Claude,
                vendor: Vendor::default(),
                label: "claude-aaaaaa".into(),
                legacy_label: None,
                email: email.into(),
                identity: None,
                browser: None,
                joinable: true,
                preferred: vec![(
                    MachineRef::from_registry("fixture-m3", Some("m3".into())),
                    Some(1),
                )],
            }],
            ..Lab::default()
        };
        assert_eq!(holdings_in(&lab, &config), Some(Vec::new()), "no roster");
        let roster = accounts::roster_beside_for(&config, Cli::Claude).unwrap();
        let row = Row {
            label: "claude-aaaaaa".into(),
            dir: Some("/scratch/ann".into()),
            email: Some(email.into()),
            ..Row::default()
        };
        std::fs::write(
            &roster,
            accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            holdings_in(&lab, &config),
            Some(vec![Holding {
                account: Pk::new(&key).unwrap(),
                why: Why::Borrow,
            }])
        );
        let unnamed = Lab {
            me: None,
            ..lab.clone()
        };
        assert_eq!(holdings_in(&unnamed, &config), None);
        std::fs::write(&roster, "[[account]]\nlabel = 7\n").unwrap();
        assert_eq!(holdings_in(&lab, &config), None, "a broken roster");
    }
}
