// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault token facts the window shows (`docs/DESIGN-claude-setup-token-2026-11-05.md`
//! §5.9, §5.10, slice S12): one value-free read of this Mac's Claude Code accounts
//! ([`aterm_agent::harness::accounts_token_view::view_here`]) on the footer's resolver thread, at
//! most once per [`READ_EVERY_S`], posted as [`Tokens`] for the Account card, the Lab page, the
//! harness page's state line and `ctl status`, which map it without reading a file.
//!
//! A fact keeps what the view typed (its state, its dates, the Mac that made it, the login half)
//! beside the words, so a surface chooses its own words and never parses another's; and a read
//! says whether this Mac has a vault at all ([`TokensRead`]), so "no vault here" is not the same
//! answer as "no token for this account".
//!
//! The read records each new offer pair (`note_offers`), so the quiet offer is journaled once per
//! change of the pair whichever surface shows it first. Its one step ([`App::open_token_step`])
//! opens a tab beside the person's that runs `aterm harness accounts token <label> [--replace]`;
//! nothing here raises a modal, a banner or a notification. A platform with no vault tokens reads
//! none, and every surface then says nothing of them.

use std::sync::Arc;

use aterm_agent::harness::account_card::LoginHalf;
use aterm_agent::harness::token_state::TokenState;

/// The least time between two reads of the token view.
pub(crate) const READ_EVERY_S: i64 = 60;

/// The quiet offer's one step, as the window runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenOffer {
    /// The card's and the Lab page's one line (`a vault token would keep work signed in: make one`).
    pub(crate) line: String,
    /// The verb's account: the row's label, or the account's email.
    pub(crate) label: String,
    /// `--replace`: the token the step offers for is still in use (`soon`, `exposed`).
    pub(crate) replace: bool,
}

impl TokenOffer {
    /// The step's words after `aterm`.
    #[must_use]
    pub(crate) fn words(&self) -> Vec<&str> {
        let mut words = vec!["harness", "accounts", "token", self.label.as_str()];
        if self.replace {
            words.push("--replace");
        }
        words
    }
}

/// One account's token, value-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenFact {
    /// The lab account key (`ac-…`).
    pub(crate) key: String,
    /// The roster row's label, where one names the account.
    pub(crate) label: Option<String>,
    /// The account's email.
    pub(crate) email: String,
    /// The state's one word (`active`, `soon`, `none`, …).
    pub(crate) state: String,
    /// The state and its value-free detail, and "not yet in vault" while a write made here waits.
    pub(crate) words: String,
    /// The state line's phrase while an offer stands (`login ends in 5 days; token: none`).
    pub(crate) phrase: Option<String>,
    /// The offer, where one stands.
    pub(crate) offer: Option<TokenOffer>,
    /// The token's state as the view typed it (§5.1), which [`Self::state`] and [`Self::words`]
    /// say.
    pub(crate) kind: TokenState,
    /// When the token was made (unix seconds), where the vault's meta or this Mac's row says.
    pub(crate) made_at: Option<u64>,
    /// When it ends.
    pub(crate) ends_at: Option<u64>,
    /// The Mac that made it, as its meta names it.
    pub(crate) made_on: Option<String>,
    /// When the Mac that made it verified it, where this Mac's row holds that.
    pub(crate) verified_at: Option<u64>,
    /// A token made here is not in the vault yet: its write waits, or no server took it.
    pub(crate) not_yet: bool,
    /// The account's stored login on this Mac, the offer pair's other half.
    pub(crate) login: LoginHalf,
    /// A session on this Mac is logged into the account.
    pub(crate) logged_in: bool,
    /// The vault column as a person reads it (`in vault`, `not yet in vault (waiting: …)`).
    pub(crate) vault_words: Option<String>,
    /// The other Macs whose sessions ran on the token at the last fleet read.
    pub(crate) peers: Vec<String>,
    /// When that fleet read was.
    pub(crate) peers_read_at: Option<u64>,
}

#[cfg(test)]
impl TokenFact {
    /// A test's fact of the account `email` (row `label`) whose token stands at `kind`: its word
    /// and words the state's own, no dates, no offer, a login with no end known.
    pub(crate) fn for_test(label: Option<&str>, email: &str, kind: TokenState) -> Self {
        let account = "ac-fedcba9876543210";
        Self {
            key: account.into(),
            label: label.map(str::to_owned),
            email: email.into(),
            state: kind.word().to_owned(),
            words: kind.words(),
            phrase: None,
            offer: None,
            kind,
            made_at: None,
            ends_at: None,
            made_on: None,
            verified_at: None,
            not_yet: false,
            login: LoginHalf::Ok,
            logged_in: false,
            vault_words: None,
            peers: Vec::new(),
            peers_read_at: None,
        }
    }
}

/// What a read of the token view could say at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokensRead {
    /// This Mac has no vault to hold a token (none configured under its home, no home, or a
    /// platform with no vault tokens): the read covers no account, and that is not "no token".
    NoVault,
    /// The vault's view was read: [`Tokens::facts`] are the accounts it covers.
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only a Unix read has a vault to cover: elsewhere every read is NoVault"
        )
    )]
    Read,
}

/// The window's token facts: whether the last read had a vault to read, and the accounts it
/// covered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tokens {
    /// Whether there was a vault to read.
    pub(crate) read: TokensRead,
    /// The accounts the read covered; none with no vault.
    pub(crate) facts: Vec<TokenFact>,
}

impl Tokens {
    /// A read of a vault's view that covered `facts`.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) const fn read(facts: Vec<TokenFact>) -> Self {
        Self {
            read: TokensRead::Read,
            facts,
        }
    }

    /// A read on a Mac with no vault.
    #[must_use]
    pub(crate) const fn no_vault() -> Self {
        Self {
            read: TokensRead::NoVault,
            facts: Vec::new(),
        }
    }

    /// The account whose row is `label`, else whose email is `email`.
    #[must_use]
    pub(crate) fn of(&self, label: Option<&str>, email: Option<&str>) -> Option<&TokenFact> {
        let facts = &self.facts;
        label
            .and_then(|label| facts.iter().find(|f| f.label.as_deref() == Some(label)))
            .or_else(|| email.and_then(|email| facts.iter().find(|f| f.email == email)))
    }

    /// The account of a tab whose footer says `facts`: its roster row's label, else its email.
    #[must_use]
    pub(crate) fn of_footer(
        &self,
        facts: Option<&aterm_agent::harness::footer::FooterFacts>,
    ) -> Option<&TokenFact> {
        let (label, email) = footer_account(facts);
        self.of(label, email)
    }

    /// The account keyed `key`.
    #[must_use]
    pub(crate) fn of_key(&self, key: &str) -> Option<&TokenFact> {
        self.facts.iter().find(|f| f.key == key)
    }
}

/// The roster row's label and the email of a tab whose footer says `facts`.
#[must_use]
pub(crate) fn footer_account(
    facts: Option<&aterm_agent::harness::footer::FooterFacts>,
) -> (Option<&str>, Option<&str>) {
    let sub = facts.and_then(|f| f.subscription.as_ref());
    (
        sub.and_then(|s| s.label.as_deref())
            .or_else(|| facts.and_then(|f| f.account.as_deref())),
        sub.and_then(|s| s.email.as_deref()),
    )
}

/// `ctl status`'s `auth=<stored|token|outside|-> token=<state|->` for a tab whose footer says
/// `route` (`None`: another program, or before its footer read the process), whose account row
/// is `label` and email `email` (§5.10): on the token route the token's own account, else the
/// tab's account.
#[must_use]
pub(crate) fn status_fields(
    route: Option<&aterm_agent::harness::accounts::AuthRoute>,
    (label, email): (Option<&str>, Option<&str>),
    tokens: Option<&Tokens>,
) -> String {
    let (auth, _) = crate::survey::auth_fields(route.cloned());
    let token = match (route, tokens) {
        (None, _) | (_, None) => "-",
        (Some(aterm_agent::harness::accounts::AuthRoute::VaultToken { entry }), Some(tokens)) => {
            tokens
                .of_key(entry.key().as_str())
                .map_or("-", |fact| fact.state.as_str())
        }
        (Some(_), Some(tokens)) => tokens
            .of(label, email)
            .map_or("-", |fact| fact.state.as_str()),
    };
    format!("auth={auth} token={token}")
}

#[cfg(not(test))]
static SHOWN: std::sync::Mutex<(Option<i64>, Option<Arc<Tokens>>)> =
    std::sync::Mutex::new((None, None));

#[cfg(test)]
thread_local! {
    /// A test's own facts: tests run side by side, and the resolver's read is a no-op in them.
    static SHOWN: std::cell::RefCell<Option<Arc<Tokens>>> = const { std::cell::RefCell::new(None) };
    /// Each step a press asked for, as its words after `aterm`.
    pub(crate) static STEPS_ASKED: std::cell::RefCell<Vec<Vec<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The facts the resolver last posted; `None` before its first read.
#[must_use]
pub(crate) fn shown() -> Option<Arc<Tokens>> {
    #[cfg(not(test))]
    {
        SHOWN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .clone()
    }
    #[cfg(test)]
    {
        SHOWN.with(|s| s.borrow().clone())
    }
}

/// A test's facts on its own thread.
#[cfg(test)]
pub(crate) fn show(tokens: Option<Tokens>) {
    SHOWN.with(|s| *s.borrow_mut() = tokens.map(Arc::new));
}

/// Read the token view again where [`READ_EVERY_S`] has passed since the last read at wall
/// second `now` (the footer's resolver thread asks on each Claude Code resolve); a move posts
/// the new facts and wakes the open Account card. A no-op in a unit test, which posts its own.
pub(crate) fn refresh(now: i64) {
    #[cfg(test)]
    let _ = now;
    #[cfg(not(test))]
    {
        let due = {
            let mut shown = SHOWN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let due = due(shown.0, now);
            if due {
                shown.0 = Some(now);
            }
            due
        };
        if !due {
            return;
        }
        // With no harness state there is nothing to read from: the facts stay as they were
        // (not read yet, before a first read), never "no vault".
        let Some(read) = read(now).map(Arc::new) else {
            return;
        };
        let moved = {
            let mut shown = SHOWN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let moved = shown.1.as_deref() != Some(&*read);
            shown.1 = Some(read);
            moved
        };
        if moved {
            crate::accounts_outlook::wake_card();
        }
    }
}

/// Whether a read is owed at wall second `now` after one at `last`: [`READ_EVERY_S`] passed, or
/// the clock stepped back.
fn due(last: Option<i64>, now: i64) -> bool {
    last.is_none_or(|at| now.saturating_sub(at) >= READ_EVERY_S || now < at)
}

/// One read of this Mac's token view at `now`, with each new offer pair recorded.
#[cfg(all(unix, not(test)))]
fn read(now: i64) -> Option<Tokens> {
    let harness = crate::harness_host::harness_state()?;
    let config = crate::app_config::config_path();
    let home = aterm_types::dirs::home_dir();
    Some(facts_of_view(
        &harness,
        aterm_agent::harness::accounts_token_view::view_here(
            &harness,
            config.as_deref(),
            home.as_deref(),
            now,
        ),
        now,
    ))
}

/// A platform with no vault tokens has no vault to read.
#[cfg(all(not(unix), not(test)))]
fn read(_now: i64) -> Option<Tokens> {
    Some(Tokens::no_vault())
}

/// The window's facts of a token view (`None`: no home or no vault configured under it, so
/// [`TokensRead::NoVault`]), each new offer pair recorded in the store under `harness`.
#[cfg(unix)]
pub(crate) fn facts_of_view(
    harness: &std::path::Path,
    view: Option<Vec<aterm_agent::harness::accounts_token_view::AccountToken>>,
    now: i64,
) -> Tokens {
    use aterm_agent::harness::accounts_step::Step;
    let Some(mut accounts) = view else {
        return Tokens::no_vault();
    };
    let store = aterm_agent::harness::accounts::Store::at(harness);
    if let Err(why) = aterm_agent::harness::accounts_token_view::note_offers(
        &store,
        &mut accounts,
        u64::try_from(now).unwrap_or(0),
    ) {
        aterm_log::warn!("accounts: the token offers could not be recorded: {why}");
    }
    Tokens::read(
        accounts
            .iter()
            .map(|account| {
                let mut words = account.state.words();
                if account.not_yet() {
                    words.push_str("; not yet in vault");
                }
                let used = account.row.as_ref().map(|row| &row.used_on);
                TokenFact {
                    kind: account.state.clone(),
                    made_at: account.minted_at(),
                    ends_at: account.expires_at(),
                    made_on: account.minted_on(),
                    verified_at: account.row.as_ref().and_then(|row| row.verified),
                    not_yet: account.not_yet(),
                    login: account.login,
                    logged_in: account.logged_in,
                    vault_words: account.vault_words(),
                    peers: used.map(|used| used.peers.clone()).unwrap_or_default(),
                    peers_read_at: used.and_then(|used| used.peers_read_at),
                    key: account.key.clone(),
                    label: account.label.clone(),
                    email: account.email.clone(),
                    state: account.state.word().to_owned(),
                    words,
                    phrase: account.phrase(now),
                    offer: account.offer.as_ref().and_then(|offer| match &offer.step {
                        Step::Token { label, replace } => Some(TokenOffer {
                            line: account.offer_line()?,
                            label: label.clone(),
                            replace: *replace,
                        }),
                        _ => None,
                    }),
                }
            })
            .collect(),
    )
}

impl crate::App {
    /// The Lab page's press on account row `index`'s offer: its step, as the page last showed
    /// it ([`crate::lab_accounts::LabAccount::offer`]); a row with none opens nothing.
    pub(crate) fn lab_token_step(&mut self, window: crate::WindowId, index: usize) {
        let offer = self.lab_screen.account_offer(index);
        if let Some(offer) = offer {
            self.open_token_step(window, &offer);
        }
    }

    /// The quiet offer's one step: a tab beside the person's in `window` that runs `aterm
    /// harness accounts token <label> [--replace]` (§5.9). Nothing else is raised. A platform
    /// with no vault tokens offers no step, and opens nothing.
    pub(crate) fn open_token_step(&mut self, window: crate::WindowId, offer: &TokenOffer) {
        let words = offer.words();
        #[cfg(test)]
        STEPS_ASKED.with(|asked| {
            asked
                .borrow_mut()
                .push(words.iter().map(|w| (*w).to_owned()).collect());
        });
        #[cfg(not(unix))]
        let _ = (window, words);
        #[cfg(unix)]
        self.run_token_step(window, &words);
    }

    #[cfg(unix)]
    fn run_token_step(&mut self, window: crate::WindowId, words: &[&str]) {
        if words.iter().any(|word| word.chars().any(char::is_control)) {
            return;
        }
        let quoted: Vec<String> = words
            .iter()
            .map(|word| {
                if word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.@+".contains(c))
                {
                    (*word).to_owned()
                } else {
                    format!("'{}'", word.replace('\'', "'\\''"))
                }
            })
            .collect();
        let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
        let Some(line) = crate::ledger_key::own_front_door()
            .and_then(|exe| crate::ai_screen::launch_line(&exe, &quoted))
        else {
            aterm_log::warn!("accounts: this program path has no token step invocation");
            return;
        };
        aterm_log::info!("accounts: a tab opens to run `aterm {}`", words.join(" "));
        let _act = crate::tab_marks::ActScope::gesture("token");
        self.open_off_loop(
            window,
            crate::tab_spawn::SpawnPlace::Tab,
            crate::tab_spawn::SpawnAsk::default(),
            crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| match landed {
                Ok(sid) => app.type_into_new_session(&sid, &line),
                Err(why) => aterm_log::warn!("accounts: the token tab did not open: {why}"),
            })),
        );
    }
}

#[cfg(test)]
#[path = "token_shown_tests.rs"]
mod tests;
