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
//! lab (every test, a debug build) reports nothing, so no place row is written. So is its login
//! source (the `login` rows of the shared file `ai/logins`, docs/DESIGN-shared-files-2026-11-11.md
//! §10 file 3, [`Logins`]): what each CLI's accounts store says of each login this Mac holds,
//! read without a credential (`Store::peek_account_records`) into the harness's value-free words
//! (`journal::login_rows`), every word checked again into the journal's own types
//! ([`logins_in`]); a CLI whose store does not read reports nothing of that CLI, so none of its
//! rows ends on a passing failure and no other CLI's waits for it. The vault is not read for it:
//! a Claude Code row is on its token as the upkeep's last reading says (`tokens_here.json`), the
//! reading its launch takes. No token or its state is reported: those are the fleet vault's
//! alone.

use aterm_agent::harness::{
    accounts::{self, Cli, Roster},
    alab_roster::{self, Lab},
    journal::{self, Note, Sink},
    observe::{self, sweep::OnToken},
    provider_read::{self, ProbeEvent},
};
use aterm_db::alab::journal::{
    Appended, Event, Kind, Pk, Publishing, Res, Source, Spool, Window, Word,
    mine::{self, Login},
    place::{self, Holding, Why},
};
use aterm_uds::file_watch::FileStamp;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

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

/// `word` parsed by `parse`, where there is one; `None` (the outer one) where it is no word.
fn word<T>(word: Option<&str>, parse: impl Fn(&str) -> Option<T>) -> Option<Option<T>> {
    match word {
        None => Some(None),
        Some(word) => parse(word).map(Some),
    }
}

/// The journal event a harness note is, when every word is in the journal's vocabulary.
fn event(note: &Note) -> Option<Event> {
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
        plan: word(note.plan, Word::new)?,
        src: Source::parse(note.src)?,
        why: word(note.why, Word::new)?,
        to: word(note.to.as_ref().map(journal::Key::as_str), Pk::new)?,
        tab: word(note.tab.as_ref().map(journal::Tab::as_str), Word::new)?,
        harness: word(note.harness, Word::new)?,
        // A harness note is never a `decided`: the publisher notes those (SF-08b).
        op: None,
        via: None,
        program: None,
    })
    // Only a kind about one tab's run names a tab or a harness, as every reader admits it.
    .filter(|event| (event.tab.is_none() && event.harness.is_none()) || event.kind.names_tab())
    // A plan word rides only on a `plan-seen`, and a `plan-seen` always carries one.
    .filter(|event| (event.kind == Kind::PlanSeen) == event.plan.is_some())
}

static GLUE: OnceLock<Glue> = OnceLock::new();

fn glue() -> &'static Glue {
    GLUE.get_or_init(|| Glue {
        spool: Spool::local(),
        publishing: Publishing::new(aterm_types::dirs::aterm_config_path()),
    })
}

/// Install the journal sink for this process, fill the provider-read door with it, and give the
/// flusher this Mac's holdings and its logins.
pub(super) fn install() {
    journal::install(glue());
    let _ = provider_read::install(Box::new(probe));
    let _ = place::install_holdings(Box::new(|| {
        let lab = alab_roster::current()?;
        let config = aterm_types::dirs::aterm_config_path()?;
        let home = user_home();
        holdings_in(
            &lab,
            &config,
            home.as_deref(),
            &on_token_now(&config, home.as_deref()),
        )
    }));
    let _ = mine::install_logins(Box::new(Logins::default()));
}

/// This Mac's login source, the flusher's `login` rows ([`mine::Source`]): each pooled CLI's
/// accounts store under the harness state, read at a pass the flusher's gate lets past
/// ([`logins_in`]), and the stamps of the files they are read from ([`login_files`]), looked at
/// each pass so a login whose row changed with no sharp note beside it is read within a minute;
/// so is the clock against the second the last report's token reading can answer otherwise
/// ([`LoginsRead::turns`]), the one word of a row that moves with no file moving.
#[derive(Default)]
struct Logins {
    /// What the last report saw: `None` before the first.
    seen: Mutex<Option<Seen>>,
}

/// What a report of [`Logins`] saw.
struct Seen {
    /// The stamps of [`login_files`] as it found them before it read.
    stamps: Vec<Option<FileStamp>>,
    /// [`LoginsRead::turns`] of what it read; `None` after a read that did not finish.
    turns: Option<i64>,
}

impl Logins {
    /// The config, the harness state and the user's home the report reads under.
    fn places() -> Option<(PathBuf, PathBuf, Option<PathBuf>)> {
        Some((
            aterm_types::dirs::aterm_config_path()?,
            aterm_agent::harness::cli::harness_state_dir().ok()?,
            user_home(),
        ))
    }
}

impl mine::Source for Logins {
    fn moved(&self) -> bool {
        let Some((config, harness, home)) = Self::places() else {
            return false;
        };
        let stamps = stamps(&login_files(&config, &harness, home.as_deref()));
        let seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        moved_since(
            seen.as_ref(),
            &stamps,
            aterm_agent::supervise::limit::unix_now(),
        )
    }

    fn report(&self) -> Option<mine::Reports> {
        let (config, harness, home) = Self::places()?;
        // Stamped before the read, so a file that moves while it reads has moved at the next pass.
        let stamps = stamps(&login_files(&config, &harness, home.as_deref()));
        let now = aterm_agent::supervise::limit::unix_now();
        let read = alab_roster::current()
            .and_then(|lab| logins_in(&lab, &config, &harness, home.as_deref(), now));
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(Seen {
            stamps,
            turns: read.as_ref().and_then(|read| read.turns),
        });
        read.map(|read| read.reports)
    }
}

/// Whether a login may read otherwise at `now` than when `seen` was read: none was, a file of
/// [`login_files`] moved (`stamps` now), or its token reading's turn came.
fn moved_since(seen: Option<&Seen>, stamps: &[Option<FileStamp>], now: i64) -> bool {
    seen.is_none_or(|seen| seen.stamps != stamps || seen.turns.is_some_and(|at| at <= now))
}

/// What [`logins_in`] read.
#[derive(Debug, PartialEq)]
struct LoginsRead {
    /// One report per pooled CLI.
    reports: mine::Reports,
    /// The first second after the read at which a Claude Code row's reading on its vault token
    /// can answer otherwise by the clock alone (`accounts::on_token_turns`: the reading coming to
    /// stand, aging out, or the row's token ending): `None` where none is ahead.
    turns: Option<i64>,
}

/// Each file's stamp now, `None` for one that is not there.
fn stamps(files: &[PathBuf]) -> Vec<Option<FileStamp>> {
    files.iter().map(|file| FileStamp::of(file)).collect()
}

/// `cli`'s accounts store under `harness`: Claude Code's there, any other CLI's under `home` too
/// (none without one).
fn store_for(cli: Cli, harness: &Path, home: Option<&Path>) -> Option<accounts::Store> {
    match (cli, home) {
        (Cli::Claude, _) => Some(accounts::Store::at(harness)),
        (_, Some(home)) => Some(accounts::Store::at_for(harness, cli, home)),
        (_, None) => None,
    }
}

/// Every file a pass's login rows are read from ([`logins_in`]): each pooled CLI's roster beside
/// the `aterm.toml` at `config`, and its store's state, sync receipt, ledger and `logins.json`
/// (`Store::state_files`, `Store::logins_files`), with Claude Code's reading of the rows on their
/// vault token (`accounts::TOKENS_HERE_FILE`). Their stamps are looked at, never their contents.
fn login_files(config: &Path, harness: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for cli in Cli::ALL {
        files.extend(accounts::roster_beside_for(config, cli));
        if let Some(store) = store_for(cli, harness, home) {
            files.extend(store.state_files());
            files.extend(store.logins_files());
            if cli == Cli::Claude {
                files.push(
                    store
                        .state_file()
                        .with_file_name(accounts::TOKENS_HERE_FILE),
                );
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// The Claude Code rows on their vault token now: such a row is held by its token's account, not
/// by the profile its directory may still hold, as the login sweep reads it.
fn on_token_now(config: &Path, home: Option<&Path>) -> OnToken {
    match (
        aterm_agent::harness::cli::harness_state_dir().ok(),
        home,
        roster_of(Some(config), Cli::Claude),
    ) {
        (Some(state), Some(home), Some(roster)) => crate::alab_observed::on_token(
            &state,
            home,
            &roster,
            aterm_agent::supervise::limit::unix_now(),
        ),
        _ => OnToken::none(),
    }
}

/// This Mac's logins at `now`, one report per pooled CLI: its accounts store under `harness`
/// ([`store_for`]; its roster beside the `aterm.toml` at `config`), read without a credential
/// (`Store::peek_account_records`) with the upkeep's reading of the rows on their vault token
/// (`Store::read_tokens_here`, no vault read), through the harness's value-free rows
/// (`journal::login_rows`), each word checked again into the journal's types ([`login`]); a row
/// outside the journal's words is left out. Each says why this Mac holds it as the lab does:
/// `linked` where a preference of this Mac's names it, else `fill` (run here with no preference
/// naming it, or an account the lab does not know); the flusher's book says the reason of a
/// `place` row over either. A CLI with no roster holds none (`Some` of nothing); one whose store
/// does not read, or that keeps its store under a home there is none of, is `None`, which leaves
/// its rows as the book holds them. With them, when the token reading of a Claude Code row it read
/// next turns ([`LoginsRead::turns`]). `None` where the lab names no machine for this Mac.
fn logins_in(
    lab: &Lab,
    config: &Path,
    harness: &Path,
    home: Option<&Path>,
    now: i64,
) -> Option<LoginsRead> {
    let me = lab.me.as_ref()?;
    let mut out = mine::Reports::new();
    let mut turns: Option<i64> = None;
    for cli in Cli::ALL {
        let Some(word) = mine::Cli::parse(cli.id()) else {
            continue;
        };
        let mut read = || {
            let Some(path) = accounts::roster_beside_for(config, cli) else {
                return Some(Vec::new());
            };
            let store = store_for(cli, harness, home)?;
            let mut records = store.peek_account_records(&path, now).ok()?;
            records.state.tokens_here = store.read_tokens_here();
            for row in &records.roster.rows {
                if let Some(at) = accounts::on_token_turns(row, &records.state, now) {
                    turns = Some(turns.map_or(at, |first| first.min(at)));
                }
            }
            let linked: BTreeSet<&str> = records
                .roster
                .rows
                .iter()
                .filter_map(|row| lab.account_of(cli, row))
                .filter(|account| account.preferred_on(&me.key).is_some())
                .map(|account| account.pk.as_str())
                .collect();
            let held = |key: &journal::Key| {
                if linked.contains(key.as_str()) {
                    "linked"
                } else {
                    "fill"
                }
            };
            Some(
                journal::login_rows(cli, &records, &keyer, &held, now)
                    .iter()
                    .filter_map(login)
                    .collect(),
            )
        };
        out.insert(word, read());
    }
    Some(LoginsRead {
        reports: out,
        turns,
    })
}

/// The journal's login a harness row is, when every word is in its vocabulary.
fn login(row: &journal::LoginRow) -> Option<Login> {
    Some(Login {
        account: Pk::new(row.account.as_str()).filter(|pk| pk.as_str().starts_with("ac-"))?,
        cli: mine::Cli::parse(row.cli)?,
        place: mine::Place::parse(row.place)?,
        state: mine::State::parse(row.state)?,
        proof: mine::Proof::parse(row.proof)?,
        src: mine::Src::parse(row.src)?,
        use_: if row.tabs > 0 {
            mine::Use::Running
        } else {
            mine::Use::Idle
        },
        tabs: row.tabs,
        held: mine::Held::parse(row.held)?,
        since: row.since,
        served_at: row.served_at,
        ends_by: row.ends_by,
        ended_at: row.ended_at,
        attempt: mine::Attempt::parse(row.attempt)?,
        attempt_at: row.attempt_at,
        attempt_route: word(row.attempt_route, mine::Route::parse)?,
        plan: word(row.plan, Word::new)?,
    })
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
