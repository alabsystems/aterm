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
pub mod transfer;

pub use catalog::{Class, GhProfile, Purpose, Want};
pub use ledger::{
    FILE as LEDGER, absorb, list, peek, raise, raise_refused, retire, satisfied, with_ledger,
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

/// The most open requests one requester holds; past that, its records are inert.
pub const MAX_OPEN: usize = 16;

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
    /// The want window ended before every member answered, and none that answered holds it
    /// (the custodian's decision at the window's end, §6.2); the ask names who did not answer.
    pub window_ended: bool,
    /// It was made once [`SILENCE_SECS`] passed with no custodian record on the posted request
    /// and no offer, so this Mac stood in for the silent custodian ([`silent`]).
    pub custodian_silent: bool,
}

impl Proof {
    /// Whether asking a person on it was warranted: nothing holds it that a Mac could give.
    #[must_use]
    pub const fn warrants_asking(&self) -> bool {
        self.absent
            && (self.swept_unheld
                || self.window_ended
                || (self.every_locker_open && (self.before_commons || self.custodian_silent)))
    }
}

/// The most members a custodian's window decision names; past that it counts them.
pub const MAX_UNANSWERED: usize = 16;

/// The members a custodian's decision at the want window's end names as not having answered
/// the request (§6.2): no sweep and no offer on it. Empty when every member swept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unanswered {
    /// Their vault host names, at most [`MAX_UNANSWERED`], in name order.
    pub hosts: Vec<String>,
    /// How many more did not answer: past that cap, or a member no `[machines]` row names.
    pub others: u64,
}

impl Unanswered {
    /// Every member answered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.others == 0
    }

    /// Who did not answer, for a line: "m3", "m3 and m4", "m3 and 2 other Macs", "2 Macs".
    fn words(&self) -> String {
        let macs = |n: u64| if n == 1 { "Mac" } else { "Macs" };
        let mut names = self.hosts.clone();
        if self.others > 0 {
            let other = if names.is_empty() { "" } else { "other " };
            names.push(format!("{} {other}{}", self.others, macs(self.others)));
        }
        match names.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        }
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
    /// [`SILENCE_SECS`] passed with no custodian record on the posted request ([`silent`]).
    pub custodian_silent: bool,
    /// A Mac that holds it offered it while a person was asked, and the ask names that copy
    /// ([`Decision::Offered`]).
    pub late_offer: bool,
    /// The posted request's operation digest in `credentials/requests/1`. A second flush
    /// repeats the same command under the same retry identity, so the table returns this
    /// receipt rather than a second request.
    pub record: Option<Vec<u8>>,
    /// The one line that names what it waits on, or how it ended.
    pub line: String,
    /// The vault seq the line is stated at.
    pub seq: Option<u64>,
}

impl Need {
    /// Whether `selector`, a want key or a ledger id in hex, names it (`need retire`).
    #[must_use]
    pub fn named(&self, selector: &str) -> bool {
        self.want.key() == selector || aterm_codec::hex::encode_lower(&self.id) == selector
    }
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
    /// Its consumer's check passes without a delivery.
    Unneeded,
    /// A person withdraws it by hand on this Mac (`aterm secrets need retire`), with their note.
    Withdraw(String),
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

/// What the lab had heard on a request when its person is asked, which the ask's words rest on.
#[derive(Debug, Clone, Copy)]
enum Heard<'a> {
    /// No Mac the lab can reach holds it: every member swept and none offered, or none could
    /// be asked.
    Nothing,
    /// The want window ended before these members answered, and none that answered holds it.
    Unanswered(&'a Unanswered),
    /// A Mac that holds it offered it once the person was asked; the members the window's end
    /// named as not having answered, one of which it is unless it joined since.
    Offered(&'a Unanswered),
}

/// What a person is asked for a vault-only want the absence proof shows no Mac holds.
#[must_use]
pub fn ask_for(want: &Want, purpose: Purpose, host: &str) -> Option<Ask> {
    ask_on(want, purpose, host, Heard::Nothing)
}

/// [`ask_for`], in words that rest on what the lab heard. A Mac that did not answer, or one that
/// offered, may hold a copy, which saves making a new value, so that comes first.
fn ask_on(want: &Want, purpose: Purpose, host: &str, heard: Heard<'_>) -> Option<Ask> {
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
                    format!(", for the AI account {label}'s email-code sign-in,")
                }
                _ => String::new(),
            },
            Some(format!(
                "and {label}_totp the same way if the mailbox has 2FA"
            )),
        ),
        Want::MailboxComponent(component) => (
            format!("The {} is missing", component.part().description()),
            if purpose == Purpose::HarnessSignIn {
                ", for an AI account's email-code sign-in,".to_owned()
            } else {
                String::new()
            },
            None,
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
    let hours = WANT_WINDOW_SECS / 3600;
    let copy = want
        .hold_path(std::path::Path::new("~"))
        .map_or_else(|| entry.clone(), |path| path.display().to_string());
    let (on, reasons, first) = match heard {
        Heard::Nothing => (
            "is on no Mac the lab can reach".to_owned(),
            vec!["no Mac the lab can reach holds it".to_owned()],
            None,
        ),
        // A Mac that did not answer may run a build with no sweep, so the words promise no
        // offer: its copy, where it holds one, is the hand route's file.
        Heard::Unanswered(unanswered) => {
            let whom = unanswered.words();
            (
                format!("is on no Mac that answered in {hours} hours, and {whom} did not answer"),
                vec![
                    format!(
                        "no Mac that answered in {hours} hours holds it, and {whom} did not answer"
                    ),
                    format!("no Mac that answered in {hours} hours holds it"),
                ],
                Some(format!(
                    "{whom} did not answer: where one holds {copy}, that copy is the <file> the step takes, so no new one is made"
                )),
            )
        }
        // The custodian's runtime takes an offered copy over the LAN and publishes it, which
        // takes the ask down (§8.2); until it does, that copy is the hand route's file.
        Heard::Offered(unanswered) => {
            let which = if unanswered.is_empty() {
                String::new()
            } else {
                format!(
                    " ({} had not answered when the window ended)",
                    unanswered.words()
                )
            };
            (
                format!(
                    "is on a Mac that offered it{which}, whose copy the vault custodian's running aterm takes over the authenticated LAN connection (until it does, that Mac's {copy} is the <file>)"
                ),
                vec![
                    "a Mac offered it after a person was asked, and its copy has not reached the vault"
                        .to_owned(),
                    "a Mac's offered copy has not reached the vault".to_owned(),
                ],
                Some(format!(
                    "a Mac that holds {} offered it after a person was asked{which}: the custodian's runtime takes the copy on its next authenticated LAN round with that Mac, and until then its {copy} is the file the admin's step takes, so no new one is made",
                    want.title()
                )),
            )
        }
    };
    let reason = reasons
        .into_iter()
        .find(|reason| fits(reason, aterm_messages::need::REASON_CAP))
        .unwrap_or_default();
    let mut needs_you = format!(
        "NEEDS YOU: {}{consumer} {on}: on the vault's admin Mac run {route}",
        want.title()
    );
    if let Some(extra) = &extra {
        needs_you.push_str(&format!(", {extra}"));
    }
    // A fact for later keeps three detail lines, so a mailbox's TOTP rides on the route's.
    let mut more: Vec<String> = first.into_iter().collect();
    more.push(match &extra {
        Some(extra) => format!("on the vault's admin Mac: {route}, {extra}"),
        None => format!("on the vault's admin Mac: {route}"),
    });
    more.push(format!("it comes from: {}", want.source()));
    Some(Ask {
        what,
        step,
        reason,
        more,
        needs_you,
    })
}

/// Whether a member's sweep can offer `want`: its hold probe names a path.
fn offerable(want: &Want) -> bool {
    want.hold_path(std::path::Path::new("~")).is_some()
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

/// `text` as a `Retired` record's field holds it (§5): at most `cap` characters, each control
/// character read as a space, since the table refuses a record that holds one.
#[must_use]
pub fn record_text(text: &str, cap: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(cap)
        .collect()
}

/// A person's note as a `Retired` record carries it (§5): at most [`MAX_NOTE`] characters, each
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
        custodian_silent: false,
        late_offer: false,
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
                    window_ended: false,
                    custodian_silent: false,
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
        Act::Unneeded | Act::Withdraw(_) => {
            let waiting = matches!(
                need.state,
                State::Local
                    | State::Open
                    | State::Offered
                    | State::Published
                    | State::AwaitingPerson
            );
            (waiting && need.needed).then(|| {
                // A withdrawal rests on a person's word, not a check: the want may still be
                // absent, so its evidence and line say who withdrew it and where.
                let (evidence, note, line) = if let Act::Withdraw(note) = act {
                    let host = facts.sight.as_ref().map(|sight| sight.host.as_str());
                    (
                        format!(
                            "withdrawn by hand on {}",
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
        Act::Decline(note) => (need.state == State::AwaitingPerson).then(|| {
            let note = note_of(note);
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
                    raises: need.raises.saturating_add(1),
                    raised_again_on_change: Some(changed(retirement, facts)),
                    asking: false,
                    ask: None,
                    proof: None,
                    got: false,
                    got_authorized: false,
                    posted: false,
                    custodian_silent: false,
                    late_offer: false,
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
    /// A Mac that holds it offered it: the custodian takes the offer (§8.2). On a request
    /// already awaiting a person, the ask names that copy until the custodian's runtime
    /// publishes it, and who the window's end named as not having answered (empty where it
    /// named nobody).
    Offered(Unanswered),
    /// The custodian copied it into this Mac's own host section at vault `seq` (§8.1).
    Published {
        /// The vault version that carries it.
        seq: u64,
    },
    /// Every Mac allowed to give it swept and none holds it, or the want window ended before
    /// the members it names answered, and a person can make it.
    NeedsPerson(Unanswered),
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
    let host = facts
        .sight
        .as_ref()
        .map_or("<host>", |sight| sight.host.as_str());
    match decision {
        // A Mac that answered late holds what a person was asked for. The ask stays up until
        // the custodian's runtime publishes that copy (the model's `LateOffer`), and moves to
        // it, once.
        Decision::Offered(unanswered) if need.state == State::AwaitingPerson => {
            if need.late_offer || !facts.policy.allows(&need.want) {
                return None;
            }
            let ask = ask_on(&need.want, need.purpose, host, Heard::Offered(unanswered))?;
            Some(Need {
                late_offer: true,
                line: ask.needs_you.clone(),
                ask: Some(ask),
                updated: now,
                ..need.clone()
            })
        }
        Decision::Offered(_) => {
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
        Decision::NeedsPerson(unanswered) => {
            // An ask that rests on less than this decision is restated on it, once: one this
            // Mac made standing in for a silent custodian ([`silent`]) on the custodian's own,
            // and one the window's end made on every member's sweep once each has answered.
            let restates = need.state == State::AwaitingPerson
                && !need.late_offer
                && need
                    .proof
                    .is_some_and(|proof| proof.custodian_silent || proof.window_ended);
            if (need.state == State::AwaitingPerson && !restates)
                || !facts.policy.allows(&need.want)
                || facts.holds(&need.want)
            {
                return None;
            }
            // A decision the window's end made rests on fewer sweeps than every member's: it
            // says so, and the ask names who did not answer where one of them could hold it.
            let window = !unanswered.is_empty();
            let heard = if window && offerable(&need.want) {
                Heard::Unanswered(unanswered)
            } else {
                Heard::Nothing
            };
            let ask = ask_on(&need.want, need.purpose, host, heard)?;
            let proof = Proof {
                seq: facts.sight.as_ref().map_or(0, |sight| sight.seq),
                absent: !facts.holds(&need.want),
                every_locker_open: !facts.hidden(),
                before_commons: false,
                swept_unheld: !window,
                window_ended: window,
                custodian_silent: false,
            };
            if restates && need.ask.as_ref() == Some(&ask) && need.proof == Some(proof) {
                return None;
            }
            Some(Need {
                state: State::AwaitingPerson,
                wait: Wait::Person,
                asking: true,
                proof: Some(proof),
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

/// `need` once [`SILENCE_SECS`] passed with no custodian record on its posted request and no
/// offer it could take (§6.1): the vault admin's aterm wrote nothing on it (no window there runs
/// a build with the custodian pass, or its pass waits on what it cannot do), so the line says so
/// and a fact for later sends the person to that Mac ([`exceptions`]). A person-sourced
/// vault-only want this Mac's absence proof shows no Mac but an admin holds goes to a person
/// instead, as before Commons: the model's `NeedsPerson`, this Mac standing in for the silent
/// custodian. A want that reached this Mac's own section (the hand route the line names, run on
/// the admin Mac) is delivered, as a custodian's copy is; a refused value is new only at a later
/// vault version. A member cannot read which Mac is the admin (the admin locker is sealed to the
/// admins, a Commons binding carries no name), so the words name its role. Each pass reads it
/// again, so a sight that was missing or a locker that was hidden then moves it on; `None` for a
/// need not open or offered on the table, or one this observation leaves as it is.
#[must_use]
pub fn silent(need: &Need, facts: &Facts) -> Option<Need> {
    if !need.posted || !matches!(need.state, State::Open | State::Offered) {
        return None;
    }
    if let Some(sight) = facts.sight.as_ref()
        && facts.holds(&need.want)
        && facts.policy.allows(&need.want)
        && need.refused.is_none_or(|refused| sight.seq > refused)
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
    if let Some(sight) = facts.sight.as_ref()
        && need.want.class() == Class::VaultOnly
        && need.want.person_sourced()
        && !sight.hidden
        && !facts.holds(&need.want)
        && facts.policy.allows(&need.want)
        && let Some(ask) = ask_for(&need.want, need.purpose, &sight.host)
    {
        return unmoved(Need {
            state: State::AwaitingPerson,
            wait: Wait::Person,
            asking: true,
            proof: Some(Proof {
                seq: sight.seq,
                absent: true,
                every_locker_open: true,
                before_commons: false,
                swept_unheld: false,
                window_ended: false,
                custodian_silent: true,
            }),
            line: ask.needs_you.clone(),
            ask: Some(ask),
            seq: Some(sight.seq),
            ..silent
        });
    }
    let host = facts
        .sight
        .as_ref()
        .map_or("<host>", |sight| sight.host.as_str());
    let route = hand_route(&need.want, host).map_or_else(String::new, |route| {
        format!("; meanwhile a person with the value runs {route}")
    });
    unmoved(Need {
        wait: Wait::Custodian,
        line: format!(
            "posted to the lab: the vault admin's aterm has written no Commons record on it in the {} hours since it was posted, and no Mac offered it; an aterm window on the vault's admin Mac, on a build with the custodian pass, decides it{route}",
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

/// The rows that ask, one per want key (the same text on every Mac the want blocks): of a want's
/// rows, the one whose ask rests on the most the lab heard (a copy a Mac offered, then who did
/// not answer, which save making a new value), the oldest among equals. A want's other rows
/// (another purpose's, retired or waiting) never hide the one that asks.
fn asking(needs: &[Need]) -> Vec<(&Need, &Ask)> {
    let heard = |need: &Need| {
        (
            need.late_offer,
            need.proof.is_some_and(|proof| proof.window_ended),
        )
    };
    let mut chosen: Vec<(&Need, &Ask)> = Vec::new();
    for (need, ask) in needs
        .iter()
        .filter(|need| need.asking)
        .filter_map(|need| need.ask.as_ref().map(|ask| (need, ask)))
    {
        match chosen
            .iter_mut()
            .find(|(row, _)| row.want.key() == need.want.key())
        {
            Some(row) if heard(need) > heard(row.0) => *row = (need, ask),
            Some(_) => {}
            None => chosen.push((need, ask)),
        }
    }
    chosen
}

/// The exceptions entries of every need awaiting a person, one per want key ([`asking`]), then
/// the one fact for later of the needs a silent custodian left open ([`silent`]).
#[must_use]
pub fn exceptions(needs: &[Need]) -> Vec<aterm_messages::exceptions::Entry> {
    asking(needs)
        .into_iter()
        .filter_map(|(need, _)| exception(need))
        .chain(silent_custodian(needs))
        .collect()
}

/// `secrets.need.custodian.silent`, a fact for later with `Why::YourAction` (§6.1, as §12.4's
/// founding fact): one for the vault. Its step is the one that works whatever the admin Mac
/// runs, each request's `aterm secrets set` there (a delivery ends the request, [`silent`]); its
/// detail lines say a build with the custodian pass decides them itself, then name each
/// request's line while they fit and count the rest. `None` while no need is open past
/// [`SILENCE_SECS`] with no custodian record.
fn silent_custodian(needs: &[Need]) -> Option<aterm_messages::exceptions::Entry> {
    use aterm_messages::exceptions::{Entry, Family, MORE_LINES};
    use aterm_messages::need::{Reason as Words, Then, Why};
    let open: Vec<&Need> = needs
        .iter()
        .filter(|need| need.state == State::Open && need.custodian_silent)
        .collect();
    if open.is_empty() {
        return None;
    }
    let hours = SILENCE_SECS / 3600;
    let wants: BTreeSet<String> = open.iter().map(|need| need.want.key()).collect();
    let mut entry = Entry::new(
        Family::Secrets,
        "custodian",
        "silent",
        "The vault admin's aterm is silent",
        Why::YourAction(Words::new(format!(
            "it wrote no Commons record on a request of this Mac's in {hours} hours"
        ))),
        Then::step("Set each entry the lines below name on the vault's admin Mac"),
    )
    .log_line(format!(
        "secrets need: no custodian record in {hours} hours on {}",
        wants.into_iter().collect::<Vec<_>>().join(", ")
    ))
    .more("each line names its aterm secrets set step there; a window on a build with the custodian pass decides them itself (aterm secrets need pass runs it by hand)");
    let room = MORE_LINES.saturating_sub(1);
    let shown = if open.len() > room {
        room.saturating_sub(1)
    } else {
        room
    };
    for need in open.iter().take(shown) {
        entry = entry.more(format!("{}: {}", need.want.key(), need.line));
    }
    if open.len() > shown {
        entry = entry.more(format!(
            "{} more: aterm secrets need ls names each",
            open.len() - shown
        ));
    }
    Some(entry)
}

/// The `NEEDS YOU:` lines of every need awaiting a person, one per want key ([`asking`]).
#[must_use]
pub fn needs_you(needs: &[Need]) -> Vec<String> {
    asking(needs)
        .into_iter()
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
        reason = "A custodian's copy from vault text into the requester's own host section is aterm_db::credentials::custodian_pass_at on the admin Mac; this ledger takes its Published record in through need::decided, or, where no custodian record came for SILENCE_SECS, need::silent reads the delivery off this Mac's own section (a person's hand route)."
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
        action = "NeedsPerson",
        reason = "A custodian's attested NeedsPerson record is aterm_db::credentials::custodian_pass_at, which this ledger takes in through need::decided; before Commons the requester's own Absent move asks instead, and once SILENCE_SECS passed with no custodian record on a posted request need::silent asks on the same absence proof (aterm_db::credentials::pass_at reads the silence off the table)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbCredentialRequest",
        action = "LateOffer",
        reason = "A request awaiting a person keeps its ask until the runtime accepts and publishes the late holder's delivery; this ledger consumes that Published record, and until it comes need::decided (Decision::Offered) moves the ask to the holder's copy, once."
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
