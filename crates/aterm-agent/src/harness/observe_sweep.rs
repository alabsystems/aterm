// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine sweep (`docs/DESIGN-account-discovery-2026-11-09.md`, step 4): every login place
//! on this Mac, read by its CLI's own observer whether or not a tab runs on it, so a login no
//! tab's turn-end point reads is still heard of.
//!
//! A login place is a CLI's default home or a roster row's own directory ([`places`]); `aterm
//! harness accounts observed` prints the same places. A Claude Code row that runs on its vault
//! token ([`OnToken`]) is read as its token's account, never as the profile its directory still
//! holds. [`sweep`] notes each place that names an account as a `seen` with no tab (`x`): on the
//! place's first sighting, when its file comes to name another account, then once a day
//! ([`IDLE_REFRESH_S`]). What was noted is kept in one small file under the harness's state
//! ([`Swept`]), since every long-lived aterm process on a Mac sweeps and the day is the Mac's,
//! not a process's; a note the journal did not take is taken back out of it ([`run`]).
//!
//! Each look also hands every such login to the registry as [`LoginState::Idle`], whether or
//! not its note was due: the file is no memory of what the registry took, so the sink's own
//! queue passes a swept login once a day and again until a pass of it succeeds.
//!
//! A place that names no account (signed out, key-only, routed, unread) is not noted and keeps
//! what it last said, so a file caught mid-write is no change.
//!
//! Each look also hands the registry every account signed in to one of aterm's own browser
//! profiles ([`browser_logins`]), `idle` from the source `browser`: an account the person signed
//! in to by hand in `accounts browser`'s window, which no CLI's file may name. Those are no `seen`
//! notes; the journal hears what CLIs and tabs run.
//!
//! Where a place's file states its account's plan ([`super::Observed::plan`]), the sweep notes a
//! `plan-seen` with the plan word once per account per change of word: the word last noted of
//! each account is kept in the same file, so neither a later look nor another process notes it
//! again until the file states another word. Of two places that name one account the newer file
//! speaks for its plan, so the word does not flap between them and a stale file never restates
//! a word the newer one no longer states.

use super::{Observation, Process};
use crate::harness::accounts::{self, AuthRoute, Cli, ProcessLogin, Roster, Row, TokenEntry};
use crate::harness::journal::{
    self, LoginState, Note, ObservedCli, ObservedLogin, ObservedSrc, SeenWhy,
};
use crate::harness::profile;
use aterm_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// How often a login place's unchanged account is noted again: once a day.
pub const IDLE_REFRESH_S: i64 = 86_400;

/// One login place on this Mac as its CLI's observer read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place<'a> {
    pub cli: ObservedCli,
    /// The roster row that names this place, the belief; `None` for a default home no row names.
    pub row: Option<&'a Row>,
    /// What the place's file says now.
    pub seen: Observation,
    /// The place, where the observer named no file.
    pub fallback: PathBuf,
    /// Where this CLI's login is read.
    pub src: ObservedSrc,
}

impl<'a> Place<'a> {
    /// The login a process started on this place runs on, as its environment makes the roster
    /// believe it ([`ProcessLogin`]): the row's own directory, the default home for a row with
    /// none, on the row's vault token where the place was read as one.
    #[must_use]
    pub fn who(&self) -> ProcessLogin {
        let route = match (self.src, self.row.and_then(TokenEntry::of_row)) {
            (ObservedSrc::VaultToken, Some(entry)) => AuthRoute::VaultToken { entry },
            _ => AuthRoute::Stored,
        };
        ProcessLogin {
            login: self
                .row
                .and_then(|row| row.dir.clone())
                .filter(|dir| !dir.is_empty()),
            default_home: true,
            route,
        }
    }

    /// The account this place's login runs on ([`accounts::reconcile`]): the row the file's
    /// address answers to, the account no row lists, or the belief where nothing was read.
    #[must_use]
    pub fn reconciled(&self, roster: &'a Roster) -> accounts::Reconciled<'a> {
        accounts::reconcile(roster, &self.who(), &self.seen)
    }

    /// The file the place was read from, else the place itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        match &self.seen {
            Observation::Seen(seen) => &seen.path,
            Observation::SignedOut { path } | Observation::KeyOnly { path, .. } => path,
            Observation::Routed { .. } | Observation::Unread => &self.fallback,
        }
    }
}

/// The default home, where no row of `roster` names it, then each row's place.
fn rows(roster: &Roster) -> Vec<(Option<&Row>, Option<&str>)> {
    let mut places = Vec::new();
    if roster.row_for_login(None).is_none() {
        places.push((None, None));
    }
    for row in &roster.rows {
        places.push((Some(row), row.dir.as_deref().filter(|dir| !dir.is_empty())));
    }
    places
}

/// Which Claude Code roster rows run on their vault token on this Mac
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.8,
/// [`crate::harness::accounts_tokens::rows_on_token_read`]). The profile in such a row's
/// directory may be stale (§5.7), so its place is read as its token's account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnToken {
    /// The vault could not be read just now: which rows are on a token is not known, so no
    /// row's place is read.
    Unread,
    /// The labels of the rows on their token; every other row is on its stored login.
    Rows(BTreeSet<String>),
}

impl OnToken {
    /// No row is on a token: a CLI or a Mac with no vault token.
    #[must_use]
    pub const fn none() -> Self {
        Self::Rows(BTreeSet::new())
    }
}

/// Every Claude Code login place `roster` names on this Mac, each as Claude Code's observer
/// ([`super::claude`]) reads it now for a process started on that place with nothing else set:
/// the default home's profile for the row with no `dir` (and for no row), each row's own
/// directory otherwise. A place whose profile is absent is [`Observation::Unread`].
///
/// A row `on_token` names is read on its token route: its account is the row's own, whatever
/// profile its directory holds, and a row the lab cannot key is unread. While `on_token` is
/// [`OnToken::Unread`] every row's place is unread; the default home no row names has no token
/// and is read as its profile.
#[must_use]
pub fn claude_places<'a>(
    roster: &'a Roster,
    user_home: &Path,
    on_token: &OnToken,
) -> Vec<Place<'a>> {
    rows(roster)
        .into_iter()
        .filter_map(|(row, dir)| {
            let path = match dir {
                None => profile::effective_of(&[], user_home)?,
                Some(dir) => profile::profile_in_home(Path::new(dir)),
            };
            let token = match (row, on_token) {
                (None, _) => Some(false),
                (Some(_), OnToken::Unread) => None,
                (Some(row), OnToken::Rows(rows)) => Some(rows.contains(&row.label)),
            };
            let (seen, src) = match token {
                None => (Observation::Unread, ObservedSrc::Profile),
                Some(true) => (
                    row.and_then(TokenEntry::of_row)
                        .map_or(Observation::Unread, |entry| {
                            let route = AuthRoute::VaultToken { entry };
                            super::claude(&[], user_home, route, Some(roster))
                        }),
                    ObservedSrc::VaultToken,
                ),
                Some(false) => {
                    let env: Vec<String> = dir
                        .map(|dir| format!("{}={dir}", accounts::LOGIN_ENV))
                        .into_iter()
                        .collect();
                    (
                        super::claude(&env, user_home, AuthRoute::Stored, Some(roster)),
                        ObservedSrc::Profile,
                    )
                }
            };
            Some(Place {
                cli: Cli::Claude.into(),
                row,
                seen,
                fallback: path,
                src,
            })
        })
        .collect()
}

/// Every login place of the pooled CLI `cli` other than Claude Code that `roster` names on this
/// Mac, and its default home, each as its own observer reads it now ([`super::pooled`]) for a
/// process started on that place with nothing else set. The default home is a place whether or
/// not a row names it, and is left out only where no row does and nothing is read there. Claude
/// Code's places are [`claude_places`]'s.
#[must_use]
pub fn pool_places<'a>(cli: Cli, roster: &'a Roster, user_home: &Path) -> Vec<Place<'a>> {
    use crate::ai::connector::Seam;
    let src = match cli {
        Cli::Codex | Cli::Grok | Cli::Kimi => ObservedSrc::AuthFile,
        Cli::Agy => ObservedSrc::CliLog,
        Cli::Claude => return Vec::new(),
    };
    let default_home = user_home.join(cli.accounts().default_home);
    rows(roster)
        .into_iter()
        .filter_map(|(row, dir)| {
            // The environment a process started on this place carries: Codex's own login
            // directory is named apart from its home, agy's is a whole home.
            let home_of = match cli.accounts().seam {
                Seam::Overlay { .. } => dir.map_or(user_home, Path::new),
                Seam::LoginDir { .. } | Seam::LoginFile { .. } => user_home,
            };
            let env = vec![format!("HOME={}", home_of.display())];
            let who = ProcessLogin {
                login: dir.map(str::to_owned),
                default_home: true,
                route: AuthRoute::Stored,
            };
            let process = Process {
                argv: &[],
                env: &env,
                cwd: None,
            };
            let seen = super::pooled(cli, &process, user_home, &who, Some(roster));
            if row.is_none() && seen == Observation::Unread {
                return None;
            }
            Some(Place {
                cli: cli.into(),
                row,
                seen,
                fallback: dir.map_or_else(|| default_home.clone(), PathBuf::from),
                src,
            })
        })
        .collect()
}

/// Every login place `roster` names for the pooled CLI `cli` on this Mac, each as its observer
/// reads it now: [`claude_places`] (rows `on_token` names read as their token's account) or
/// [`pool_places`]. With no `user_home` nothing can be read: each row's place is unread, which
/// leaves it to its row, the belief, and no default home is listed.
#[must_use]
pub fn places_of<'a>(
    cli: Cli,
    roster: &'a Roster,
    user_home: Option<&Path>,
    on_token: &OnToken,
) -> Vec<Place<'a>> {
    match (cli, user_home) {
        (Cli::Claude, Some(home)) => claude_places(roster, home, on_token),
        (_, Some(home)) => pool_places(cli, roster, home),
        (_, None) => roster
            .rows
            .iter()
            .map(|row| Place {
                cli: cli.into(),
                row: Some(row),
                seen: Observation::Unread,
                fallback: row.dir.as_deref().map(PathBuf::from).unwrap_or_default(),
                src: match cli {
                    Cli::Claude => ObservedSrc::Profile,
                    Cli::Agy => ObservedSrc::CliLog,
                    Cli::Codex | Cli::Grok | Cli::Kimi => ObservedSrc::AuthFile,
                },
            })
            .collect(),
    }
}

/// Muse Code's one login place on this Mac, as its observer reads it now ([`super::muse`]): Muse
/// keeps no roster, so no row believes anything of it. None where nothing is read there.
#[must_use]
pub fn muse_place(user_home: &Path) -> Option<Place<'static>> {
    let env = vec![format!("HOME={}", user_home.display())];
    let process = Process {
        argv: &[],
        env: &env,
        cwd: None,
    };
    let seen = super::muse(&process, user_home);
    (seen != Observation::Unread).then(|| Place {
        cli: ObservedCli::Muse,
        row: None,
        seen,
        fallback: user_home.join(".config/muse/auth.json"),
        src: ObservedSrc::AuthFile,
    })
}

/// Every login place on this Mac: each pooled CLI's default home and each directory a row of its
/// roster in `rosters` names (a CLI with no roster there has its default home alone), and Muse
/// Code's. `on_token` says which Claude Code rows run on their vault token ([`claude_places`]).
#[must_use]
pub fn places<'a>(
    rosters: &'a [(Cli, Roster)],
    user_home: &Path,
    on_token: &OnToken,
) -> Vec<Place<'a>> {
    // No roster: the default home alone, which no row names.
    static NONE: Roster = Roster { rows: Vec::new() };
    let mut out = Vec::new();
    for cli in Cli::ALL {
        let roster = rosters
            .iter()
            .find_map(|(of, roster)| (*of == cli).then_some(roster))
            .unwrap_or(&NONE);
        match cli {
            Cli::Claude => out.extend(claude_places(roster, user_home, on_token)),
            Cli::Codex | Cli::Grok | Cli::Kimi | Cli::Agy => {
                out.extend(pool_places(cli, roster, user_home));
            }
        }
    }
    out.extend(muse_place(user_home));
    out
}

/// Every account signed in to one of aterm's own browser profiles on this Mac
/// (`<harness state>/accounts/browsers/` and `…/vendor-browsers/`), by what aterm itself records
/// of each profile, as [`LoginState::Idle`] logins with no tab from [`ObservedSrc::Browser`]:
///
/// - a profile a roster row of `rosters` owns (the one its headless sign-ins run in and
///   `accounts browser` opens, [`headless_oauth::row_browser_profile`]) is that row's CLI and
///   account (its expected identity address, else its email); a vendor row's only where the
///   profile is its own under `vendor-browsers/`, not Claude Code's that it is lent;
/// - a Claude Code profile no row owns is the account its directory is named for, where the name
///   spells one address one to one ([`profile_email`]); a vendor profile no row owns names no
///   vendor, and is left out;
/// - a profile still marked unproven ([`headless_oauth::PROFILE_UNPROVEN_MARK`], made for a
///   sign-in that never succeeded) is signed in to nothing.
///
/// No cookie, stored value or file the browser wrote is read: only the profiles' directory
/// names, the mark aterm keeps in them and the rosters. An account two profiles name is handed
/// once.
#[must_use]
pub fn browser_logins(
    harness_state: &Path,
    user_home: &Path,
    rosters: &[(Cli, Roster)],
    now: i64,
) -> Vec<ObservedLogin> {
    let mut out: Vec<ObservedLogin> = Vec::new();
    for seen in browser_profiles(harness_state, user_home, rosters, now) {
        if !out.contains(&seen.login) {
            out.push(seen.login);
        }
    }
    out
}

/// One of aterm's own browser profiles signed in to an account ([`browser_profiles`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserLogin {
    /// The login, as [`browser_logins`] hands it.
    pub login: ObservedLogin,
    /// The profile's directory.
    pub profile: PathBuf,
    /// The label of the roster row that owns the profile; `None` for a Claude Code profile no
    /// row owns, read from its directory's name.
    pub row: Option<String>,
}

/// Every signed-in profile [`browser_logins`] reads, one per profile in the order it reads
/// them (each row's in roster order, then the unowned Claude Code profiles by name), with the
/// profile and the row that owns it: what `accounts observed` lists beside the login places.
#[must_use]
pub fn browser_profiles(
    harness_state: &Path,
    user_home: &Path,
    rosters: &[(Cli, Roster)],
    now: i64,
) -> Vec<BrowserLogin> {
    use crate::harness::headless_oauth;
    let Ok(at) = u64::try_from(now) else {
        return Vec::new();
    };
    let proven = |dir: &Path| {
        dir.is_dir()
            && std::fs::symlink_metadata(dir.join(headless_oauth::PROFILE_UNPROVEN_MARK))
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    };
    let mut owned: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out: Vec<BrowserLogin> = Vec::new();
    let mut push = |cli: Cli, region, email: &str, profile: &Path, row: Option<&str>| {
        let Some((realm, email)) = journal::observed_claims(cli.into(), region, email) else {
            return;
        };
        out.push(BrowserLogin {
            login: ObservedLogin {
                cli: cli.into(),
                realm,
                email,
                state: LoginState::Idle,
                tab: None,
                at,
                src: ObservedSrc::Browser,
            },
            profile: profile.to_path_buf(),
            row: row.map(str::to_owned),
        });
    };
    for (cli, roster) in rosters {
        let store = accounts::Store::at_for(harness_state, *cli, user_home);
        let shared = store.shared_dir();
        let vendors = shared.join(headless_oauth::VENDOR_PROFILES_DIR);
        for row in &roster.rows {
            let Some(email) = row.email.as_deref().filter(|email| !email.is_empty()) else {
                continue;
            };
            let profile = headless_oauth::row_browser_profile(
                *cli,
                &store.dir,
                &shared,
                &row.label,
                Some(email),
            );
            // A vendor row lent Claude Code's profile of its address (`vendor_profile_dir`,
            // while it has none of its own) is not signed in to its vendor by Claude Code's
            // sign-in there, and that profile stays the Claude Code account's.
            if *cli != Cli::Claude && !profile.starts_with(&vendors) {
                continue;
            }
            // A row's profile is the row's, proven or not: its name is never read for another.
            if proven(&profile) {
                push(
                    *cli,
                    row.vendor.region,
                    row.expected_identity_email().unwrap_or(email),
                    &profile,
                    Some(&row.label),
                );
            }
            owned.insert(profile);
        }
    }
    let own = accounts::Store::at(harness_state)
        .dir
        .join(headless_oauth::BROWSER_PROFILES_DIR);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&own)
        .map(|entries| {
            entries
                .filter_map(|entry| Some(entry.ok()?.path()))
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        if owned.contains(&dir) || !proven(&dir) {
            continue;
        }
        if let Some(email) = dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(profile_email)
        {
            push(Cli::Claude, None, &email, &dir, None);
        }
    }
    out
}

/// The address a browser profile's directory `name` spells one to one: `<local>-at-<domain>`
/// with one `-at-` and a domain whose last label is a top-level one (letters, or `xn--`
/// punycode), whose address [`crate::harness::headless_oauth::sanitize_profile_key`] names
/// `name` again. A label's profile, one named with a digest (`<key>-<8 hex>`: a `+` address,
/// or one cut at 64 characters) and one whose name two addresses could spell name none.
#[must_use]
pub fn profile_email(name: &str) -> Option<String> {
    let (local, domain) = name.split_once("-at-")?;
    let top = domain.rsplit_once('.').map(|(_, top)| top)?;
    let top_level = top.starts_with("xn--") || top.bytes().all(|b| b.is_ascii_alphabetic());
    if local.is_empty() || domain.contains("-at-") || top.is_empty() || !top_level {
        return None;
    }
    let email = format!("{local}@{domain}");
    (crate::harness::headless_oauth::sanitize_profile_key(&email) == name).then_some(email)
}

/// The key of the file's plan memory ([`Swept::render`]): no place key is one, since every place
/// key is a CLI word, a space and a file.
const PLANS_KEY: &str = "plans";

/// What the sweep last noted of each login place, by the place (`<cli> <file>`): the account it
/// named (`<realm> <claims email>`) and when it was noted, Unix seconds; and the plan word last
/// noted of each account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    places: BTreeMap<String, (String, i64)>,
    plans: BTreeMap<String, String>,
}

impl Swept {
    /// Why the place `place`'s sighting of `account` at `now` is noted, or `None` where it is
    /// not yet due: the place's first sighting, another account in it, then at most once per
    /// [`IDLE_REFRESH_S`]. A stored time ahead of `now` (a clock set back) is due.
    fn due(&self, place: &str, account: &str, now: i64) -> Option<SeenWhy> {
        match self.places.get(place) {
            None => Some(SeenWhy::First),
            Some((last, _)) if last != account => Some(SeenWhy::Changed),
            Some((_, at)) if now.saturating_sub(*at) >= IDLE_REFRESH_S || *at > now => {
                Some(SeenWhy::Refresh)
            }
            Some(_) => None,
        }
    }

    /// Take back what a sweep at `now` kept of `account`, whose note the journal did not take:
    /// each place it noted then says what `before` (the file as that sweep read it) said of
    /// it, so the place is due again as it was.
    #[cfg(unix)]
    fn take_back(&mut self, before: &Self, account: &str, now: i64) {
        let noted: Vec<String> = self
            .places
            .iter()
            .filter(|(_, (named, at))| named == account && *at == now)
            .map(|(place, _)| place.clone())
            .collect();
        for place in noted {
            match before.places.get(&place) {
                Some(last) => self.places.insert(place, last.clone()),
                None => self.places.remove(&place),
            };
        }
    }

    /// Take back the plan word a sweep kept of `account`, whose `plan-seen` the journal did not
    /// take: the account holds what `before` held, so the word is due again.
    #[cfg(unix)]
    fn take_back_plan(&mut self, before: &Self, account: &str) {
        match before.plans.get(account) {
            Some(word) => self.plans.insert(account.to_owned(), word.clone()),
            None => self.plans.remove(account),
        };
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        let mut places = Map::new();
        for (place, (account, at)) in &self.places {
            let mut last = Map::new();
            last.insert("account".into(), Value::from(account.as_str()));
            last.insert("at".into(), Value::from(*at));
            places.insert(place.clone(), Value::Object(last));
        }
        if !self.plans.is_empty() {
            let plans = self
                .plans
                .iter()
                .map(|(account, word)| (account.clone(), Value::from(word.as_str())))
                .collect();
            places.insert(PLANS_KEY.into(), Value::Object(plans));
        }
        format!("{}\n", Value::Object(places))
    }

    /// What `bytes` of the file say; an entry that does not read is left out, so its place is
    /// noted as new.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let Ok(Value::Object(file)) = aterm_json::from_slice::<Value>(bytes) else {
            return Self::default();
        };
        let places = file
            .iter()
            .filter(|(place, _)| place.as_str() != PLANS_KEY)
            .filter_map(|(place, last)| {
                let account = last.get("account")?.as_str()?.to_owned();
                let at = last.get("at")?.as_i64()?;
                Some((place.clone(), (account, at)))
            })
            .collect();
        // A word outside the plan vocabulary is no memory: its account's plan is due again.
        let plans = file
            .get(PLANS_KEY)
            .and_then(Value::as_object)
            .map(|plans| {
                plans
                    .iter()
                    .filter_map(|(account, word)| {
                        let word = journal::plan_word(word.as_str()?)?;
                        Some((account.clone(), word.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { places, plans }
    }
}

/// The folder and name of the sweep's file under the harness's state.
#[cfg(unix)]
const SWEPT_DIR: &str = "alab";
#[cfg(unix)]
const SWEPT_FILE: &str = "account-sweep.json";
/// More than any Mac's login places make.
#[cfg(unix)]
const SWEPT_MAX: usize = 1 << 20;

/// What the sweep last noted on this Mac; nothing where the file is absent or does not read.
#[cfg(unix)]
#[must_use]
pub fn load(harness_state: &Path) -> Swept {
    aterm_uds::private_file::read_private(&harness_state.join(SWEPT_DIR), SWEPT_FILE, SWEPT_MAX)
        .map_or_else(|_| Swept::default(), |bytes| Swept::parse(&bytes))
}

/// Keep `swept` for this Mac's next sweep, replacing the file whole.
///
/// # Errors
/// The file's folder cannot be made private, or the file cannot be written.
#[cfg(unix)]
pub fn save(harness_state: &Path, swept: &Swept) -> std::io::Result<()> {
    let dir = harness_state.join(SWEPT_DIR);
    aterm_uds::private_file::ensure_private_dir(&dir)?;
    aterm_uds::private_file::write_private(&dir, SWEPT_FILE, swept.render().as_bytes())
}

/// What one sweep gives: the journal notes that are due (each place's `seen`, then each
/// account's `plan-seen`), and every login the places name, for the registry's `account-obs`
/// rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    pub notes: Vec<Note>,
    /// The account each of `notes` is of, as [`Swept`] names it.
    pub of: Vec<String>,
    pub logins: Vec<ObservedLogin>,
}

/// One sweep of `places` at `now`: for each place that names an account, the login as
/// [`LoginState::Idle`] with no tab (where a tab of this process runs it, the registry's queue
/// counts it running), and, where the place is due ([`Swept::due`]), a `seen` note that names
/// no tab and no harness. An account two places name is noted and handed once a sweep. Then a
/// `plan-seen` of each account whose newest file states a plan word that is not the word last
/// noted of it. Keys come from the installed journal sink; a place whose account it cannot
/// key gives nothing and is left due.
#[must_use]
pub fn sweep(places: &[Place<'_>], swept: &mut Swept, now: i64) -> Sweep {
    let mut out = Sweep::default();
    let Ok(t) = u64::try_from(now) else {
        return out;
    };
    let mut handed: BTreeSet<String> = BTreeSet::new();
    let mut noted: BTreeSet<String> = BTreeSet::new();
    // Each account's newest file, and the plan-seen of the word it states, where it states one.
    let mut stated: BTreeMap<String, (Option<i64>, Option<Note>)> = BTreeMap::new();
    for place in places {
        let Some(observed) = place.seen.seen() else {
            continue;
        };
        let Some((realm, email)) =
            journal::observed_claims(observed.cli, observed.region, &observed.email)
        else {
            continue;
        };
        let Some(key) = journal::realm_key(&realm, &email) else {
            continue;
        };
        let at = format!("{} {}", observed.cli.id(), observed.path.display());
        let account = format!("{realm} {email}");
        if stated
            .get(&account)
            .is_none_or(|(mtime, _)| observed.mtime > *mtime)
        {
            let note = observed
                .plan
                .map(|plan| journal::plan_note(observed.cli, key, plan, observed.src, t));
            stated.insert(account.clone(), (observed.mtime, note));
        }
        if handed.insert(account.clone()) {
            out.logins.push(ObservedLogin {
                cli: observed.cli,
                realm,
                email,
                state: LoginState::Idle,
                tab: None,
                at: t,
                src: observed.src,
            });
        }
        let Some(why) = swept.due(&at, &account, now) else {
            continue;
        };
        swept.places.insert(at, (account.clone(), now));
        if !noted.insert(account.clone()) {
            continue;
        }
        out.notes.push(journal::idle_seen_note(
            observed.cli,
            key,
            observed.src,
            why,
            t,
        ));
        out.of.push(account);
    }
    for (account, (_, note)) in stated {
        let Some((note, plan)) = note.and_then(|note| Some((note, note.plan?))) else {
            continue;
        };
        if swept.plans.get(&account).is_some_and(|held| held == plan) {
            continue;
        }
        swept.plans.insert(account.clone(), plan.to_owned());
        out.notes.push(note);
        out.of.push(account);
    }
    out
}

/// This Mac's sweep at `now`, whole: read every login place `rosters`, `user_home` and
/// `on_token` name, hand each login to the registry through the installed sink, and note what
/// is due against the file under `harness_state`; then hand each account signed in to one of
/// aterm's own browser profiles ([`browser_logins`]). Returns how many notes the journal took.
#[cfg(unix)]
pub fn run(
    harness_state: &Path,
    user_home: &Path,
    rosters: &[(Cli, Roster)],
    on_token: &OnToken,
    now: i64,
) -> usize {
    let places = places(rosters, user_home, on_token);
    let noted = run_through(
        harness_state,
        &places,
        now,
        &mut journal::note_taken,
        &mut journal::observed,
    );
    for login in browser_logins(harness_state, user_home, rosters, now) {
        journal::observed(&login);
    }
    noted
}

/// [`run`] over `places`, through `note` (which says whether the journal took the note) and
/// `hand` (the registry's path).
///
/// - Every login is handed, due or not: whoever takes it keeps what the registry took
///   ([`journal::Sink::observed`]), so a pass that failed, or a process that ended before its
///   pass, costs no day.
/// - The file is kept before the notes are sent, and nothing is noted where it cannot be kept:
///   two processes that sweep at one moment may each note a place once, never once a look.
/// - A note the journal did not take is taken back out of the file ([`Swept::take_back`]), so
///   its place is due at the next look with the same `why`. Only a process that ends between
///   the file and the note leaves the place to the next day's sweep.
#[cfg(unix)]
fn run_through(
    harness_state: &Path,
    places: &[Place<'_>],
    now: i64,
    note: &mut dyn FnMut(&Note) -> bool,
    hand: &mut dyn FnMut(&ObservedLogin),
) -> usize {
    let before = load(harness_state);
    let mut swept = before.clone();
    let out = sweep(places, &mut swept, now);
    for login in &out.logins {
        hand(login);
    }
    if out.notes.is_empty() || save(harness_state, &swept).is_err() {
        return 0;
    }
    let mut taken = 0;
    for (each, account) in out.notes.iter().zip(&out.of) {
        if note(each) {
            taken += 1;
        } else if each.plan.is_some() {
            swept.take_back_plan(&before, account);
        } else {
            swept.take_back(&before, account, now);
        }
    }
    if taken < out.notes.len() {
        // Where this fails the untaken notes wait for the next day's sweep.
        let _ = save(harness_state, &swept);
    }
    taken
}

#[cfg(test)]
#[path = "observe_sweep_tests.rs"]
mod tests;
