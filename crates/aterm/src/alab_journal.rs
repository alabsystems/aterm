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
//! process holds and what each login place holds ([`holdings_in`]); a process that holds no
//! lab (every test, a debug build) reports nothing, so no place row is written.

use aterm_agent::harness::{
    accounts::{self, Cli, Roster},
    alab_roster::{self, Lab},
    journal::{self, Note, Sink},
    observe::{self, sweep::OnToken},
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

    fn taking(&self) -> bool {
        self.spool.is_some() && self.publishing.now()
    }

    fn note(&self, note: &Note) -> bool {
        if !self.publishing.now() {
            return false;
        }
        let (Some(spool), Some(event)) = (&self.spool, event(note)) else {
            return false;
        };
        // The spool drops what it cannot take; a note never waits or fails its caller.
        match spool.append(&event) {
            Ok(Appended::Written) => true,
            Ok(Appended::Full | Appended::Refused) | Err(_) => false,
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
        let config = aterm_types::dirs::aterm_config_path()?;
        let home = user_home();
        // A Claude Code row on its vault token is held by its token's account, not by the
        // profile its directory may still hold, as the login sweep reads it.
        let on_token = match (
            aterm_agent::harness::cli::harness_state_dir().ok(),
            home.as_deref(),
            roster_of(Some(&config), Cli::Claude),
        ) {
            (Some(state), Some(home), Some(roster)) => crate::alab_observed::on_token(
                &state,
                home,
                &roster,
                aterm_agent::supervise::limit::unix_now(),
            ),
            _ => OnToken::none(),
        };
        holdings_in(&lab, &config, home.as_deref(), &on_token)
    }));
}

/// What this Mac runs that it does not prefer (`alab_roster::holdings`), over every pooled CLI's
/// login places (`observe::sweep::places_of`: the roster beside the `aterm.toml` at `config`,
/// and the default home under `home`, which holds a login whether or not a row names it; a
/// Claude Code row `on_token` names is read as its token's account): `None` where the lab names
/// no machine for this Mac or a roster that is there does not read, so a passing failure ends
/// no place row.
fn holdings_in(
    lab: &Lab,
    config: &Path,
    home: Option<&Path>,
    on_token: &OnToken,
) -> Option<Vec<Holding>> {
    lab.me.as_ref()?;
    let mut out = Vec::new();
    for cli in Cli::ALL {
        let Some(path) = accounts::roster_beside_for(config, cli) else {
            continue;
        };
        let roster = Roster::load_for(cli, &path).ok()?;
        let places = observe::sweep::places_of(cli, &roster, home, on_token);
        for (pk, why) in alab_roster::holdings(lab, cli, &roster, &places) {
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
pub(crate) fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// `cli`'s roster beside the aterm config at `config`.
pub(crate) fn roster_of(config: Option<&Path>, cli: Cli) -> Option<Roster> {
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
#[path = "alab_journal_tests.rs"]
mod tests;
