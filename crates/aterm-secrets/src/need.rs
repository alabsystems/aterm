// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CREDENTIAL REQUESTS ON THIS MAC (docs/DESIGN-credential-requests-2026-10-27.md, step 1): a
//! consumer that lacks a credential raises a *need* ([`raise`]); the need ledger
//! (`~/.config/aterm/secrets/needs.toml`, 0600, a path no vault entry can name) keeps it with a
//! request id fixed before anything is posted; the [`catalog`] answers at once the wants no Mac
//! can give (a vendor login, this Mac's own keys); the absence proof over this Mac's applied
//! envelope ([`Sight`]) shows what the vault already gives this Mac; every other need is posted
//! to the lab's Macs and names the one party it waits on ([`Wait`]). No need is ever handed to a
//! person: one no Mac holds retires `not-held`, the blocked operation its line records. Nothing
//! here reads a secret's value: the sight reads entry names, the hold probe stats a path.
//!
//! The ledger's moves are pure ([`act`], [`settle`]) over what this Mac observes ([`Facts`]),
//! so the conformance test can drive them from every reachable state of
//! `aterm_models_db::credential_request_model`; [`raise`], [`satisfied`], [`retire`] and
//! [`list`] are those moves under the ledger's lock.

pub mod catalog;
pub mod custody;
pub mod full;
mod ledger;
pub mod transfer;

pub use crate::client::version::RefusalClaim;
pub use catalog::{Class, GhProfile, Purpose, Want};
pub use ledger::{
    FILE as LEDGER, absorb, acknowledge_from_snapshot, ask_for_tokens, list, peek, raise,
    raise_refused, raise_refused_from_snapshot, raise_rotations, raise_with_admission, retire,
    rotated, satisfied, unneeded_from_snapshot, with_ledger,
};

use crate::commons::Unavailable;
use std::collections::BTreeSet;

/// The want window: how long the lab has to answer a request before the custodian decides
/// (§6.2).
pub const WANT_WINDOW_SECS: u64 = 72 * 3600;

/// How long a posted request may go with no custodian record before this Mac reads the
/// custodian as silent on it ([`silent`]): the want window and a day more, since a custodian's
/// window runs from when its own replica took the request in, and its decision takes a pass and
/// a sync to come back.
pub const SILENCE_SECS: u64 = WANT_WINDOW_SECS + 86_400;

/// A requester's silence that lets a custodian retire its request (`lapsed`, §6.2).
pub const LAPSE_SECS: u64 = 30 * 86_400;

/// How old a requester's newest record on a waiting request may grow before it writes `Still`.
pub const STILL_SECS: u64 = 7 * 86_400;

/// The most open requests one requester holds in each [`pool`]; past that, its records are inert.
pub const MAX_OPEN: usize = 16;

/// The pool of open requests [`MAX_OPEN`] counts a request for `want` in: 1 for a want with a
/// producer ([`Want::producer`]), whose request waits for as long as a person takes to make it, 0
/// for every other want. Each pool has its own [`MAX_OPEN`], so a requester's waiting tokens
/// never hold back its other requests. A table reader that does not know a want with a producer
/// leaves its request inert, so it counts in neither.
#[must_use]
pub fn pool(want: &Want) -> usize {
    usize::from(want.producer().is_some())
}

/// The longest note a `Retired` record carries (§5).
pub const MAX_NOTE: usize = 160;

/// Where a need stands (the model's `st`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// In the need ledger, nothing posted.
    Local,
    /// Posted to Commons; holders and the custodian read it.
    Open,
    /// A holder offered it; the custodian takes it.
    Offered,
    /// The vault gives it to this Mac; its consumer's check comes next.
    Published,
    /// Its consumer's check passed after the vault delivered it.
    Fulfilled,
    /// Ended, with a reason any member can check.
    Retired,
}

impl State {
    /// Its word in the ledger and in `need ls`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Open => "open",
            Self::Offered => "offered",
            Self::Published => "published",
            Self::Fulfilled => "fulfilled",
            Self::Retired => "retired",
        }
    }

    /// The state `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::Local,
            Self::Open,
            Self::Offered,
            Self::Published,
            Self::Fulfilled,
            Self::Retired,
        ]
        .into_iter()
        .find(|state| state.word() == word)
    }

    /// Whether nothing moves it any more (fulfilled or retired).
    #[must_use]
    pub const fn ended(self) -> bool {
        matches!(self, Self::Fulfilled | Self::Retired)
    }
}

/// Why a request ended (§6.2; the model's `why`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// An earlier build's: the want existed on no computer (the paper master, before the vault
    /// held it). The lab's table keeps such records, so the word still reads; nothing writes it
    /// now.
    NeverStored,
    /// Each Mac makes its own.
    NeverCopied,
    /// The policy in force does not give it to this Mac.
    NotAuthorized,
    /// Its consumer no longer needs it (or the requester withdrew it).
    NotNeeded,
    /// No Mac allowed to give it holds it.
    NotHeld,
    /// An earlier build's: a person declined it. The lab's table keeps such records, so the word
    /// still reads; nothing writes it now.
    Declined,
    /// The vault no longer names the requester.
    RequesterGone,
    /// The requester wrote nothing on it for 30 days.
    Lapsed,
    /// A newer request of the requester replaced it.
    Superseded,
}

impl Reason {
    /// Whether a need it retired may be raised again once this Mac's facts change ([`changed`]):
    /// `not-needed` and `lapsed` always may, `not-authorized` under a newer policy and `not-held`
    /// once a member offers it; the rest never.
    #[must_use]
    pub const fn may_change(self) -> bool {
        matches!(
            self,
            Self::NotNeeded | Self::Lapsed | Self::NotAuthorized | Self::NotHeld
        )
    }

    /// Every reason.
    pub const ALL: [Self; 9] = [
        Self::NeverStored,
        Self::NeverCopied,
        Self::NotAuthorized,
        Self::NotNeeded,
        Self::NotHeld,
        Self::Declined,
        Self::RequesterGone,
        Self::Lapsed,
        Self::Superseded,
    ];

    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::NeverStored => "never-stored",
            Self::NeverCopied => "never-copied",
            Self::NotAuthorized => "not-authorized",
            Self::NotNeeded => "not-needed",
            Self::NotHeld => "not-held",
            Self::Declined => "declined",
            Self::RequesterGone => "requester-gone",
            Self::Lapsed => "lapsed",
            Self::Superseded => "superseded",
        }
    }

    /// The reason `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.word() == word)
    }
}

/// The one party that moves a need that has not ended (the model's `wait`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Wait {
    /// Ended: nobody.
    Nobody,
    /// Holders: a member that holds it offers it.
    Holders,
    /// The custodian: the vault's admin aterm decides or publishes.
    Custodian,
    /// This Mac: it posts the request, or its consumer checks again.
    Requester,
    /// The founding: no Mac can be asked until Commons runs and this Mac writes to it.
    Founding,
}

impl Wait {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Nobody => "nobody",
            Self::Holders => "holders",
            Self::Custodian => "custodian",
            Self::Requester => "requester",
            Self::Founding => "founding",
        }
    }

    /// The wait `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::Nobody,
            Self::Holders,
            Self::Custodian,
            Self::Requester,
            Self::Founding,
        ]
        .into_iter()
        .find(|wait| wait.word() == word)
    }
}

/// A retirement's reason and what it rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    /// Why it ended.
    pub reason: Reason,
    /// What the reason rests on: `catalog <n>`, `policy <generation>`, a vault seq, a record id.
    pub evidence: String,
    /// At most 160 characters, never a value.
    pub note: String,
    /// The policy generation a `not-authorized` cites (the model's `atgen`).
    pub generation: Option<u64>,
}

/// One need in the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    /// The request id: 16 random bytes, fixed before the first attempt.
    pub id: [u8; 16],
    /// What is wanted.
    pub want: Want,
    /// Whose need it is.
    pub purpose: Purpose,
    /// Where it stands.
    pub state: State,
    /// Who moves it.
    pub wait: Wait,
    /// How it ended, once retired.
    pub retired: Option<Retirement>,
    /// Unix seconds it was first raised.
    pub raised: u64,
    /// Unix seconds of its last move.
    pub updated: u64,
    /// How many times this want was raised for this purpose (the model's `raises`).
    pub raises: u32,
    /// The actual authenticated source version refused, or an explicit legacy scalar floor.
    pub refused: Option<RefusalClaim>,
    /// The consumer's word that it still needs the want (the model's `need`).
    pub needed: bool,
    /// The value reached this Mac (the model's `got`).
    pub got: bool,
    /// The policy gave it to this Mac when it was published (the model's `pa`).
    pub got_authorized: bool,
    /// When raised again, whether the reason's condition had changed (the model's `okr`).
    pub raised_again_on_change: Option<bool>,
    /// Posted to Commons (the model's `posted`, step 2).
    pub posted: bool,
    /// [`SILENCE_SECS`] passed with no custodian record on the posted request ([`silent`]).
    pub custodian_silent: bool,
    /// The posted request's operation digest in `credentials/requests/1`. A second flush
    /// repeats the same command under the same retry identity, so the table returns this
    /// receipt rather than a second request.
    pub record: Option<Vec<u8>>,
    /// The one line that names what it waits on, or how it ended.
    pub line: String,
    /// The vault seq the line is stated at.
    pub seq: Option<u64>,
    /// The vault sections (`[everyone]`, `[m3 m22]`) a stolen Mac read the entry in, each
    /// still holding the value it took until a new value is published there; empty for every
    /// purpose but [`Purpose::StolenRotation`].
    pub exposed: Vec<String>,
}

/// A withdrawal by hand's evidence, before the Mac it was withdrawn on ([`Act::Withdraw`]).
const WITHDRAWN_BY_HAND: &str = "withdrawn by hand on ";

impl Need {
    /// Whether a person withdrew it by hand (`aterm secrets need retire`, [`retire`]): retired
    /// `not-needed` on their word, not on a check. [`raise`] asks again as for any `not-needed`;
    /// [`ask_for_tokens`], which asks on each read of the harness's offers, leaves it standing,
    /// and the ledger prunes it after every other ended row.
    #[must_use]
    pub fn withdrawn_by_hand(&self) -> bool {
        self.state == State::Retired
            && self.retired.as_ref().is_some_and(|retired| {
                retired.reason == Reason::NotNeeded
                    && retired.evidence.starts_with(WITHDRAWN_BY_HAND)
            })
    }

    /// Whether `selector`, a want key or a ledger id in hex, names it (`need retire`).
    #[must_use]
    pub fn named(&self, selector: &str) -> bool {
        self.want.key() == selector || aterm_codec::hex::encode_lower(&self.id) == selector
    }
}

/// What this Mac's applied vault version shows: entry names and authenticated public history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sight {
    /// The vault's name.
    pub vault: String,
    /// This Mac's vault host name, from its own join (never `machine.toml`).
    pub host: String,
    /// The applied version.
    pub seq: u64,
    /// The vault signing key that version is signed by, authenticated through this Mac's pin
    /// and the handoff chain: a custodian's attestation verifies against it (§5).
    pub signer: [u8; 32],
    /// The ALab vault ([`crate::client::alab_vault`]).
    pub alab: bool,
    /// Entry names in lockers this Mac opens.
    pub held: BTreeSet<String>,
    /// The actual accepted source view, captured beside these names before any table lock.
    pub version: Option<crate::client::version::AuthenticatedVersion>,
}

/// Where Commons stands for this Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commons {
    /// No control entry this Mac can read, and why.
    Unavailable(Unavailable),
    /// The control entry is visible; this Mac is not a current Writer.
    Visible,
    /// This Mac is a current Commons Writer: a request can be posted.
    Writer,
}

/// The policy in force (§7.2): the compiled default, generation 0, until a custodian-signed
/// `Policy` record (step 5) says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The generation, cited by a `not-authorized` retirement.
    pub generation: u64,
    /// Whether it gives posted wants to any admitted member (ALab's default).
    pub members_may_have: bool,
}

impl Policy {
    /// The compiled default: the ALab vault gives every posted want to any admitted member;
    /// any other vault gives nothing by request until its custodian publishes a policy.
    #[must_use]
    pub const fn compiled(alab: bool) -> Self {
        Self {
            generation: 0,
            members_may_have: alab,
        }
    }

    /// Whether this Mac may have `want`. Decided before anything reads whether the vault holds
    /// it, so the answer reads the same either way; the catalog's fixed classes are never
    /// given.
    #[must_use]
    pub const fn allows(&self, want: &Want) -> bool {
        want.posted() && self.members_may_have
    }
}

/// What this Mac observes when a need moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The applied vault version, or `None` when this Mac reads no signed vault.
    pub sight: Option<Sight>,
    /// The entries, of those the sight names, of the wants with a producer ([`Want::producer`])
    /// this reading judged ([`judged_for`]: the ledger's open ones, and one being raised) whose
    /// value this Mac uses now: each Claude Code token whose standing is verified and in use here
    /// ([`crate::claude_token::Standing::verified_in_use`], which a refusal here ends). A token
    /// whose name alone the sight shows (unreadable here, ended, mismatched, refused here, or
    /// unverified, whose serving on its minting Mac is the harness's verdict there) is not one; a
    /// reading that judged no such want ([`facts`]) holds none.
    pub in_use: BTreeSet<String>,
    /// Commons for this Mac.
    pub commons: Commons,
    /// The policy in force.
    pub policy: Policy,
    /// A member offered it (step 2's `Offer` records; the model's `held`).
    pub offered: bool,
    /// Unix seconds now.
    pub now: u64,
}

impl Facts {
    /// Strict source progress is retry eligibility, never consumer/provider acceptance.
    pub fn after_refusal(&self, refused: Option<&RefusalClaim>) -> bool {
        match refused {
            None => true,
            Some(RefusalClaim::LegacySequence(floor)) => self.sight.as_ref().is_some_and(|s| {
                s.version.as_ref().map_or(s.seq > *floor, |version| {
                    version.envelope_sequence().is_some_and(|seq| seq > *floor)
                })
            }),
            Some(RefusalClaim::Authenticated(floor)) => self
                .sight
                .as_ref()
                .and_then(|s| s.version.as_ref())
                .is_some_and(|v| {
                    v.relation(floor) == crate::client::version::VersionRelation::Advanced
                }),
        }
    }

    fn publication_after(&self, version: &RefusalClaim, refused: Option<&RefusalClaim>) -> bool {
        match (version, refused) {
            (_, None) => true,
            (RefusalClaim::LegacySequence(next), Some(RefusalClaim::LegacySequence(floor))) => {
                next > floor
            }
            (RefusalClaim::Authenticated(next), Some(RefusalClaim::LegacySequence(floor))) => {
                next.envelope().is_some_and(|e| e.seq > *floor)
            }
            (RefusalClaim::Authenticated(next), Some(RefusalClaim::Authenticated(floor))) => self
                .sight
                .as_ref()
                .and_then(|s| s.version.as_ref())
                .is_some_and(|v| {
                    v.relation_between(next, floor)
                        == crate::client::version::VersionRelation::Advanced
                }),
            _ => false,
        }
    }

    /// Whether the vault gives this Mac `want`: its entry is in the sight, and, for a want with
    /// a producer, its value is in use here ([`Self::in_use`]), not its name alone.
    fn holds(&self, want: &Want) -> bool {
        let Some(sight) = &self.sight else {
            return false;
        };
        want.entry().is_some_and(|entry| {
            sight.held.contains(&entry)
                && (want.producer().is_none() || self.in_use.contains(&entry))
        })
    }

    fn writer(&self) -> bool {
        self.commons == Commons::Writer
    }
}

/// One move of a need (an action of the model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// The catalog answers a want no Mac can give.
    Catalog,
    /// The policy does not give it to this Mac.
    Unauthorized,
    /// Its consumer's check passes after the delivery.
    Apply,
    /// Its consumer's check passes without a delivery.
    Unneeded,
    /// It is withdrawn by hand on this Mac (`aterm secrets need retire`), with a note.
    Withdraw(String),
    /// Its consumer needs it again after `not-needed`.
    NeedAgain,
    /// Raise it again, only on a changed condition.
    Reraise,
    /// Name the waiter again after what this Mac observes moved (Commons became ready).
    Refresh,
    /// The request reached `credentials/requests/1` (aterm_db::credentials): this Mac is a
    /// Commons Writer and the policy gives it the want. The caller takes this move only once
    /// the table holds the record.
    Flush,
}

/// The want's entry, for a line.
fn entry_words(want: &Want) -> String {
    want.entry().unwrap_or_else(|| want.key())
}

/// The waiter of a need still in the ledger.
fn local_wait(want: &Want, facts: &Facts) -> Wait {
    match want.class() {
        Class::PerMac => Wait::Requester,
        Class::AdminAct => Wait::Founding,
        Class::Copyable | Class::VaultOnly if facts.writer() => Wait::Requester,
        Class::Copyable | Class::VaultOnly => Wait::Founding,
    }
}

/// The line a need still in the ledger shows: the one party it waits on.
fn local_line(want: &Want, facts: &Facts, refused: Option<&RefusalClaim>) -> String {
    let vault = facts.sight.as_ref().map(|sight| sight.vault.as_str());
    let founding = match facts.commons {
        Commons::Writer => {
            return format!(
                "this Mac posts the request for {} to Commons at its next pass",
                want.title()
            );
        }
        Commons::Visible => "this Mac becomes a Commons Writer: it enrolls itself while an aterm window runs, and m22's held approver does not add open admissions to Commons (an admin's aterm secrets approve <host> does)".to_owned(),
        Commons::Unavailable(Unavailable::ControlNotVisible) => format!(
            "no other Mac can be asked until Commons runs on the {} vault (control-not-visible); the vault's admin aterm founds it at its first window pass on a release that has founding",
            vault.unwrap_or("this")
        ),
        Commons::Unavailable(Unavailable::Unconfigured) => {
            "this Mac is in no vault, so no other Mac can be asked (aterm secrets join)".to_owned()
        }
        Commons::Unavailable(reason) => format!(
            "no other Mac can be asked until this Mac reads Commons control ({}: {})",
            reason.code(),
            reason.explanation()
        ),
    };
    match want.class() {
        Class::Copyable => format!(
            "{founding}; the vault lacking it proves nothing, since its copies live on release Macs by design"
        ),
        Class::VaultOnly if facts.holds(want) => format!(
            "the vault gives this Mac {} at seq {}, and its consumer still finds none: it raises again with refused = {}",
            entry_words(want),
            facts.sight.as_ref().map_or(0, |sight| sight.seq),
            refused.map_or_else(
                || format!("seq {}", facts.sight.as_ref().map_or(0, |s| s.seq)),
                ToString::to_string
            )
        ),
        Class::VaultOnly | Class::AdminAct => founding,
        Class::PerMac => String::new(),
    }
}

/// Whether the reason a need ended has changed, so it may be raised again (§6.2): never time,
/// except a lapse.
fn changed(retirement: &Retirement, facts: &Facts) -> bool {
    retirement.reason.may_change()
        && match retirement.reason {
            Reason::NotAuthorized => retirement.generation != Some(facts.policy.generation),
            Reason::NotHeld => facts.offered,
            _ => true,
        }
}

/// `text` as a `Retired` record's field holds it (§5): at most `cap` characters, each control
/// character read as a space, since the table refuses a record that holds one.
#[must_use]
pub fn record_text(text: &str, cap: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(cap)
        .collect()
}

/// A note as a `Retired` record carries it (§5): at most [`MAX_NOTE`] characters, each
/// control character read as a space ([`record_text`]).
#[must_use]
pub fn note_of(text: &str) -> String {
    record_text(text, MAX_NOTE)
}

fn retire_with(need: &Need, retirement: Retirement, line: String, now: u64) -> Need {
    Need {
        state: State::Retired,
        wait: Wait::Nobody,
        retired: Some(retirement),
        line,
        updated: now,
        ..need.clone()
    }
}

/// A new need for `want` raised by `purpose` (the model's `Raise*`), still in the ledger.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Raise",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "RaisePerMachine",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "RaiseVaultOnly",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "RaiseProducer",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[must_use]
pub fn fresh(id: [u8; 16], want: Want, purpose: Purpose, facts: &Facts) -> Need {
    let wait = local_wait(&want, facts);
    Need {
        id,
        line: local_line(&want, facts, None),
        seq: facts.sight.as_ref().map(|sight| sight.seq),
        want,
        purpose,
        state: State::Local,
        wait,
        retired: None,
        raised: facts.now,
        updated: facts.now,
        raises: 1,
        refused: None,
        needed: true,
        got: false,
        got_authorized: false,
        raised_again_on_change: None,
        posted: false,
        custodian_silent: false,
        record: None,
        exposed: Vec::new(),
    }
}

/// `act` applied to `need` under `facts`, or `None` when its guard does not hold.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Catalog",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Unauthorized",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Apply",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Unneeded",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "NeedAgain",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Reraise",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Flush",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "CommonsReady",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
#[must_use]
pub fn act(need: &Need, facts: &Facts, act: &Act) -> Option<Need> {
    let now = facts.now;
    let posted_class = matches!(need.want.class(), Class::Copyable | Class::VaultOnly);
    match act {
        Act::Catalog => {
            let (reason, line) = need.want.catalog_retirement()?;
            (need.state == State::Local).then(|| {
                retire_with(
                    need,
                    Retirement {
                        reason,
                        evidence: format!("catalog {}", catalog::VERSION),
                        note: String::new(),
                        generation: None,
                    },
                    line,
                    now,
                )
            })
        }
        Act::Unauthorized => {
            let waiting = matches!(need.state, State::Local | State::Open);
            (waiting && posted_class && need.want.posted() && !facts.policy.allows(&need.want))
                .then(|| {
                    let vault = facts
                        .sight
                        .as_ref()
                        .map_or("this", |sight| sight.vault.as_str());
                    retire_with(
                        need,
                        Retirement {
                            reason: Reason::NotAuthorized,
                            evidence: format!("policy {}", facts.policy.generation),
                            note: String::new(),
                            generation: Some(facts.policy.generation),
                        },
                        format!(
                            "the {vault} vault's policy (generation {}) gives {} to no Mac by request; it is raised again once a later policy generation exists",
                            facts.policy.generation,
                            need.want.title()
                        ),
                        now,
                    )
                })
        }
        Act::Apply => (need.state == State::Published).then(|| Need {
            state: State::Fulfilled,
            wait: Wait::Nobody,
            line: format!(
                "fulfilled: {} arrived and its consumer's check passes",
                need.want.title()
            ),
            updated: now,
            ..need.clone()
        }),
        Act::Unneeded | Act::Withdraw(_) => {
            let waiting = matches!(
                need.state,
                State::Local | State::Open | State::Offered | State::Published
            );
            (waiting && need.needed).then(|| {
                // A withdrawal rests on a word given by hand, not a check: the want may still be
                // absent, so its evidence and line say where it was withdrawn.
                let (evidence, note, line) = if let Act::Withdraw(note) = act {
                    let host = facts.sight.as_ref().map(|sight| sight.host.as_str());
                    (
                        format!(
                            "{WITHDRAWN_BY_HAND}{}",
                            host.unwrap_or("the requesting Mac")
                        ),
                        note_of(note),
                        format!(
                            "not needed: withdrawn by hand on this Mac{}; raising {} again asks again",
                            host.map_or_else(String::new, |host| format!(" ({host})")),
                            need.want.title()
                        ),
                    )
                } else {
                    (
                        "the requester's own check".to_owned(),
                        String::new(),
                        format!(
                            "not needed: {}'s check no longer lacks {}",
                            need.purpose.id(),
                            need.want.title()
                        ),
                    )
                };
                Need {
                    needed: false,
                    ..retire_with(
                        need,
                        Retirement {
                            reason: Reason::NotNeeded,
                            evidence,
                            note,
                            generation: None,
                        },
                        line,
                        now,
                    )
                }
            })
        }
        Act::NeedAgain => (need.state == State::Retired
            && need.retired.as_ref().map(|r| r.reason) == Some(Reason::NotNeeded)
            && !need.needed)
            .then(|| Need {
                needed: true,
                updated: now,
                ..need.clone()
            }),
        Act::Reraise => {
            let retirement = need.retired.as_ref()?;
            let guard = need.state == State::Retired
                && need.needed
                && need.raises >= 1
                && changed(retirement, facts);
            guard.then(|| {
                let mut id = [0; 16];
                fill_id(&mut id);
                Need {
                    id,
                    state: State::Local,
                    wait: local_wait(&need.want, facts),
                    retired: None,
                    raises: need.raises.saturating_add(1),
                    raised_again_on_change: Some(changed(retirement, facts)),
                    got: false,
                    got_authorized: false,
                    posted: false,
                    custodian_silent: false,
                    record: None,
                    line: local_line(&need.want, facts, need.refused.as_ref()),
                    seq: facts.sight.as_ref().map(|sight| sight.seq),
                    updated: now,
                    ..need.clone()
                }
            })
        }
        Act::Flush => {
            let guard = need.state == State::Local
                && posted_class
                && need.purpose.asks_the_lab()
                && facts.writer()
                && facts.policy.allows(&need.want);
            // A want with a producer has no holder to offer it and no copy to take: what moves
            // it is this Mac's own reading of the vault's log ([`settle`]), so it waits on the
            // requester, and its line names the producer.
            guard.then(|| match need.want.producer() {
                None => Need {
                    state: State::Open,
                    wait: Wait::Holders,
                    posted: true,
                    line: format!(
                        "posted to the lab: waits on the Macs allowed to give {} to sweep for it, and on the vault admin's aterm to copy it from the vault",
                        need.want.title()
                    ),
                    updated: now,
                    ..need.clone()
                },
                Some(producer) => Need {
                    state: State::Open,
                    wait: Wait::Requester,
                    posted: true,
                    line: format!(
                        "posted to the lab: {} has one producer, {producer}, run on a Mac signed in to that account; this Mac ends the request once the vault's log gives it {} and the token is in use here",
                        need.want.title(),
                        entry_words(&need.want)
                    ),
                    updated: now,
                    ..need.clone()
                },
            })
        }
        Act::Refresh => (need.state == State::Local && posted_class).then(|| {
            let wait = local_wait(&need.want, facts);
            Need {
                wait,
                line: local_line(&need.want, facts, need.refused.as_ref()),
                seq: facts.sight.as_ref().map(|sight| sight.seq),
                ..need.clone()
            }
        }),
    }
}

/// What the lab decided about a request this Mac posted, read off a record whose writer and
/// evidence checked (`aterm_db::credentials` holds the checks, §6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A Mac that holds it offered it: the custodian takes the offer (§8.2).
    Offered,
    /// The custodian copied it into this Mac's own host section at vault `seq` (§8.1).
    Published {
        /// The vault version that carries it.
        version: RefusalClaim,
    },
    /// Retired with a reason whose writer and evidence checked.
    Retired(Retirement),
}

/// What reached `credentials/requests/1`, or was read off it, for one of this Mac's needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Absorb {
    /// The request record is in the table, under this operation digest ([`Act::Flush`]).
    Flushed(Vec<u8>),
    /// A terminal record retired an obsolete wire request, and its bound successor was
    /// durably posted. The local ledger adopts the successor's nonce and receipt together.
    Reissued { nonce: [u8; 16], record: Vec<u8> },
    /// The lab decided it.
    Decided(Decision),
    /// [`SILENCE_SECS`] passed with no custodian record on it and no offer ([`silent`]).
    CustodianSilent,
}

/// The line a lab retirement leaves.
fn retired_line(want: &Want, retirement: &Retirement) -> String {
    let title = want.title();
    let note = if retirement.note.is_empty() {
        String::new()
    } else {
        format!(" ({})", retirement.note)
    };
    match retirement.reason {
        Reason::NotHeld => format!(
            "not held: no Mac allowed to give {title} holds it ({}); it is raised again when a Mac the sweeps did not cover holds it{note}",
            retirement.evidence
        ),
        Reason::Lapsed => format!(
            "lapsed: the lab heard nothing from this Mac on it for 30 days; this Mac raises it again as it takes this in, while it is still needed{note}"
        ),
        Reason::RequesterGone => format!(
            "requester gone: the vault no longer names this Mac's recipient ({}); this Mac's new incarnation raises its own{note}",
            retirement.evidence
        ),
        Reason::NotAuthorized => format!(
            "not authorized: the vault's policy ({}) gives {title} to no Mac by request; it is raised again once a later policy generation exists{note}",
            retirement.evidence
        ),
        reason => format!("retired {}: {}{note}", reason.word(), retirement.evidence),
    }
}

/// `need` moved by what the lab decided, or `None` when the decision does not apply to where
/// it stands (an ended need, a publication of one never posted).
#[must_use]
pub fn decided(need: &Need, facts: &Facts, decision: &Decision) -> Option<Need> {
    let now = facts.now;
    if !need.posted || !matches!(need.state, State::Open | State::Offered) {
        return None;
    }
    match decision {
        Decision::Offered => {
            (need.state == State::Open && facts.policy.allows(&need.want)).then(|| {
                Need {
                    state: State::Offered,
                    wait: Wait::Custodian,
                    line: format!(
                        "offered: a Mac that holds {} offered it; the vault custodian's running aterm takes its copy over the authenticated LAN connection",
                        need.want.title()
                    ),
                    updated: now,
                    ..need.clone()
                }
            })
        }
        Decision::Published { version } => (facts.policy.allows(&need.want)
            && facts.publication_after(version, need.refused.as_ref()))
        .then(|| Need {
            state: State::Published,
            wait: Wait::Requester,
            got: true,
            got_authorized: true,
            line: format!(
                "published: the vault admin's aterm put {} in this Mac's own section at {version}; this Mac's next sync applies it and its consumer's check fulfils it",
                entry_words(&need.want)
            ),
            seq: version.sequence(),
            updated: now,
            ..need.clone()
        }),
        Decision::Retired(retirement) => Some(retire_with(
            need,
            retirement.clone(),
            retired_line(&need.want, retirement),
            now,
        )),
    }
}

/// `need` once [`SILENCE_SECS`] passed with no custodian record on its posted request and no
/// offer it could take (§6.1): the vault admin's aterm wrote nothing on it (no window there runs
/// a build with the custodian pass, or its pass waits on what it cannot do), so it waits on the
/// custodian and its line says so. A want that reached this Mac's own section is delivered, as a
/// custodian's copy is; a refused value is new only at a later vault version. Each pass reads it
/// again, so a sight that was missing then moves it on; `None` for a need not open or offered on
/// the table, or one this observation leaves as it is.
#[must_use]
pub fn silent(need: &Need, facts: &Facts) -> Option<Need> {
    // A want with a producer waits on no custodian: a value in use here, arriving in the
    // vault's log, ends it ([`settle`]).
    if !need.posted
        || !matches!(need.state, State::Open | State::Offered)
        || need.want.producer().is_some()
    {
        return None;
    }
    if let Some(sight) = facts.sight.as_ref()
        && facts.holds(&need.want)
        && facts.policy.allows(&need.want)
        && facts.after_refusal(need.refused.as_ref())
    {
        return Some(Need {
            state: State::Published,
            wait: Wait::Requester,
            got: true,
            got_authorized: true,
            custodian_silent: true,
            line: format!(
                "the vault gives this Mac {} at seq {}, with no custodian record on the request; its consumer's next check fulfils it",
                entry_words(&need.want),
                sight.seq
            ),
            seq: Some(sight.seq),
            updated: facts.now,
            ..need.clone()
        });
    }
    if need.state != State::Open {
        return None;
    }
    let silent = Need {
        custodian_silent: true,
        updated: facts.now,
        ..need.clone()
    };
    let unmoved = |next: Need| {
        (Need {
            updated: need.updated,
            ..next.clone()
        } != *need)
            .then_some(next)
    };
    unmoved(Need {
        wait: Wait::Custodian,
        line: format!(
            "posted to the lab: the vault admin's aterm has written no Commons record on it in the {} hours since it was posted, and no Mac offered it; an aterm window on the vault's admin Mac, on a build with the custodian pass, decides it",
            SILENCE_SECS / 3600
        ),
        ..silent
    })
}

/// A fresh request id: 16 random bytes. A source that fails leaves zeros, which a ledger row
/// with a fresh id never keeps ([`ledger`] refuses to save it).
fn fill_id(id: &mut [u8; 16]) {
    if aterm_uds::rand::fill(id).is_err() {
        *id = [0; 16];
    }
}

/// Every automatic move `need` takes under `facts`, in the order the design fixes: the catalog
/// first, then the policy before anything reads whether the vault holds it, then, for a need no
/// consumer checks for itself, the delivery fulfils it, then the waiter named again. The admin act ends when the control entry
/// applies; `entry:<name>` is answered from this Mac's own envelope; a want with a producer
/// ([`Want::producer`]) is answered whatever its purpose, since no consumer in this build checks
/// one for itself, once the vault's log gives this Mac a value in use here ([`arrival`]): a token
/// whose name alone it shows answers nothing.
#[must_use]
pub fn settle(need: &Need, facts: &Facts) -> Need {
    if need.state.ended() {
        return need.clone();
    }
    if !need.purpose.asks_the_lab() {
        return rotation_wait(need);
    }
    if let Some(next) = act(need, facts, &Act::Catalog) {
        return next;
    }
    if need.want.class() == Class::AdminAct {
        let visible = matches!(facts.commons, Commons::Visible | Commons::Writer);
        return if visible {
            Need {
                state: State::Fulfilled,
                wait: Wait::Nobody,
                line: "fulfilled: this Mac reads the Commons control entry".to_owned(),
                updated: facts.now,
                ..need.clone()
            }
        } else {
            Need {
                wait: Wait::Founding,
                line: "waits on the vault admin's aterm: it founds Commons at its first window pass on a release that has founding (aterm secrets commons init on that Mac does the same by hand)".to_owned(),
                ..need.clone()
            }
        };
    }
    if let Want::Entry(name) = &need.want {
        return answer_entry(need, name, facts);
    }
    if need.want == Want::FleetCode {
        return fleet_code(need, facts);
    }
    if let Some(next) = act(need, facts, &Act::Unauthorized) {
        return next;
    }
    if let Some(next) = arrival(need, facts) {
        return next;
    }
    if !need.purpose.checks_itself()
        && facts.holds(&need.want)
        && let Some(next) = act(need, facts, &Act::Apply)
    {
        let seq = facts.sight.as_ref().map_or(0, |sight| sight.seq);
        return Need {
            line: format!(
                "fulfilled: {} is in this Mac's envelope at vault seq {seq}; no consumer in this build checks it further",
                need.want.title()
            ),
            ..next
        };
    }
    act(need, facts, &Act::Refresh).unwrap_or_else(|| need.clone())
}

/// The arrival of a want with a producer (the model's `Arrived`): once the vault's log gives this
/// Mac its value in use here ([`Facts::in_use`]), `need` ends as the model's `Unneeded` does, said
/// as an arrival resting on the vault seq this Mac reads it at. No Mac copied it: this Mac reads
/// it in place in the vault's log. `None` for any other want, or one not in use here.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCredentialRequest",
        action = "Arrived",
        project = "aterm_secrets::need::tests::request_transition"
    )
)]
fn arrival(need: &Need, facts: &Facts) -> Option<Need> {
    if need.want.producer().is_none() || !facts.holds(&need.want) {
        return None;
    }
    let next = act(need, facts, &Act::Unneeded)?;
    let seq = facts.sight.as_ref().map_or(0, |sight| sight.seq);
    Some(Need {
        retired: next.retired.map(|retired| Retirement {
            evidence: format!("vault seq {seq}"),
            ..retired
        }),
        line: format!(
            "arrived: the vault's log gives this Mac {} at seq {seq}, in use here; it is read there in place and never copied or written to a file",
            entry_words(&need.want)
        ),
        seq: Some(seq),
        ..next
    })
}

/// A stolen Mac's rotation stays on this Mac until a new value is published in each section it
/// read the entry in ([`rotated`]) or it is withdrawn by hand: no Mac's copy answers it, and the
/// vault holding the entry proves nothing.
fn rotation_wait(need: &Need) -> Need {
    let sections = if need.exposed.is_empty() {
        String::new()
    } else {
        format!(" in {}", need.exposed.join(", "))
    };
    Need {
        wait: Wait::Requester,
        line: format!(
            "filed for rotation: a stolen Mac could read {}{sections}; an aterm secrets set of it there on this Mac closes this",
            entry_words(&need.want)
        ),
        ..need.clone()
    }
}

/// `fleet-code` is the ALab vault's `[everyone]` control entry, which no holder contributes: it is
/// fulfilled once this Mac's envelope holds it, and until then waits on this Mac joining the ALab
/// vault (default setup does that) or, in it, on an admin's `aterm fleet publish`.
fn fleet_code(need: &Need, facts: &Facts) -> Need {
    let (state, wait, line) = match &facts.sight {
        Some(sight) if facts.holds(&need.want) => (
            State::Fulfilled,
            Wait::Nobody,
            format!(
                "fulfilled: the vault gives this Mac the default fleet's code at seq {}",
                sight.seq
            ),
        ),
        Some(sight) if sight.alab => (
            State::Local,
            Wait::Custodian,
            "waits on an ALab vault admin's `aterm fleet publish`, or this Mac's admission to \
             the vault"
                .to_owned(),
        ),
        _ => (
            State::Local,
            Wait::Requester,
            "waits on this Mac joining the ALab vault, which alone carries the default fleet"
                .to_owned(),
        ),
    };
    if need.state == state && need.wait == wait && need.line == line {
        return need.clone();
    }
    Need {
        state,
        wait,
        line,
        updated: facts.now,
        ..need.clone()
    }
}

/// `entry:<name>` is never posted: an AI CLI's login file is retired `never-copied`, an entry in
/// a locker this Mac opens is already its own, and any other name answers the same words whether
/// the entry is addressed elsewhere or absent.
fn answer_entry(need: &Need, name: &str, facts: &Facts) -> Need {
    if let Some(why) = crate::text::cli_login_refusal(name) {
        return retire_with(
            need,
            Retirement {
                reason: Reason::NeverCopied,
                evidence: format!("catalog {}", catalog::VERSION),
                note: String::new(),
                generation: None,
            },
            format!("retired never-copied: {why}"),
            facts.now,
        );
    }
    let seq = facts.sight.as_ref().map_or(0, |sight| sight.seq);
    if facts.holds(&need.want) {
        return retire_with(
            need,
            Retirement {
                reason: Reason::NotNeeded,
                evidence: format!("vault seq {seq}"),
                note: String::new(),
                generation: None,
            },
            format!("already yours at seq {seq}: run aterm secrets sync"),
            facts.now,
        );
    }
    retire_with(
        need,
        Retirement {
            reason: Reason::NotAuthorized,
            evidence: format!("policy {}", facts.policy.generation),
            note: String::new(),
            generation: Some(facts.policy.generation),
        },
        format!(
            "{name} is not addressed to this Mac: an entry reaches the Macs its section names, and only the vault's admin addresses one (aterm secrets set {name} --for <host> on that Mac)"
        ),
        facts.now,
    )
}

/// The `needs:` line `aterm secrets status` prints, or `None` with no need that has not ended.
#[must_use]
pub fn status_line(needs: &[Need]) -> Option<String> {
    let waiting: Vec<String> = needs
        .iter()
        .filter(|need| !need.state.ended())
        .map(|need| format!("{}: waits on {}", need.want.key(), need.wait.word()))
        .collect();
    (!waiting.is_empty())
        .then(|| format!("needs: {} waiting ({})", waiting.len(), waiting.join(", ")))
}

/// What this Mac observes now: its applied vault version's entry names, Commons, and the policy.
/// It judges no want with a producer ([`judged_for`] does).
///
/// # Errors
/// This Mac's vault state cannot be read.
#[cfg(any(unix, windows))]
pub fn facts(paths: &crate::client::Paths, now: u64) -> crate::Result<Facts> {
    facts_with_commons(paths, now, || observed_commons(paths))
}

/// Commons observation may establish local custody. Keep it outside any retained State guard.
#[cfg(any(unix, windows))]
pub(crate) fn observed_commons(paths: &crate::client::Paths) -> Commons {
    match crate::commons::observe(paths) {
        Ok(crate::commons::Observation::Visible(_)) => {
            if crate::commons::bootstrap::writer_here(paths) {
                Commons::Writer
            } else {
                Commons::Visible
            }
        }
        Ok(crate::commons::Observation::Unavailable(reason)) => Commons::Unavailable(reason),
        Err(_) => Commons::Unavailable(Unavailable::Busy),
    }
}

/// Reuse the ordinary sight/policy producer with a supplied availability observation.
/// The supplied Commons value is scheduling, never authority to publish or release a value.
#[cfg(any(unix, windows))]
pub(crate) fn facts_with_commons(
    paths: &crate::client::Paths,
    now: u64,
    commons: impl FnOnce() -> Commons,
) -> crate::Result<Facts> {
    let sight = crate::client::sight(paths)?;
    let commons = commons();
    // The configured vault decides the policy, not the applied version: a Mac mid-sync or
    // waiting on its membership has the same policy as once it applies. A Mac in no vault has
    // no policy against it yet, and its need waits on joining one.
    let configured = crate::client::config(paths)?
        .as_ref()
        .map(crate::client::alab_vault);
    let policy = if alab_for_tests(paths) {
        Policy::compiled(true)
    } else {
        policy_for(configured)
    };
    Ok(Facts {
        sight,
        in_use: BTreeSet::new(),
        commons,
        policy,
        offered: false,
        now,
    })
}

/// `facts` with [`Facts::in_use`] judged for `wants`: each Claude Code token one of them names
/// whose entry the sight names has its standing read here, by the typed token reading, which
/// returns no value, and counts where it is verified and in use at `facts.now`. Only those tokens are read, so
/// a ledger that asks for none pays nothing. A reading that finds this Mac's state busy, or fails,
/// gives none: a need of one waits, which is never a false answer, and the next reading tries
/// again.
pub(crate) fn judged_for<'a>(
    paths: &crate::client::Paths,
    mut facts: Facts,
    wants: impl IntoIterator<Item = &'a Want>,
) -> Facts {
    let Some(sight) = facts.sight.as_ref() else {
        return facts;
    };
    let keys: BTreeSet<&crate::claude_token::Key> = wants
        .into_iter()
        .filter_map(|want| match want {
            Want::ClaudeToken(key) if sight.held.contains(&key.entry()) => Some(key),
            _ => None,
        })
        .collect();
    let in_use: BTreeSet<String> = keys
        .into_iter()
        .filter(|key| {
            matches!(
                crate::claude_token::standing(paths, key),
                Ok(crate::claude_token::Lookup::Found(standing)) if standing.verified_in_use(facts.now)
            )
        })
        .map(crate::claude_token::Key::entry)
        .collect();
    facts.in_use = in_use;
    facts
}

/// The policy for a Mac whose configured vault is `configured` (`Some(true)` for the ALab
/// vault), or that is in none (`None`). The configured vault decides, never the version applied:
/// a Mac mid-sync, or waiting on its membership, has the policy it will have once it applies. A
/// Mac in no vault has no policy against it yet, so its need waits on joining one rather than
/// retiring `not-authorized`.
#[must_use]
pub const fn policy_for(configured: Option<bool>) -> Policy {
    match configured {
        None => Policy::compiled(true),
        Some(alab) => Policy::compiled(alab),
    }
}

/// The scratch vaults a test treats under the ALab vault's policy ([`test_alab_policy`]).
#[cfg(all(any(unix, windows), any(test, debug_assertions)))]
static ALAB_FOR_TESTS: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Treat the scratch vault under `paths` under the ALab vault's compiled policy, as a test of a
/// crate that raises needs does: a scratch vault is signed by a test key, never ALab's.
#[cfg(all(any(unix, windows), any(test, debug_assertions)))]
pub fn test_alab_policy(paths: &crate::client::Paths) {
    ALAB_FOR_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(paths.dir.clone());
}

#[cfg(any(unix, windows))]
fn alab_for_tests(paths: &crate::client::Paths) -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        ALAB_FOR_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&paths.dir)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        let _ = paths;
        false
    }
}

/// The model's actions this branch does not bind to a ledger move, each with why: the
/// environment's own steps, and the steps the lab's records and the custodian take
/// (docs/DESIGN-credential-requests-2026-10-27.md §15, steps 2 to 5).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Stored",
        reason = "The vault holding the entry is the admin's act; client::sight reads it off the applied envelope, which the give_then_satisfied test exercises."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Arrive",
        reason = "A member holding a copy is that member's store: aterm_db::credentials::pass_at's sweep asks it for the hold probe's one entry (Want::hold_entry) and writes the Offer, which this ledger takes in through need::decided."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Policy",
        reason = "A new policy generation is a custodian-signed Policy record (step 5); the compiled default is generation 0, and the fixture flips auth per generation from each state."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Quiet",
        reason = "A requester's silence is the absence of its Still record, which aterm_db::credentials::pass_at writes weekly while a request waits; the table projection reads it before a lapse counts (the table's lapse test)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Widen",
        reason = "A custodian's copy from vault text into the requester's own host section is aterm_db::credentials::custodian_pass_at on the admin Mac; this ledger takes its Published record in through need::decided, or, where no custodian record came for SILENCE_SECS, need::silent reads the delivery off this Mac's own section."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Offer",
        reason = "A holder's Offer record is aterm_db::credentials::pass_at's sweep; this ledger takes it in through need::decided (Decision::Offered)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "OfferLost",
        reason = "A failed runtime handoff leaves the request unpublished and advances to another Offer; credentials::handoff tests exercise the wire and the retained source."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Publish",
        reason = "A custodian's publication of a holder's delivery is aterm_db::credentials::handoff; this ledger consumes its attested Published record, while the copy from vault text is Widen."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Revoked",
        reason = "A policy that withdraws an offered want needs a later policy generation, a custodian-signed Policy record (step 5); the compiled policy has one generation."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Sweep",
        reason = "A member's Sweep record is aterm_db::credentials::pass_at in the sync.requests item; the table projection counts it before a not-held counts (the table's not-held test)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "NotHeld",
        reason = "A custodian's attested not-held retirement is aterm_db::credentials::custodian_pass_at; this ledger takes it in through need::decided."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Gone",
        reason = "A requester the vault no longer names is retired requester-gone by a custodian: the table projection accepts an attested one, and no custodian in this build writes it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Lapse",
        reason = "The custodian's lapse of a silent requester is aterm_db::credentials::custodian_pass_at; this ledger takes it in through need::decided."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyPublishUnauthorized",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyFlushCatalog",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyNotHeldUnswept",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyRetireSilently",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyStrandOffer",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyReraiseSame",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyFulfilledOnOffer",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyPublishToEveryone",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyPublishWithoutCustody",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyLapseLive",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyParkSweep",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyCopyProducer",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyFlushProducerToHolders",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "InUse",
        reason = "A want with a producer's value coming into use on the requester is a person running aterm harness accounts token on a Mac signed in to the account and the vault's log carrying it here, verified (claude_token::Standing::verified_in_use); need::judged_for reads it into Facts::in_use, as the need-of-a-token log-lab test exercises."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "OutOfUse",
        reason = "A want with a producer's value leaving use on the requester is Claude Code refusing it on this Mac (claude_token::note_refused, which Standing::state reads as State::Rejected) or its end passing (State::Ended); Standing::verified_in_use is false for both until a newer token replaces it, so need::judged_for puts nothing in Facts::in_use, as the need-of-a-token log-lab test exercises with m40's refusal."
    )
)]
#[doc(hidden)]
pub fn credential_request_environment() {}

/// File `want` for `purpose` ([`raise`]) and say so: the one line a step that lacks a usable
/// value reports, naming the want, `why` this Mac holds none and the request (or why none was
/// filed). It names no value.
#[must_use]
pub fn blocked(paths: &crate::client::Paths, want: Want, purpose: Purpose, why: &str) -> String {
    let key = want.key();
    match raise(paths, want, purpose) {
        Ok(need) => format!(
            "this Mac holds no usable {key} ({why}); `aterm secrets need {key}` asked the lab's \
             Macs (request {})",
            aterm_codec::hex::encode_lower(&need.id)
        ),
        Err(error) => format!(
            "this Mac holds no usable {key} ({why}); `aterm secrets need {key}` was not filed: \
             {error}"
        ),
    }
}

#[cfg(test)]
mod tests;
