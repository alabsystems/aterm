// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CREDENTIAL REQUESTS ON THIS MAC (docs/DESIGN-credential-requests-2026-10-27.md, step 1): a
//! consumer that lacks a credential raises a *need* ([`raise`]); the need ledger
//! (`~/.config/aterm/secrets/needs.toml`, 0600, a path no vault entry can name) keeps it with a
//! request id fixed before anything is posted; the [`catalog`] answers at once the wants no Mac
//! can give (the paper master, a vendor login); the absence proof over this Mac's applied
//! envelope ([`Sight`]) shows, before Commons runs, that no Mac but an admin holds a vault-only
//! want; and only then is a person asked, with one exact step ([`Ask`]). Every other need names
//! the one party it waits on ([`Wait`]). Nothing here reads a secret's value: the sight reads
//! entry names, the hold probe stats a path.
//!
//! The ledger's moves are pure ([`act`], [`settle`]) over what this Mac observes ([`Facts`]),
//! so the conformance test can drive them from every reachable state of
//! `aterm_models_db::credential_request_model`; [`raise`], [`satisfied`], [`retire`] and
//! [`list`] are those moves under the ledger's lock.

pub mod catalog;
pub mod custody;
mod ledger;

pub use catalog::{Class, GhProfile, Purpose, Want};
pub use ledger::{
    FILE as LEDGER, absorb, list, peek, raise, raise_refused, retire, satisfied, with_ledger,
};

use crate::commons::Unavailable;
use std::collections::BTreeSet;

/// The want window: how long the lab has to answer a request before the custodian decides
/// (§6.2).
pub const WANT_WINDOW_SECS: u64 = 72 * 3600;

/// A requester's silence that lets a custodian retire its request (`lapsed`, §6.2).
pub const LAPSE_SECS: u64 = 30 * 86_400;

/// How old a requester's newest record on a waiting request may grow before it writes `Still`.
pub const STILL_SECS: u64 = 7 * 86_400;

/// The most open requests one requester holds; past that, its records are inert.
pub const MAX_OPEN: usize = 16;

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
    /// Only a person can give it: the one state that shows a `NEEDS YOU`.
    AwaitingPerson,
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
            Self::AwaitingPerson => "awaiting-person",
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
            Self::AwaitingPerson,
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
    /// The want exists on no computer.
    NeverStored,
    /// Each Mac makes its own.
    NeverCopied,
    /// The policy in force does not give it to this Mac.
    NotAuthorized,
    /// Its consumer no longer needs it (or the requester withdrew it).
    NotNeeded,
    /// No Mac allowed to give it holds it, and no person can make one.
    NotHeld,
    /// A person declined to give it.
    Declined,
    /// The vault no longer names the requester.
    RequesterGone,
    /// The requester wrote nothing on it for 30 days.
    Lapsed,
    /// A newer request of the requester replaced it.
    Superseded,
}

impl Reason {
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
    /// The person.
    Person,
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
            Self::Person => "person",
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
            Self::Person,
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

/// The absence proof a person was asked on (§9.4), recorded where the ask was made (the
/// model's `askok`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proof {
    /// The vault seq the proof is stated at.
    pub seq: u64,
    /// The want's entry is in no locker this Mac opens.
    pub absent: bool,
    /// Every locker but `admin` is one this Mac opens.
    pub every_locker_open: bool,
    /// It was made before this Mac could post to Commons, so no Mac could be asked.
    pub before_commons: bool,
    /// Every member that may contribute swept the request and none holds it (the custodian's
    /// decision, step 3).
    pub swept_unheld: bool,
}

impl Proof {
    /// Whether asking a person on it was warranted: nothing holds it that a Mac could give.
    #[must_use]
    pub const fn warrants_asking(&self) -> bool {
        self.absent && ((self.before_commons && self.every_locker_open) || self.swept_unheld)
    }
}

/// What a person is asked (§10): a glass title, one step, a reason, detail lines, and the
/// `NEEDS YOU:` line an agent's report carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// At most 48 characters (`exceptions::WHAT_CAP`).
    pub what: String,
    /// One action, with no `; ` and no ` or `.
    pub step: String,
    /// Why only a person, with no pending words.
    pub reason: String,
    /// Where the value comes from and the alternatives.
    pub more: Vec<String>,
    /// The one `NEEDS YOU:` line, the same on every Mac the want blocks.
    pub needs_you: String,
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
    /// The vault seq whose value the consumer refused, if any.
    pub refused: Option<u64>,
    /// The consumer's word that it still needs the want (the model's `need`).
    pub needed: bool,
    /// Whether a `NEEDS YOU` is up for it (the model's `you`).
    pub asking: bool,
    /// The proof a person was asked on.
    pub proof: Option<Proof>,
    /// What the person is asked, while `asking`.
    pub ask: Option<Ask>,
    /// The value reached this Mac (the model's `got`).
    pub got: bool,
    /// The policy gave it to this Mac when it was published (the model's `pa`).
    pub got_authorized: bool,
    /// When raised again, whether the reason's condition had changed (the model's `okr`).
    pub raised_again_on_change: Option<bool>,
    /// Posted to Commons (the model's `posted`, step 2).
    pub posted: bool,
    /// The posted request's operation digest in `credentials/requests/1`. A second flush
    /// repeats the same command under the same retry identity, so the table returns this
    /// receipt rather than a second request.
    pub record: Option<Vec<u8>>,
    /// The one line that names what it waits on, or how it ended.
    pub line: String,
    /// The vault seq the line is stated at.
    pub seq: Option<u64>,
}

/// What this Mac's applied vault version shows of a want's entries (§9.4): names only.
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
    /// Some locker but `admin` is one this Mac cannot open.
    pub hidden: bool,
    /// Entry names in lockers this Mac opens.
    pub held: BTreeSet<String>,
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
    fn hidden(&self) -> bool {
        self.sight.as_ref().is_none_or(|sight| sight.hidden)
    }

    fn holds(&self, want: &Want) -> bool {
        let Some(sight) = &self.sight else {
            return false;
        };
        want.entry()
            .is_some_and(|entry| sight.held.contains(&entry))
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
    /// Before Commons, the absence proof hands a vault-only want to a person.
    Absent,
    /// The policy does not give it to this Mac.
    Unauthorized,
    /// The vault now gives it to this Mac.
    Give,
    /// Its consumer's check passes after the delivery.
    Apply,
    /// The requester withdraws it, or its consumer's check passes without a delivery.
    Unneeded,
    /// The person declines to give it.
    Decline(String),
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
    /// A request awaiting a person reached the table too, so a holder that comes back can
    /// still offer and take the ask down.
    Post,
}

/// The want's entry, for a line.
fn entry_words(want: &Want) -> String {
    want.entry().unwrap_or_else(|| want.key())
}

/// The waiter of a need still in the ledger.
fn local_wait(want: &Want, facts: &Facts) -> Wait {
    match want.class() {
        Class::Nowhere | Class::PerMac => Wait::Requester,
        Class::AdminAct => Wait::Founding,
        Class::Copyable if facts.writer() => Wait::Requester,
        Class::VaultOnly if facts.writer() || !facts.hidden() => Wait::Requester,
        Class::Copyable | Class::VaultOnly => Wait::Founding,
    }
}

/// The hand route that works before the handoff: a person at the vault's admin Mac publishes
/// the value into this Mac's own host section.
fn hand_route(want: &Want, host: &str) -> Option<String> {
    let entry = want.entry()?;
    let quoted = if entry.starts_with("~/") {
        format!("'{entry}'")
    } else {
        entry
    };
    Some(format!(
        "aterm secrets set {quoted} --for {host} --file <file> on the vault's admin Mac"
    ))
}

/// The line a need still in the ledger shows: the one party it waits on.
fn local_line(want: &Want, facts: &Facts, refused: Option<u64>) -> String {
    let host = facts.sight.as_ref().map(|sight| sight.host.as_str());
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
    let mut line = match want.class() {
        Class::Copyable => format!(
            "{founding}; the vault lacking it proves nothing, since its copies live on release Macs by design"
        ),
        Class::VaultOnly if facts.holds(want) => format!(
            "the vault gives this Mac {} at seq {}, and its consumer still finds none: it raises again with refused = {}",
            entry_words(want),
            facts.sight.as_ref().map_or(0, |sight| sight.seq),
            refused
                .or(facts.sight.as_ref().map(|sight| sight.seq))
                .unwrap_or(0)
        ),
        Class::VaultOnly if facts.hidden() => format!(
            "{founding}; this Mac cannot open every locker of its vault version, so it cannot show that no Mac holds {}",
            entry_words(want)
        ),
        Class::VaultOnly | Class::AdminAct => founding,
        Class::Nowhere | Class::PerMac => String::new(),
    };
    if want.class() == Class::Copyable
        && let Some(route) = hand_route(want, host.unwrap_or("<host>"))
    {
        line.push_str(&format!(
            "; until then, a person with the file runs {route}"
        ));
    }
    line
}

/// What a person is asked for a vault-only want the absence proof shows no Mac holds.
#[must_use]
pub fn ask_for(want: &Want, purpose: Purpose, host: &str) -> Option<Ask> {
    let entry = want.entry()?;
    let quoted = if entry.starts_with("~/") {
        format!("'{entry}'")
    } else {
        entry.clone()
    };
    let route = format!("aterm secrets set {quoted} --for {host} --file <file>");
    let (what, consumer, extra) = match want {
        Want::Mailbox(label) => (
            format!("{label}'s mailbox password is missing"),
            match purpose {
                Purpose::HarnessSignIn => {
                    format!(", for the Claude account {label}'s email-code sign-in,")
                }
                _ => String::new(),
            },
            Some(format!(
                "and {label}_totp the same way if the mailbox has 2FA"
            )),
        ),
        Want::Gh(_) | Want::SiteDeployerKey | Want::Notary => (
            format!("{} is missing", capitalized(&want.title())),
            String::new(),
            None,
        ),
        _ => return None,
    };
    let fits = |words: &String, cap: usize| words.chars().count() <= cap;
    let what = if fits(&what, aterm_messages::exceptions::WHAT_CAP) {
        what
    } else {
        "A needed credential is missing".to_owned()
    };
    let step = [
        format!("Run {route} on the vault's admin Mac"),
        format!("Set {entry} for {host} on the vault's admin Mac"),
        format!("Set {entry} on the vault's admin Mac"),
        "Set the missing entry on the vault's admin Mac".to_owned(),
    ]
    .into_iter()
    .find(|step| fits(step, aterm_messages::need::STEP_CAP))
    .unwrap_or_default();
    let mut needs_you = format!(
        "NEEDS YOU: {}{consumer} is on no Mac the lab can reach: on the vault's admin Mac run {route}",
        want.title()
    );
    if let Some(extra) = &extra {
        needs_you.push_str(&format!(", {extra}"));
    }
    let mut more = vec![format!("on the vault's admin Mac: {route}")];
    if let Some(extra) = extra {
        more.push(extra);
    }
    more.push(format!("it comes from: {}", want.source()));
    Some(Ask {
        what,
        step,
        reason: "no Mac the lab can reach holds it".to_owned(),
        more,
        needs_you,
    })
}

fn capitalized(words: &str) -> String {
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Whether the reason a need ended has changed, so it may be raised again (§6.2): never time,
/// except a lapse.
fn changed(retirement: &Retirement, facts: &Facts) -> bool {
    match retirement.reason {
        Reason::NotNeeded | Reason::Lapsed => true,
        Reason::NotAuthorized => retirement.generation != Some(facts.policy.generation),
        Reason::NotHeld => facts.offered,
        Reason::NeverStored
        | Reason::NeverCopied
        | Reason::Declined
        | Reason::RequesterGone
        | Reason::Superseded => false,
    }
}

fn retire_with(need: &Need, retirement: Retirement, line: String, now: u64) -> Need {
    Need {
        state: State::Retired,
        wait: Wait::Nobody,
        retired: Some(retirement),
        asking: false,
        ask: None,
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
        action = "RaiseNowhere",
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
        asking: false,
        proof: None,
        ask: None,
        got: false,
        got_authorized: false,
        raised_again_on_change: None,
        posted: false,
        record: None,
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
        action = "Absent",
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
        action = "Give",
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
        action = "Decline",
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
        action = "Post",
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
            let waiting = matches!(
                need.state,
                State::Local | State::Open | State::AwaitingPerson
            );
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
        Act::Absent => {
            let sight = facts.sight.as_ref()?;
            let guard = need.state == State::Local
                && need.want.class() == Class::VaultOnly
                && need.want.posted()
                && need.want.person_sourced()
                && !facts.writer()
                && !sight.hidden
                && !facts.holds(&need.want)
                && facts.policy.allows(&need.want);
            if !guard {
                return None;
            }
            let ask = ask_for(&need.want, need.purpose, &sight.host)?;
            Some(Need {
                state: State::AwaitingPerson,
                wait: Wait::Person,
                asking: true,
                proof: Some(Proof {
                    seq: sight.seq,
                    absent: !facts.holds(&need.want),
                    every_locker_open: !sight.hidden,
                    before_commons: !facts.writer(),
                    swept_unheld: false,
                }),
                line: ask.needs_you.clone(),
                ask: Some(ask),
                seq: Some(sight.seq),
                updated: now,
                ..need.clone()
            })
        }
        Act::Give => {
            let guard = need.state == State::AwaitingPerson
                && facts.holds(&need.want)
                && facts.policy.allows(&need.want);
            guard.then(|| {
                let seq = facts.sight.as_ref().map_or(0, |sight| sight.seq);
                Need {
                    state: State::Published,
                    wait: Wait::Requester,
                    asking: false,
                    ask: None,
                    got: true,
                    got_authorized: facts.policy.allows(&need.want),
                    line: format!(
                        "the vault gives this Mac {} at seq {seq}; its consumer's next check fulfils it",
                        entry_words(&need.want)
                    ),
                    seq: Some(seq),
                    updated: now,
                    ..need.clone()
                }
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
        Act::Unneeded => {
            let waiting = matches!(
                need.state,
                State::Local
                    | State::Open
                    | State::Offered
                    | State::Published
                    | State::AwaitingPerson
            );
            (waiting && need.needed).then(|| Need {
                needed: false,
                ..retire_with(
                    need,
                    Retirement {
                        reason: Reason::NotNeeded,
                        evidence: "the requester's own check".to_owned(),
                        note: String::new(),
                        generation: None,
                    },
                    format!(
                        "not needed: {}'s check no longer lacks {}",
                        need.purpose.id(),
                        need.want.title()
                    ),
                    now,
                )
            })
        }
        Act::Decline(note) => (need.state == State::AwaitingPerson).then(|| {
            let note: String = note.chars().take(160).collect();
            retire_with(
                need,
                Retirement {
                    reason: Reason::Declined,
                    evidence: "the requester".to_owned(),
                    note,
                    generation: None,
                },
                format!(
                    "declined by a person: {} is not given; raise it again by hand to ask again",
                    need.want.title()
                ),
                now,
            )
        }),
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
                    raises: need.raises + 1,
                    raised_again_on_change: Some(changed(retirement, facts)),
                    asking: false,
                    ask: None,
                    proof: None,
                    got: false,
                    got_authorized: false,
                    posted: false,
                    record: None,
                    line: local_line(&need.want, facts, need.refused),
                    seq: facts.sight.as_ref().map(|sight| sight.seq),
                    updated: now,
                    ..need.clone()
                }
            })
        }
        Act::Flush => {
            let guard = need.state == State::Local
                && posted_class
                && facts.writer()
                && facts.policy.allows(&need.want);
            guard.then(|| Need {
                state: State::Open,
                wait: Wait::Holders,
                posted: true,
                line: format!(
                    "posted to the lab: waits on the Macs allowed to give {} to sweep for it, and on the vault admin's aterm to copy it from the vault",
                    need.want.title()
                ),
                updated: now,
                ..need.clone()
            })
        }
        Act::Post => {
            (need.state == State::AwaitingPerson && !need.posted && facts.writer()).then(|| Need {
                posted: true,
                updated: now,
                ..need.clone()
            })
        }
        Act::Refresh => (need.state == State::Local && posted_class).then(|| {
            let wait = local_wait(&need.want, facts);
            Need {
                wait,
                line: local_line(&need.want, facts, need.refused),
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
        seq: u64,
    },
    /// Every Mac allowed to give it swept and none holds it, or the want window ended, and a
    /// person can make it.
    NeedsPerson,
    /// Retired with a reason whose writer and evidence checked.
    Retired(Retirement),
}

/// What reached `credentials/requests/1`, or was read off it, for one of this Mac's needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Absorb {
    /// The request record is in the table, under this operation digest ([`Act::Flush`]).
    Flushed(Vec<u8>),
    /// A request awaiting a person is in the table too ([`Act::Post`]).
    Posted(Vec<u8>),
    /// A terminal record retired an obsolete wire request, and its bound successor was
    /// durably posted. The local ledger adopts the successor's nonce and receipt together.
    Reissued { nonce: [u8; 16], record: Vec<u8> },
    /// The lab decided it.
    Decided(Decision),
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
            "lapsed: the lab heard nothing from this Mac on it for 30 days; its next pass raises it again while it is still needed{note}"
        ),
        Reason::RequesterGone => format!(
            "requester gone: the vault no longer names this Mac's recipient ({}); this Mac's new incarnation raises its own{note}",
            retirement.evidence
        ),
        Reason::Declined => format!(
            "declined on the vault's admin Mac: {title} is not given; raise it again by hand to ask again{note}"
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
    let live = matches!(
        need.state,
        State::Open | State::Offered | State::AwaitingPerson
    );
    if !need.posted || !live {
        return None;
    }
    match decision {
        Decision::Offered => {
            (need.state == State::Open && facts.policy.allows(&need.want)).then(|| {
                // The handoff that takes a holder's copy is step 4's: until it lands, the one
                // route that moves an offered want is the hand route, so the line names it.
                let host = facts
                    .sight
                    .as_ref()
                    .map_or("<host>", |sight| sight.host.as_str());
                let route = hand_route(&need.want, host).map_or_else(String::new, |route| {
                    format!("; meanwhile a person with that Mac's copy runs {route}")
                });
                Need {
                    state: State::Offered,
                    wait: Wait::Custodian,
                    line: format!(
                        "offered: a Mac that holds {} offered it, and the vault admin's aterm takes an offer only over the handoff from a holder, which this aterm version lacks{route}",
                        need.want.title()
                    ),
                    updated: now,
                    ..need.clone()
                }
            })
        }
        Decision::Published { seq } => facts.policy.allows(&need.want).then(|| Need {
            state: State::Published,
            wait: Wait::Requester,
            asking: false,
            ask: None,
            got: true,
            got_authorized: true,
            line: format!(
                "published: the vault admin's aterm put {} in this Mac's own section at seq {seq}; this Mac's next sync applies it and its consumer's check fulfils it",
                entry_words(&need.want)
            ),
            seq: Some(*seq),
            updated: now,
            ..need.clone()
        }),
        Decision::NeedsPerson => {
            if need.state == State::AwaitingPerson
                || !facts.policy.allows(&need.want)
                || facts.holds(&need.want)
            {
                return None;
            }
            let host = facts.sight.as_ref().map_or("<host>", |sight| sight.host.as_str());
            let ask = ask_for(&need.want, need.purpose, host)?;
            Some(Need {
                state: State::AwaitingPerson,
                wait: Wait::Person,
                asking: true,
                proof: Some(Proof {
                    seq: facts.sight.as_ref().map_or(0, |sight| sight.seq),
                    absent: !facts.holds(&need.want),
                    every_locker_open: !facts.hidden(),
                    before_commons: false,
                    swept_unheld: true,
                }),
                line: ask.needs_you.clone(),
                ask: Some(ask),
                updated: now,
                ..need.clone()
            })
        }
        Decision::Retired(retirement) => Some(retire_with(
            need,
            retirement.clone(),
            retired_line(&need.want, retirement),
            now,
        )),
    }
}

/// A fresh request id: 16 random bytes. A source that fails leaves zeros, which a ledger row
/// with a fresh id never keeps ([`ledger`] refuses to save it).
fn fill_id(id: &mut [u8; 16]) {
    if aterm_uds::rand::fill(id).is_err() {
        *id = [0; 16];
    }
}

/// Every automatic move `need` takes under `facts`, in the order the design fixes: the catalog
/// first, then the policy before anything reads whether the vault holds it, then what the
/// vault gives (and, for a need no consumer checks for itself, the delivery fulfils it), then
/// the absence proof, then the waiter named again. The admin act ends when the control entry
/// applies; `entry:<name>` is answered from this Mac's own envelope.
#[must_use]
pub fn settle(need: &Need, facts: &Facts) -> Need {
    if need.state.ended() {
        return need.clone();
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
    if let Some(next) = act(need, facts, &Act::Unauthorized) {
        return next;
    }
    let given = act(need, facts, &Act::Give);
    let current = given.as_ref().unwrap_or(need);
    if !current.purpose.checks_itself()
        && facts.holds(&current.want)
        && let Some(next) = act(current, facts, &Act::Apply)
    {
        let seq = facts.sight.as_ref().map_or(0, |sight| sight.seq);
        return Need {
            line: format!(
                "fulfilled: {} is in this Mac's envelope at vault seq {seq}; no consumer in this build checks it further",
                current.want.title()
            ),
            ..next
        };
    }
    if let Some(next) = given {
        return next;
    }
    if let Some(next) = act(need, facts, &Act::Absent) {
        return next;
    }
    act(need, facts, &Act::Refresh).unwrap_or_else(|| need.clone())
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

/// The exceptions entry a need awaiting a person files (§10): `secrets.need.<want>.<cause>`, a
/// fact for later with `Why::OnlyYou`. `None` for every other need.
#[must_use]
pub fn exception(need: &Need) -> Option<aterm_messages::exceptions::Entry> {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason as Words, Then, Why};
    let ask = need.ask.as_ref().filter(|_| need.asking)?;
    let mut entry = Entry::new(
        Family::Secrets,
        &need.want.key(),
        "awaiting-person",
        &ask.what,
        Why::OnlyYou(Words::new(&ask.reason)),
        Then::step(&ask.step),
    )
    .log_line(format!(
        "secrets need {} awaiting a person",
        need.want.key()
    ));
    for line in &ask.more {
        entry = entry.more(line);
    }
    Some(entry)
}

/// The exceptions entries of every need awaiting a person, one per want key (the same text on
/// every Mac the want blocks). A want's other rows (another purpose's, retired or waiting)
/// never hide the one that asks.
#[must_use]
pub fn exceptions(needs: &[Need]) -> Vec<aterm_messages::exceptions::Entry> {
    let mut seen = BTreeSet::new();
    needs
        .iter()
        .filter_map(|need| exception(need).map(|entry| (need, entry)))
        .filter(|(need, _)| seen.insert(need.want.key()))
        .map(|(_, entry)| entry)
        .collect()
}

/// The `NEEDS YOU:` lines of every need awaiting a person, one per want key.
#[must_use]
pub fn needs_you(needs: &[Need]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    needs
        .iter()
        .filter(|need| need.asking)
        .filter_map(|need| need.ask.as_ref().map(|ask| (need, ask)))
        .filter(|(need, _)| seen.insert(need.want.key()))
        .map(|(_, ask)| ask.needs_you.clone())
        .collect()
}

/// The `needs:` line `aterm secrets status` prints, or `None` with no need that has not ended.
#[must_use]
pub fn status_line(needs: &[Need]) -> Option<String> {
    let asking: Vec<String> = needs
        .iter()
        .filter(|need| need.asking)
        .map(|need| need.want.key())
        .collect();
    let waiting: Vec<String> = needs
        .iter()
        .filter(|need| !need.asking && !need.state.ended())
        .map(|need| format!("{}: waits on {}", need.want.key(), need.wait.word()))
        .collect();
    if asking.is_empty() && waiting.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !asking.is_empty() {
        parts.push(format!(
            "{} awaiting a person ({})",
            asking.len(),
            asking.join(", ")
        ));
    }
    if !waiting.is_empty() {
        parts.push(format!(
            "{} waiting ({})",
            waiting.len(),
            waiting.join(", ")
        ));
    }
    Some(format!("needs: {}", parts.join(", ")))
}

/// What this Mac observes now: its applied vault version's entry names, Commons, and the policy.
///
/// # Errors
/// This Mac's vault state cannot be read.
#[cfg(unix)]
pub fn facts(paths: &crate::client::Paths, now: u64) -> crate::Result<Facts> {
    let sight = crate::client::sight(paths)?;
    let commons = match crate::commons::observe(paths) {
        Ok(crate::commons::Observation::Visible(_)) => {
            if crate::commons::bootstrap::writer_here(paths) {
                Commons::Writer
            } else {
                Commons::Visible
            }
        }
        Ok(crate::commons::Observation::Unavailable(reason)) => Commons::Unavailable(reason),
        Err(_) => Commons::Unavailable(Unavailable::Busy),
    };
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
        commons,
        policy,
        offered: false,
        now,
    })
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
#[cfg(all(unix, any(test, debug_assertions)))]
static ALAB_FOR_TESTS: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Treat the scratch vault under `paths` under the ALab vault's compiled policy, as a test of a
/// crate that raises needs does: a scratch vault is signed by a test key, never ALab's.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn test_alab_policy(paths: &crate::client::Paths) {
    ALAB_FOR_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(paths.dir.clone());
}

#[cfg(unix)]
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
        action = "Source",
        reason = "A person can make the want or not: the catalog fixes it per want (Want::person_sourced), and the conformance fixture reads it from each reachable state."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Hidden",
        reason = "Whether some locker of the applied version is one this Mac cannot open is the vault's, read by client::sight; the conformance fixture sets it from each state, and absence_needs_every_locker_open reads it off real envelopes."
    )
)]
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
        reason = "A member holding a copy is that member's disk: aterm_db::credentials::pass_at's sweep stats the hold probe's one path (Want::hold_path) and writes the Offer, which this ledger takes in through need::decided."
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
        reason = "A custodian's copy from vault text into the requester's own host section is aterm_db::credentials::custodian_pass_at on the admin Mac; this ledger takes its Published record in through need::decided."
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
        reason = "A delivery lost between holder and custodian is step 4's handoff exchange, which this build does not have."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "Publish",
        reason = "A custodian's publication of a holder's accepted delivery is step 4's handoff, which this build does not have; the copy from vault text is Widen."
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
        action = "NeedsPerson",
        reason = "A custodian's attested NeedsPerson record is aterm_db::credentials::custodian_pass_at, which this ledger takes in through need::decided; before Commons the requester's own Absent move asks instead."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "LateOffer",
        reason = "A holder's offer on a request awaiting a person takes the ask down only once step 4's handoff can deliver it, which this build does not have."
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
        action = "BuggyDeclineKeepsNeedsYou",
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
        action = "BuggyAskPersonForHeld",
        reason = "A Buggy negative control of the model, dead at the committed config; the real code has no such step."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "BuggyAbsentDespiteHidden",
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
#[doc(hidden)]
pub fn credential_request_environment() {}

#[cfg(test)]
mod tests;
