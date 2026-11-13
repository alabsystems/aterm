// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault token state as every surface shows it, value-free, and the quiet offer
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9, §5.10, slice S12).
//!
//! - **The accounts** ([`covered`]): each Claude Code roster row that names an email, and each
//!   account a session on this Mac is logged into by its own login record, row or not
//!   ([`super::accounts_tokens::session_logins`]), keyed by the lab account key; and the accounts
//!   this Mac's table keeps a row of.
//! - **The view** ([`view`]): one read of the vault's tokens ([`TokenVault::listed`]), this Mac's
//!   table and the clock make each account's [`TokenState`] (§5.1), its login half, and the offer
//!   where one stands on a host whose verb makes a token ([`Places::offers`]).
//! - **The offer** is a pair, the token state and the login state (`ok`, `ending` under
//!   [`ENDING_S`] left, `ended`; `-` while a token is in use; `none` for a roster row with no
//!   login on this Mac, [`LoginHere`]), each with the date it began. It
//!   stands for an account a session here is logged into, in a state [`TokenState::offer`] offers
//!   for, while no write of this Mac's of the token waits. [`note_offers`] journals
//!   `token-offered` once per pair, so a re-read journals nothing and no timer offers again.
//! - **Peers' use** ([`peers_of`]) comes from one bounded fleet read: each peer whose session
//!   rows carry `token=<ac key>`; with no read, the report says "peers not read".
//!
//! Nothing here reads, holds or prints a token: the vault's standings are value-free.

pub use super::account_card::{ENDING_S, LoginHalf, LoginHere};
use super::accounts::{self, Cli, Store, TokenEntry};
use super::accounts_step::Step;
use super::accounts_tokens::{
    self as tokens, InVault, Offered, SessionLogin, TokenNote, TokenRow, TokenState, TokenVault,
    date,
};
use aterm_json::{Map, Value};
use aterm_secrets::claude_token::{Key, Lookup, Standing};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// An account the surfaces cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Covered {
    /// The lab account key.
    pub key: Key,
    /// The row's claims email, else the login record's.
    pub email: String,
    /// The roster row's label, where one names it.
    pub label: Option<String>,
    /// A session on this Mac is logged into it.
    pub logged_in: bool,
    /// Its stored login here.
    pub login: LoginHere,
}

/// The offer standing for an account (§5.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The pair it is for: `<token>:<since>/<login>:<since>`.
    pub pair: String,
    /// The one step that takes it: the verb for the account, with `--replace` where its token is
    /// still in use.
    pub step: Step,
    /// When it was first made, once [`note_offers`] read the table.
    pub since: Option<u64>,
}

/// One account as the surfaces show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountToken {
    /// The lab account key.
    pub key: String,
    /// Its email.
    pub email: String,
    /// The roster row's label, where one names it.
    pub label: Option<String>,
    /// Its token's state here (§5.1).
    pub state: TokenState,
    /// What the vault holds of its token, value-free.
    pub standing: Option<Standing>,
    /// This Mac's table row of it.
    pub row: Option<TokenRow>,
    /// Whether a write of this Mac's of the token still waits to be signed (`None`: unread).
    pub pending: Option<bool>,
    /// A session on this Mac is logged into it.
    pub logged_in: bool,
    /// The offer pair's login half: [`Covered::login`], or [`LoginHalf::InUse`] while a token is
    /// in use.
    pub login: LoginHere,
    /// The offer, where one stands.
    pub offer: Option<Offer>,
}

impl AccountToken {
    /// How the surfaces name it: its row's label, else its email.
    #[must_use]
    pub fn named(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.email)
    }

    /// The vault entry's name.
    #[must_use]
    pub fn entry(&self) -> String {
        Key::parse(&self.key).map_or_else(String::new, |key| key.entry())
    }

    fn meta(&self) -> Option<&aterm_secrets::claude_token::Meta> {
        self.standing.as_ref()?.meta.as_ref()
    }

    /// When its token was made.
    #[must_use]
    pub fn minted_at(&self) -> Option<u64> {
        self.meta()
            .map(|meta| meta.minted_at)
            .or_else(|| self.row.as_ref()?.minted_at)
    }

    /// When its token ends.
    #[must_use]
    pub fn expires_at(&self) -> Option<u64> {
        self.meta()
            .map(|meta| meta.expires_at)
            .or_else(|| self.row.as_ref()?.expires_at)
    }

    /// The Mac that made it, for display.
    #[must_use]
    pub fn minted_on(&self) -> Option<String> {
        self.meta()
            .map(|meta| meta.minted_on.clone())
            .or_else(|| self.row.as_ref()?.minted_on.clone())
    }

    /// Where its end came from: `printed` or `assumed`.
    #[must_use]
    pub fn expiry_from(&self) -> Option<String> {
        self.meta()
            .map(|meta| tokens::expiry_word(meta.expiry_from).to_owned())
            .or_else(|| self.row.as_ref()?.expiry_from.clone())
    }

    /// The seat that signed it, read from the log.
    #[must_use]
    pub fn signer(&self) -> Option<String> {
        self.standing
            .as_ref()
            .and_then(|standing| standing.signer.clone())
            .or_else(|| self.row.as_ref()?.signer.clone())
    }

    /// Whether a token made here is not yet in vault: a write of it waits, or it was signed here
    /// and no server took it yet.
    #[must_use]
    pub fn not_yet(&self) -> bool {
        self.pending == Some(true)
            || self
                .row
                .as_ref()
                .is_some_and(|row| row.vault == InVault::SignedHere)
    }

    /// The vault column (§5.1) as a person reads it.
    #[must_use]
    pub fn vault_words(&self) -> Option<String> {
        if self.pending == Some(true) {
            return Some(match self.row.as_ref().map(|row| &row.vault) {
                Some(InVault::Waiting(why)) => format!("not yet in vault (waiting: {why})"),
                _ => "not yet in vault (waiting in this Mac's vault writes)".to_owned(),
            });
        }
        match self.row.as_ref().map(|row| &row.vault) {
            // The write that waited is signed now: the log holds it.
            Some(InVault::Waiting(_)) if self.standing.is_some() => Some("in vault".to_owned()),
            Some(vault) => Some(vault.words()),
            None => self.standing.as_ref().map(|_| "in vault".to_owned()),
        }
    }

    /// The tab's state line's short phrase while an offer stands (§5.9): `token: none`, or
    /// `login ends in 5 days; token: none`.
    #[must_use]
    pub fn phrase(&self, now: i64) -> Option<String> {
        self.offer.as_ref()?;
        let token = format!("token: {}", self.state.word());
        Some(match self.login.half() {
            Some(LoginHalf::Ending { end }) => {
                let days = (end - now).max(0) / 86_400;
                match days {
                    0 => format!("login ends today; {token}"),
                    1 => format!("login ends in 1 day; {token}"),
                    n => format!("login ends in {n} days; {token}"),
                }
            }
            Some(LoginHalf::Ended { .. }) => format!("login ended; {token}"),
            Some(LoginHalf::InUse | LoginHalf::Ok) | None => token,
        })
    }

    /// The Lab page's one quiet line while an offer stands (§5.9); the Account card says the
    /// offer as its own item.
    #[must_use]
    pub fn offer_line(&self) -> Option<String> {
        let offer = self.offer.as_ref()?;
        let named = self.named();
        Some(match offer.step {
            Step::Token { replace: true, .. } => {
                format!("{named}'s vault token is {}: replace it", self.state.word())
            }
            _ => format!("a vault token would keep {named} signed in: make one"),
        })
    }

    /// The account's line in the report: its state and every value-free field, then the offer's
    /// step, or "not yet in vault" while a write made here waits.
    fn text(&self, peers: &PeersShown) -> String {
        let mut parts = vec![self.state.words()];
        if self.minted_at().is_some() || self.row.is_some() {
            parts.push(format!("entry {}", self.entry()));
        }
        if let Some(at) = self.minted_at() {
            let on = self
                .minted_on()
                .map_or_else(String::new, |on| format!(" on {on}"));
            parts.push(format!("made {}{on}", date(at)));
        }
        if let Some(at) = self.expires_at() {
            let from = self
                .expiry_from()
                .map_or_else(String::new, |from| format!(" ({from})"));
            parts.push(format!("ends {}{from}", date(at)));
        }
        if let Some(signer) = self.signer() {
            parts.push(format!("signer {signer}"));
        }
        if let Some(vault) = self.vault_words() {
            parts.push(vault);
        }
        if let Some(row) = &self.row {
            if let Some(at) = row.verified {
                parts.push(format!("verified {}", date(at)));
            }
            if let Some(at) = row.rejected_at {
                parts.push(format!("rejected here {}", date(at)));
            }
            if let (Some(first), Some(last)) =
                (row.used_on.first_served_at, row.used_on.last_served_at)
            {
                parts.push(format!("used here {}..{}", date(first), date(last)));
            }
            if self.standing.is_some() {
                parts.push(peers.words(row));
            }
        }
        if self.label.is_some() {
            match self.login {
                LoginHere::Half(LoginHalf::InUse) | LoginHere::Unread => {}
                LoginHere::Half(half) => parts.push(format!("login {}", half.word())),
                LoginHere::Absent => parts.push("no login here".to_owned()),
            }
        }
        let mut line = format!("  {} ({}): {}", self.named(), self.email, parts.join("; "));
        if let Some(offer) = &self.offer {
            line.push_str(&format!("\n    offer: run `{}`", offer.step));
        } else if self.not_yet() && self.state == TokenState::None {
            line.push_str("\n    not yet in vault: this Mac's vault pass signs it");
        }
        line
    }

    fn json(&self, peers: &PeersShown) -> Value {
        let mut o = Map::new();
        o.insert("key".into(), Value::from(self.key.clone()));
        o.insert("email".into(), Value::from(self.email.clone()));
        o.insert("label".into(), Value::from(self.label.clone()));
        o.insert("logged_in".into(), Value::from(self.logged_in));
        o.insert("state".into(), Value::from(self.state.word()));
        o.insert("state_words".into(), Value::from(self.state.words()));
        o.insert("entry".into(), Value::from(self.entry()));
        o.insert("minted_at".into(), Value::from(self.minted_at()));
        o.insert("expires_at".into(), Value::from(self.expires_at()));
        o.insert("expiry_from".into(), Value::from(self.expiry_from()));
        o.insert("minted_on".into(), Value::from(self.minted_on()));
        o.insert("signer".into(), Value::from(self.signer()));
        o.insert("vault".into(), Value::from(self.vault_words()));
        o.insert("not_yet_in_vault".into(), Value::from(self.not_yet()));
        let row = self.row.as_ref();
        o.insert(
            "verified".into(),
            Value::from(row.and_then(|row| row.verified)),
        );
        o.insert(
            "rejected_at".into(),
            Value::from(row.and_then(|row| row.rejected_at)),
        );
        let used = row.map(|row| &row.used_on);
        let mut here = Map::new();
        here.insert(
            "first_served_at".into(),
            Value::from(used.and_then(|used| used.first_served_at)),
        );
        here.insert(
            "last_served_at".into(),
            Value::from(used.and_then(|used| used.last_served_at)),
        );
        let mut used_on = Map::new();
        used_on.insert("here".into(), Value::Object(here));
        used_on.insert(
            "peers".into(),
            Value::Array(
                used.map(|used| used.peers.iter().cloned().map(Value::from).collect())
                    .unwrap_or_default(),
            ),
        );
        used_on.insert("peers_read".into(), Value::from(peers.read()));
        used_on.insert(
            "peers_read_at".into(),
            Value::from(used.and_then(|used| used.peers_read_at)),
        );
        o.insert("used_on".into(), Value::Object(used_on));
        o.insert("login".into(), Value::from(self.login.word()));
        o.insert(
            "offer".into(),
            self.offer.as_ref().map_or(Value::Null, |offer| {
                let mut v = Map::new();
                v.insert("state".into(), Value::from(self.state.word()));
                v.insert("login".into(), Value::from(self.login.word()));
                v.insert("pair".into(), Value::from(offer.pair.clone()));
                v.insert("since".into(), Value::from(offer.since));
                v.insert("step".into(), Value::from(offer.step.to_string()));
                Value::Object(v)
            }),
        );
        Value::Object(o)
    }
}

/// Whether this platform is offered a token's step: macOS, where the verb makes one and aterm's
/// wrapper serves it (`accounts_tokens` refuses the verb elsewhere). A surface with no host
/// passes it as [`Places::offers`]; one with a host asks that host, as the verb does
/// ([`AccountsHost::on_macos`](super::cli::AccountsHost::on_macos)).
pub const OFFERED_HERE: bool = cfg!(target_os = "macos");

/// `at` as the pair's date, `-` where none says it.
fn since_word(at: Option<i64>) -> String {
    at.and_then(|at| u64::try_from(at).ok())
        .map_or_else(|| "-".to_owned(), date)
}

/// The account `covered` as its vault lookup `found`, its table row `row` and whether a write of
/// it waits here (`pending`) make it at `now` (unix seconds), its offer where one stands on a host
/// whose verb makes a token ([`view`] keeps it only where [`Places::offers`]).
#[must_use]
pub fn account(
    covered: Covered,
    found: &Lookup<Standing>,
    row: Option<TokenRow>,
    pending: Option<bool>,
    now: u64,
) -> AccountToken {
    let state = TokenState::of(found, row.as_ref(), now);
    let login = if state.in_use() {
        LoginHere::Half(LoginHalf::InUse)
    } else {
        covered.login
    };
    let standing = match found {
        Lookup::Found(standing) => Some(standing.clone()),
        Lookup::Unknown | Lookup::Absent => None,
    };
    let token_since = match &standing {
        Some(standing) => standing.meta.as_ref().map(|meta| meta.minted_at),
        None => row.as_ref().and_then(|row| row.removed_at),
    };
    let pair = format!(
        "{}:{}/{}:{}",
        state.word(),
        since_word(token_since.and_then(|at| i64::try_from(at).ok())),
        login.word(),
        since_word(login.half().and_then(LoginHalf::since)),
    );
    let named = covered.label.as_deref().unwrap_or(&covered.email);
    let offer = covered
        .logged_in
        .then(|| state.offer(named, pending != Some(false)))
        .flatten()
        .map(|step| Offer {
            pair,
            step,
            since: None,
        });
    AccountToken {
        key: covered.key.as_str().to_owned(),
        email: covered.email,
        label: covered.label,
        state,
        standing,
        row,
        pending,
        logged_in: covered.logged_in,
        login,
        offer,
    }
}

/// Where the surfaces read the accounts from.
pub struct Places<'a> {
    /// Claude Code's accounts store.
    pub store: &'a Store,
    /// Its roster.
    pub roster: Option<&'a Path>,
    /// The accounts the sessions on this Mac are logged into.
    pub logins: &'a [SessionLogin],
    /// The clock (unix seconds).
    pub now: i64,
    /// Whether this host's verb makes a token ([`OFFERED_HERE`] for a surface with no host), so
    /// its surfaces offer the step; where it does not, they still say each token's state.
    pub offers: bool,
}

/// The accounts the surfaces cover (§5.9, §5.10): each roster row that names an email and that the
/// lab keys, then each account a session here is logged into that no row names; each once, by key.
#[must_use]
pub fn covered(places: &Places<'_>) -> Vec<Covered> {
    let mut out: Vec<Covered> = Vec::new();
    let records = places
        .store
        .read_account_records(places.roster, places.now)
        .ok();
    let logged_in = |email: &str| {
        places
            .logins
            .iter()
            .any(|login| accounts::email_matches(&login.email, email))
    };
    if let Some(records) = &records {
        let enrolled = places.store.enrolled_here(&records.roster);
        for row in &records.roster.rows {
            let (Some(email), Some(entry)) =
                (row.expected_identity_email(), TokenEntry::of_row(row))
            else {
                continue;
            };
            let Some(key) = Key::parse(entry.key().as_str()) else {
                continue;
            };
            if out.iter().any(|seen| seen.key == key) {
                continue;
            }
            let logged_in = places
                .logins
                .iter()
                .any(|login| row.matches_email(&login.email));
            // A login of the row's account is on this Mac where a session's login record names
            // it (the default login's among them) or the row's own directory holds one. A row
            // on the default login has no directory, and every such row counts as enrolled, so
            // only a login record says whether the default login is its account's.
            let holds = logged_in || (row.dir.is_some() && enrolled.contains(&row.label));
            out.push(Covered {
                key,
                email: email.to_owned(),
                label: Some(row.label.clone()),
                logged_in,
                login: LoginHere::of_row(
                    LoginHalf::of_row(row, &records.logins, &records.state, places.now),
                    holds,
                ),
            });
        }
    }
    for login in places.logins {
        let Some(key) =
            TokenEntry::of_email(&login.email).and_then(|entry| Key::parse(entry.key().as_str()))
        else {
            continue;
        };
        if out.iter().any(|seen| seen.key == key) {
            continue;
        }
        out.push(Covered {
            key,
            email: login.email.clone(),
            label: None,
            logged_in: logged_in(&login.email),
            login: LoginHere::Half(LoginHalf::Ok),
        });
    }
    out
}

/// Each covered account ([`covered`]) and each account this Mac's table keeps a row of, as one
/// read of `vault` and the table make it now, each with no offer where `places` offers none.
#[must_use]
pub fn view(places: &Places<'_>, vault: &dyn TokenVault) -> Vec<AccountToken> {
    let mut covered = covered(places);
    let table = tokens::read_table(places.store).unwrap_or_default();
    for row in table.rows.values() {
        let Some(key) = Key::parse(&row.key) else {
            continue;
        };
        if covered.iter().any(|seen| seen.key == key) {
            continue;
        }
        covered.push(Covered {
            key,
            email: row.email.clone(),
            label: row.label.clone(),
            logged_in: false,
            login: LoginHere::Unread,
        });
    }
    let listed = vault.listed().unwrap_or(Lookup::Unknown);
    let now = u64::try_from(places.now).unwrap_or(0);
    covered
        .into_iter()
        .map(|account_here| {
            let found = match &listed {
                Lookup::Unknown => Lookup::Unknown,
                Lookup::Absent => Lookup::Absent,
                Lookup::Found(standings) => standings
                    .iter()
                    .find(|standing| standing.key == account_here.key)
                    .map_or(Lookup::Absent, |standing| Lookup::Found(standing.clone())),
            };
            let pending = vault.waiting_here(&account_here.key).ok();
            let row = table.rows.get(account_here.key.as_str()).cloned();
            let mut shown = account(account_here, &found, row, pending, now);
            // The step makes a token only where aterm's wrapper serves one (the verb refuses
            // elsewhere), so no other host is offered it; its surfaces still say the state.
            if !places.offers {
                shown.offer = None;
            }
            shown
        })
        .collect()
}

/// RECORDS EACH STANDING OFFER ONCE PER PAIR (§5.9): an offer whose pair the table's `offers`
/// does not hold is written there at `now`, under `tokens.lock`, and journaled once as
/// `token-offered`; one it holds journals nothing. Each offer's `since` is filled from the table.
/// Writes nothing where every offer is already recorded. Returns how many it recorded.
///
/// # Errors
/// The table could not be read or written.
pub fn note_offers(store: &Store, accounts: &mut [AccountToken], now: u64) -> io::Result<usize> {
    let table = tokens::read_table(store)?;
    let fresh = |table: &tokens::Table, account: &AccountToken| {
        account.offer.as_ref().is_some_and(|offer| {
            table
                .offers
                .get(&account.key)
                .is_none_or(|was| was.pair != offer.pair)
        })
    };
    let mut noted: Vec<String> = Vec::new();
    if accounts.iter().any(|account| fresh(&table, account)) {
        tokens::update_table(store, |table| {
            for account in accounts.iter() {
                if fresh(table, account)
                    && let Some(offer) = &account.offer
                {
                    table.offers.insert(
                        account.key.clone(),
                        Offered {
                            pair: offer.pair.clone(),
                            at: now,
                        },
                    );
                    noted.push(account.key.clone());
                }
            }
        })?;
    }
    let table = if noted.is_empty() {
        table
    } else {
        tokens::read_table(store)?
    };
    for account in accounts.iter_mut() {
        if let Some(offer) = account.offer.as_mut() {
            offer.since = table
                .offers
                .get(&account.key)
                .filter(|was| was.pair == offer.pair)
                .map(|was| was.at);
        }
    }
    for key in &noted {
        if let Some(key) = Key::parse(key) {
            tokens::note(TokenNote::Offered, &key, now);
        }
    }
    Ok(noted.len())
}

/// How often a surface's read retries the post of a `signed here` row's write (§5.1).
pub const POST_EVERY_S: u64 = 60;

/// KEEPS "NOT YET IN VAULT" CURRENT (§5.1, §3.10): where a token this Mac made reads `signed
/// here` (signed into this Mac's log, and no server took it then) and the log holds it, the read
/// retries the idempotent post ([`TokenVault::post`]) at most once each [`POST_EVERY_S`] per row,
/// the try's time kept in the table's `posts` first; once a server takes the records, each such
/// row turns `in vault`, in the table and in `accounts`. Posts nothing where no such row is due.
/// Returns how many rows turned.
///
/// # Errors
/// The table could not be read or written.
pub fn note_posted(
    store: &Store,
    accounts: &mut [AccountToken],
    vault: &dyn TokenVault,
    now: u64,
) -> io::Result<usize> {
    let signed_here = |account: &AccountToken| {
        account.standing.is_some()
            && account
                .row
                .as_ref()
                .is_some_and(|row| row.vault == InVault::SignedHere)
    };
    if !accounts.iter().any(signed_here) {
        return Ok(0);
    }
    let table = tokens::read_table(store)?;
    let due: Vec<String> = accounts
        .iter()
        .filter(|account| signed_here(account))
        .filter(|account| {
            table
                .posts
                .get(&account.key)
                .is_none_or(|at| now < *at || now.saturating_sub(*at) >= POST_EVERY_S)
        })
        .map(|account| account.key.clone())
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    tokens::update_table(store, |table| {
        for key in &due {
            table.posts.insert(key.clone(), now);
        }
    })?;
    if !vault.post().unwrap_or(false) {
        return Ok(0);
    }
    let mut turned = 0;
    tokens::update_table(store, |table| {
        for key in &due {
            table.posts.remove(key);
            if let Some(row) = table.rows.get_mut(key)
                && row.vault == InVault::SignedHere
            {
                row.vault = InVault::InVault;
                turned += 1;
            }
        }
    })?;
    for account in accounts.iter_mut() {
        if due.contains(&account.key)
            && let Some(row) = account.row.as_mut()
            && row.vault == InVault::SignedHere
        {
            row.vault = InVault::InVault;
        }
    }
    Ok(turned)
}

/// Which peers use each token, from one fleet read (§5.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peers {
    /// No read answered: the fleet was offline, past its bound, or not asked.
    NotRead,
    /// A read at `at`: each account key's peers, by the name the read gave them.
    Read {
        /// When.
        at: u64,
        /// Each key's peers.
        by_key: BTreeMap<String, Vec<String>>,
    },
}

/// The peers each token's key names in `read` at `at`: each answering peer whose session rows
/// carry `token=<ac key>`. A read no peer answered is [`Peers::NotRead`]; a read of a fleet of
/// none is a read that names none.
#[must_use]
pub fn peers_of(read: &aterm_ctl::FleetRead, at: u64) -> Peers {
    if !read.peers.is_empty() && read.peers.iter().all(|peer| peer.answer.is_err()) {
        return Peers::NotRead;
    }
    let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for peer in &read.peers {
        let Ok(lines) = &peer.answer else {
            continue;
        };
        let name = peer.node.clone().unwrap_or_else(|| peer.label.clone());
        for line in lines.iter().filter(|line| line.starts_with("session ")) {
            let Some(key) = line
                .split(' ')
                .find_map(|kv| kv.strip_prefix("token="))
                .filter(|key| key.starts_with("ac-") && Key::parse(key).is_some())
            else {
                continue;
            };
            let peers = by_key.entry(key.to_owned()).or_default();
            if !peers.contains(&name) {
                peers.push(name.clone());
            }
        }
    }
    Peers::Read { at, by_key }
}

/// Writes a fleet read's peers into this Mac's table rows (`used_on.peers`, `peers_read_at`),
/// never into the vault; nothing where the read did not answer or nothing changed.
///
/// # Errors
/// The table could not be read or written.
pub fn note_peers(store: &Store, peers: &Peers) -> io::Result<()> {
    let Peers::Read { at, by_key } = peers else {
        return Ok(());
    };
    let table = tokens::read_table(store)?;
    if table.rows.is_empty() {
        return Ok(());
    }
    tokens::update_table(store, |table| {
        for row in table.rows.values_mut() {
            row.used_on.peers = by_key.get(&row.key).cloned().unwrap_or_default();
            row.used_on.peers_read_at = Some(*at);
        }
    })
}

/// What the report says of peers' use: a read this run, or none and the last one's time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeersShown {
    /// Read this run.
    Read,
    /// Not read this run.
    NotRead,
}

impl PeersShown {
    const fn read(self) -> bool {
        matches!(self, Self::Read)
    }

    fn words(self, row: &TokenRow) -> String {
        let peers = if row.used_on.peers.is_empty() {
            "no peer".to_owned()
        } else {
            row.used_on.peers.join(", ")
        };
        match (self, row.used_on.peers_read_at) {
            (Self::Read, _) => format!("used on peers: {peers}"),
            (Self::NotRead, Some(at)) => {
                format!("peers not read (last read {}: {peers})", date(at))
            }
            (Self::NotRead, None) => "peers not read".to_owned(),
        }
    }
}

/// One Claude Code session on this Mac as its aterm's survey row names it: its sid, the row it is
/// on (masked), how it authenticates, its token's key on the token route, and the account its
/// login record names ([`with_logins`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRoute {
    /// The session's sid.
    pub sid: String,
    /// The roster row it is on, as the survey masks it.
    pub account: Option<String>,
    /// `stored`, `token` or `outside`.
    pub auth: String,
    /// The token's account key, on the token route.
    pub token: Option<String>,
    /// The lab account key of the account its login record names, once [`with_logins`] read it.
    pub login: Option<String>,
}

/// Each of `sessions` with the account its login record names (§5.2, §5.10): on the token route
/// the token's; on a row, that row's, as `accounts` names it; on no row, the machine's default
/// login record's, which every such session runs on (`logins`' [`SessionLogin::default`]).
#[must_use]
pub fn with_logins(
    mut sessions: Vec<SessionRoute>,
    accounts: &[AccountToken],
    logins: &[SessionLogin],
) -> Vec<SessionRoute> {
    let default = logins
        .iter()
        .find(|login| login.default)
        .and_then(|login| TokenEntry::of_email(&login.email))
        .map(|entry| entry.key().as_str().to_owned());
    for session in &mut sessions {
        session.login = match (&session.token, &session.account) {
            (Some(key), _) => Some(key.clone()),
            (None, Some(label)) => accounts
                .iter()
                .find(|account| account.label.as_deref() == Some(label.as_str()))
                .map(|account| account.key.clone()),
            (None, None) => default.clone(),
        };
    }
    sessions
}

/// The Claude Code sessions a survey's `lines` name with an `auth=` (an older aterm's rows carry
/// none, and are left out).
#[must_use]
pub fn sessions_of(lines: &[String]) -> Vec<SessionRoute> {
    lines
        .iter()
        .filter(|line| line.starts_with("session "))
        .filter_map(|line| {
            let field = |key: &str| {
                line.split(' ')
                    .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
                    .filter(|value| !value.is_empty() && *value != "-")
            };
            Some(SessionRoute {
                sid: field("sid")?.to_owned(),
                account: field("account").map(str::to_owned),
                auth: field("auth")?.to_owned(),
                token: field("token").map(str::to_owned),
                login: None,
            })
        })
        .collect()
}

/// The report's token section (§5.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// This Mac has no vault to read.
    NoVault,
    /// The accounts, whether peers were read, and the sessions this aterm's survey named.
    Accounts {
        /// Each account.
        accounts: Vec<AccountToken>,
        /// Whether peers were read this run.
        peers: PeersShown,
        /// This aterm's Claude Code sessions, where its survey was read.
        sessions: Option<Vec<SessionRoute>>,
    },
}

impl Report {
    /// The section's text, appended to the report.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::NoVault => "vault tokens: this Mac has no vault to read\n".to_owned(),
            Self::Accounts {
                accounts,
                peers,
                sessions,
            } => {
                let mut text = String::from("vault tokens:\n");
                if accounts.is_empty() {
                    text.push_str("  no account a session here is on names an email\n");
                }
                for account in accounts {
                    text.push_str(&account.text(peers));
                    text.push('\n');
                }
                if let Some(sessions) = sessions.as_ref().filter(|s| !s.is_empty()) {
                    let words: Vec<String> = sessions
                        .iter()
                        .map(|session| {
                            let on = session
                                .account
                                .as_deref()
                                .map_or_else(String::new, |on| format!(" on {on}"));
                            let login = match login_of(session, accounts) {
                                (Some(key), Some(account)) => format!(
                                    " (logged into {} {key}, token: {})",
                                    account.email,
                                    account.state.word()
                                ),
                                (Some(key), None) => format!(" (logged into {key})"),
                                (None, _) => String::new(),
                            };
                            format!("@{}{on} {}{login}", session.sid, session.auth)
                        })
                        .collect();
                    text.push_str(&format!("  sessions: {}\n", words.join("; ")));
                }
                text
            }
        }
    }

    /// The section as the report's JSON carries it under `tokens`.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut o = Map::new();
        match self {
            Self::NoVault => {
                o.insert("vault".into(), Value::Null);
            }
            Self::Accounts {
                accounts,
                peers,
                sessions,
            } => {
                o.insert(
                    "accounts".into(),
                    Value::Array(accounts.iter().map(|a| a.json(peers)).collect()),
                );
                o.insert("peers_read".into(), Value::from(peers.read()));
                o.insert(
                    "sessions".into(),
                    sessions.as_ref().map_or(Value::Null, |sessions| {
                        Value::Array(
                            sessions
                                .iter()
                                .map(|session| {
                                    let mut s = Map::new();
                                    s.insert("sid".into(), Value::from(session.sid.clone()));
                                    s.insert(
                                        "account".into(),
                                        Value::from(session.account.clone()),
                                    );
                                    s.insert("route".into(), Value::from(session.auth.clone()));
                                    s.insert("token".into(), Value::from(session.token.clone()));
                                    let login = match login_of(session, accounts) {
                                        (Some(key), account) => {
                                            let mut l = Map::new();
                                            l.insert("key".into(), Value::from(key.to_owned()));
                                            l.insert(
                                                "email".into(),
                                                Value::from(account.map(|a| a.email.clone())),
                                            );
                                            l.insert(
                                                "token".into(),
                                                Value::from(account.map(|a| a.state.word())),
                                            );
                                            Value::Object(l)
                                        }
                                        (None, _) => Value::Null,
                                    };
                                    s.insert("login".into(), login);
                                    Value::Object(s)
                                })
                                .collect(),
                        )
                    }),
                );
            }
        }
        Value::Object(o)
    }
}

/// The account key `session`'s login record names, and that account among `accounts`.
fn login_of<'a>(
    session: &'a SessionRoute,
    accounts: &'a [AccountToken],
) -> (Option<&'a str>, Option<&'a AccountToken>) {
    let key = session.login.as_deref();
    (
        key,
        key.and_then(|key| accounts.iter().find(|account| account.key == key)),
    )
}

/// The doctor's words for the accounts (§5.10): how many tokens here are in each state that
/// says something, or `unknown (vault not read)` where the vault's read failed, so no account's
/// standing is known and the counts would read as a vault that holds none.
#[must_use]
pub fn doctor_words(accounts: &[AccountToken]) -> String {
    if accounts.iter().any(|a| a.state == TokenState::Unknown) {
        return "claude tokens: unknown (vault not read)".to_owned();
    }
    let count = |want: &dyn Fn(&AccountToken) -> bool| accounts.iter().filter(|a| want(a)).count();
    format!(
        "claude tokens: {} active, {} soon, {} unverified, {} not yet in vault, {} rejected",
        count(&|a| a.state == TokenState::Active),
        count(&|a| a.state == TokenState::Soon),
        count(&|a| matches!(a.state, TokenState::Unverified { .. })),
        count(&AccountToken::not_yet),
        count(&|a| a.state == TokenState::Rejected),
    )
}

/// Each roster row's token state word ([`TokenState::word`]) by label, for a survey's account
/// lines: the rows of `places` as one read of `vault` makes them.
#[must_use]
pub fn states_by_label(places: &Places<'_>, vault: &dyn TokenVault) -> BTreeMap<String, String> {
    view(places, vault)
        .into_iter()
        .filter_map(|account| Some((account.label?, account.state.word().to_owned())))
        .collect()
}

/// The token view of this Mac's own Claude Code accounts for a surface that has no host: its
/// store and roster beside `config`, the login records under `home`, its vault; `None` where no
/// home is known or no vault is configured under it ([`tokens::configured_vault`]), so a Mac with
/// no vault says nothing of tokens (a vault whose read fails reads `unknown`, [`doctor_words`]).
/// A `signed here` row's post is retried as the read goes ([`note_posted`]).
#[must_use]
pub fn view_here(
    harness: &Path,
    config: Option<&Path>,
    home: Option<&Path>,
    now: i64,
) -> Option<Vec<AccountToken>> {
    let home = home?;
    let store = Store::at(harness);
    let roster = config.and_then(|config| accounts::roster_beside_for(config, Cli::Claude));
    let vault = tokens::configured_vault(aterm_secrets::client::Paths::for_home(home))?;
    let logins = tokens::session_logins_at(Some(&store), roster.as_deref(), Some(home), now);
    let mut accounts = view(
        &Places {
            store: &store,
            roster: roster.as_deref(),
            logins: &logins,
            now,
            offers: OFFERED_HERE,
        },
        &vault,
    );
    let _ = note_posted(
        &store,
        &mut accounts,
        &vault,
        u64::try_from(now).unwrap_or(0),
    );
    Some(accounts)
}

#[cfg(test)]
#[path = "accounts_token_view_tests.rs"]
mod tests;
