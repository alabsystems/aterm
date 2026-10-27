// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Vault adoption (design §6.10): with no agh.toml, the two token files the ALab vault's
//! subscriber writes route as if `agh auth add` had provisioned them, with no command run and
//! no file edited by anyone.
//!
//! - `~/.secrets/gh_access_token` is profile `dev`, tier dev, the writer and only reader of the
//!   owner its own login names, and the first reader of every other owner.
//! - `~/.secrets/gh_access_token_alabsystems` is profile `release`, tier release, the writer of
//!   [`aterm_types::channel::PUBLIC_OWNER`] (its reader too while there is no dev token).
//!
//! Each token's login is learned with one `GET /user` through the client's effect seam, sent
//! once, and kept in `<config>/agh/vault.toml` by the token's fingerprint, so a call whose tokens
//! agh already knows sends nothing to learn them. The adopted rows are derived at load and never
//! written to agh.toml. An agh.toml routes instead, and adoption never reads, writes or merges
//! it, unless it says `vault = true`: its rows are then joined by the adopted ones, which it
//! leaves to adoption (it names no profile `dev` or `release`). The rows pass the same L1–L10
//! every agh.toml passes, and `Router::decide` holds the same tier walls over them. A token
//! whose rows would break a rule is left out (`Found::Faulted`) and the rest still route; when
//! the two files hold one token, or two tokens of one login, neither's tier is known and
//! neither routes; the same holds when one file holds the token, or a token of the login, the
//! record keeps for the other file. A record agh cannot parse routes neither token: the guard
//! against a changed login rests on it. Each adopted profile sends exactly the token adoption
//! judged: the call's client is handed that credential (`Github::pin`), never a later read of
//! the file.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::client::{Github, Meter};
#[cfg(unix)]
use crate::config::{self, AghConfig, DefaultRow, OwnerRow, ProfileRow, WriteField};
use crate::config::{ConfigFault, Dirs, Validated};
use crate::error::{Error, IdentityRef, Kind, Result, Sub};
use crate::identity::{Credential, EnvSource, Profile, TokenKind, tilde};
#[cfg(unix)]
use crate::identity::{LoadError, Source};
use crate::policy::{RefusalCode, Router, Tier};

/// The dev token the vault writes, under the home directory.
pub const DEV_TOKEN_FILE: &str = ".secrets/gh_access_token";
/// The release token the vault writes for the public owner, under the home directory.
pub const RELEASE_TOKEN_FILE: &str = ".secrets/gh_access_token_alabsystems";
/// The adoption record, in `Dirs::config`.
pub const RECORD_FILE: &str = "vault.toml";
/// The lock the record is written under, in `Dirs::config`: adoption's own, so a read that
/// records a login never holds up an auth edit.
#[cfg(unix)]
const RECORD_LOCK: &str = ".vault-record.lock";
/// The longest record read.
#[cfg(unix)]
const RECORD_MAX: usize = 4 * 1024;

/// The step for a vault token GitHub refuses (a 401): the secrets subscriber writes the vault's
/// current token, and a token the vault itself holds stale is its admin's to rotate.
pub const REFRESH_STEP: &str = "aterm secrets sync fetches the vault's current token; if the \
                                vault holds this one, its admin rotates it";

/// Why neither vault token routes when both files hold one token.
#[cfg(unix)]
const ONE_TOKEN: &str = "L7: the vault's dev and release files hold one token, so its tier is \
                         unknown and neither routes";

/// The step for the vault's two files when neither's tier is known: the vault's contents, not
/// any row of agh's, are what to fix.
#[must_use]
pub fn pair_next() -> String {
    format!(
        "aterm secrets sync applies the vault's current tokens: agh routes neither until ~/{} is \
         the dev tier's token and ~/{} the release login's",
        DEV_TOKEN_FILE, RELEASE_TOKEN_FILE
    )
}

/// One of the vault's two tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    /// `~/.secrets/gh_access_token`: profile `dev`.
    Dev,
    /// `~/.secrets/gh_access_token_alabsystems`: profile `release`.
    Release,
}

impl Slot {
    /// Both, in profile-name order.
    pub const ALL: [Slot; 2] = [Slot::Dev, Slot::Release];

    /// The profile it is adopted as: `dev` or `release`.
    #[must_use]
    pub const fn profile(self) -> &'static str {
        match self {
            Slot::Dev => "dev",
            Slot::Release => "release",
        }
    }

    /// The slot adopted as profile `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|slot| slot.profile() == name)
    }

    /// Its tier.
    #[must_use]
    pub const fn tier(self) -> Tier {
        match self {
            Slot::Dev => Tier::Dev,
            Slot::Release => Tier::Release,
        }
    }

    /// Its file, relative to the home directory.
    #[must_use]
    pub const fn file(self) -> &'static str {
        match self {
            Slot::Dev => DEV_TOKEN_FILE,
            Slot::Release => RELEASE_TOKEN_FILE,
        }
    }

    /// Its file under `home`.
    #[must_use]
    pub fn path(self, home: &Path) -> PathBuf {
        home.join(self.file())
    }

    /// The other slot.
    #[must_use]
    pub const fn other(self) -> Slot {
        match self {
            Slot::Dev => Slot::Release,
            Slot::Release => Slot::Dev,
        }
    }

    /// The step for its file when it holds no token agh can use: the vault's subscriber
    /// rewrites it, and content the vault itself holds is its admin's to replace.
    #[must_use]
    pub fn sync_step(self) -> String {
        format!(
            "aterm secrets sync rewrites ~/{} from the vault; if the vault holds this content, \
             its admin replaces it",
            self.file()
        )
    }

    /// The owner it reads and writes for: the dev token's own login, or the public owner.
    #[cfg(unix)]
    fn owner(self, login: &str) -> String {
        match self {
            Slot::Dev => login.to_string(),
            Slot::Release => aterm_types::channel::PUBLIC_OWNER.to_string(),
        }
    }
}

/// What adoption found of one vault token.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
    /// There is no such file: nothing to adopt.
    Absent,
    /// The file is there and fails the private-file or token rules: reported, never used.
    Unusable {
        /// `LoadError::slug`.
        slug: &'static str,
        /// The load's refusal.
        why: String,
        /// What makes it usable: for a vault file always a step (`vault::remedy`).
        remedy: Option<String>,
    },
    /// Routed as its profile.
    Adopted {
        /// The token's login, which `GET /user` answered.
        login: String,
        /// The token's fingerprint.
        fingerprint: String,
    },
    /// Its login is known, but its rows would break a rule beside the other rows (both tokens
    /// of one login, L7; a dev token of the public owner, L2): reported, never used, and the
    /// other rows still route.
    Faulted {
        /// The token's login, when it is known.
        login: Option<String>,
        /// The token's fingerprint.
        fingerprint: String,
        /// The rules its rows break, each `{rule}: {what}`.
        why: String,
        /// What makes it route: the vault's contents when no agh.toml joins, else agh.toml's
        /// rows.
        next: String,
    },
    /// The token loads, but its login is not known: not routed this call.
    Unlearned {
        /// The token's fingerprint.
        fingerprint: String,
        /// The token's kind.
        kind: TokenKind,
        /// Why not.
        why: String,
        /// Whether this call asked GitHub (and failed), not only read the record.
        tried: bool,
        /// Whether GitHub answered that the token does not authenticate (a 401), which no
        /// retry changes: `REFRESH_STEP` is its step.
        rejected: bool,
        /// Whether the record holds another fingerprint for this file: the token rotated, and
        /// the next routed call learns it again, refusing it if its login changed.
        rotated: bool,
    },
    /// The token now speaks as another login than the one agh adopted it as: refused.
    Changed {
        /// The token's fingerprint.
        fingerprint: String,
        /// The token's kind.
        kind: TokenKind,
        /// The login adopted earlier.
        adopted: String,
        /// The login GitHub answers now.
        now: String,
    },
}

/// One vault token, where it is, and what adoption found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vaulted {
    /// Which token.
    pub slot: Slot,
    /// Its file.
    pub token_file: PathBuf,
    /// What adoption found.
    pub found: Found,
}

/// What adoption found: each vault token, and the rows routed from the adopted ones (with an
/// agh.toml's that says `vault = true`).
#[derive(Clone, Debug)]
pub struct Adoption {
    vaulted: Vec<Vaulted>,
    validated: Option<Validated>,
    faults: Vec<ConfigFault>,
    refusal: Option<Error>,
    spent: Meter,
    joined: bool,
    /// The credential each adopted profile was judged by, which the call sends.
    pinned: Vec<Arc<Credential>>,
    /// The profiles whose learned login could not be recorded, why, and the step.
    unrecorded: Option<(String, String, String)>,
}

impl Adoption {
    /// Each vault token, `dev` then `release`.
    #[must_use]
    pub fn vaulted(&self) -> &[Vaulted] {
        &self.vaulted
    }

    /// The router over the routed rows: empty when there are none.
    /// # Errors
    /// The refusal (`refusal`): a token rotated to another login, or a record agh cannot
    /// trust.
    pub fn router(&self) -> Result<Router> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        Ok(self
            .validated
            .clone()
            .map_or_else(Router::empty, Router::from_validated))
    }

    /// Why every routed call refuses while adoption stands as it is, if it does.
    #[must_use]
    pub fn refusal(&self) -> Option<&Error> {
        self.refusal.as_ref()
    }

    /// What `agh policy check` reports of the adopted rows: the rules a left-out token's rows
    /// break (`Found::Faulted`), and a record agh cannot read.
    #[must_use]
    pub fn faults(&self) -> &[ConfigFault] {
        &self.faults
    }

    /// The routed profile so named.
    #[must_use]
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.validated.as_ref()?.profiles().get(name)
    }

    /// Whether any token was adopted.
    #[must_use]
    pub fn adopted_any(&self) -> bool {
        self.vaulted
            .iter()
            .any(|v| matches!(v.found, Found::Adopted { .. }))
    }

    /// Whether profile `name` is a vault token adopted this call.
    #[must_use]
    pub fn adopted(&self, name: &str) -> bool {
        self.vaulted
            .iter()
            .any(|v| v.slot.profile() == name && matches!(v.found, Found::Adopted { .. }))
    }

    /// Whether an agh.toml that says `vault = true` joined its rows to the adopted ones.
    #[must_use]
    pub const fn joined(&self) -> bool {
        self.joined
    }

    /// The requests adoption sent to learn logins, which the call's receipt counts.
    #[must_use]
    pub const fn spent(&self) -> &Meter {
        &self.spent
    }

    /// The credentials the adopted profiles were judged by. The call's client sends exactly
    /// these (`Github::pin`), so a token file rewritten after adoption never speaks for the
    /// rows adoption derived from the token it judged.
    #[must_use]
    pub fn pinned(&self) -> &[Arc<Credential>] {
        &self.pinned
    }

    /// The step for what adoption reports as faults, else for a vault token found and not
    /// routed: a left-out token's fix, then the record's, then the first other token's step.
    /// `None` when there is none.
    #[must_use]
    pub fn next_step(&self) -> Option<String> {
        self.vaulted
            .iter()
            .find_map(|v| match &v.found {
                Found::Faulted { next, .. } => Some(next.clone()),
                _ => None,
            })
            .or_else(|| self.unrecorded.as_ref().map(|(_, _, next)| next.clone()))
            .or_else(|| self.vaulted.iter().find_map(Vaulted::next))
    }

    /// The step for `error`, from a call that routed by adoption's rows, where the error's own
    /// step names something adoption has none of: a 401 for an adopted token is the vault's to
    /// refresh (`REFRESH_STEP`), never an agh.toml edit; an owner with no writer or no reader
    /// left is the public owner's release file's (`~/.secrets/gh_access_token_alabsystems`) or
    /// a dev token found and not routed, and with no agh.toml, any other owner's is a profile
    /// of its own (`agh auth add`), since there is no row to edit. `None` keeps the error's.
    #[must_use]
    pub fn next_for(&self, error: &Error) -> Option<String> {
        if error.kind == Kind::Unauthenticated
            && let Some(IdentityRef::Profile { name, .. }) = &error.identity
            && self.adopted(name)
        {
            return Some(REFRESH_STEP.to_string());
        }
        let Sub::Policy(
            RefusalCode::WriteNone
            | RefusalCode::ChainExhausted
            | RefusalCode::ChainStoppedUnusable,
        ) = error.sub
        else {
            return None;
        };
        let owner = error.repo.as_deref()?.split('/').next()?;
        if owner.eq_ignore_ascii_case(aterm_types::channel::PUBLIC_OWNER) {
            return self.slot_step(Slot::Release);
        }
        if self.joined {
            return None;
        }
        match self.vaulted.iter().find(|v| v.slot == Slot::Dev) {
            Some(Vaulted {
                found: Found::Absent | Found::Adopted { .. },
                ..
            })
            | None => Some(format!(
                "agh auth add NAME --file PATH --tier dev|external --owner {owner} --write, with \
                 a NAME other than dev or release: its rows join the vault's"
            )),
            Some(_) => self.slot_step(Slot::Dev),
        }
    }

    /// The step for `slot`'s token when it does not route: an absent file is the secrets
    /// subscriber's to write.
    fn slot_step(&self, slot: Slot) -> Option<String> {
        let vaulted = self.vaulted.iter().find(|v| v.slot == slot)?;
        match &vaulted.found {
            Found::Absent => Some(format!(
                "~/{} is absent: aterm secrets sync writes it where the vault holds it",
                slot.file()
            )),
            _ => vaulted.next(),
        }
    }

    /// The stderr lines of a call that routes without a vault token it found: one for the
    /// tokens whose login this call could not reach GitHub to learn, one for each token GitHub
    /// refused (a 401), one for each unusable token, one for the tokens left out for one fault
    /// (both files, when neither's tier is known), and one when a learned login could not be
    /// recorded. Empty when every token found routes and is recorded.
    #[must_use]
    pub fn note(&self, home: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        let failed: Vec<(&Vaulted, &str)> = self
            .vaulted
            .iter()
            .filter_map(|v| match &v.found {
                Found::Unlearned {
                    why,
                    tried: true,
                    rejected: false,
                    ..
                } => Some((v, why.as_str())),
                _ => None,
            })
            .collect();
        if let Some((first, why)) = failed.first() {
            let names: Vec<&str> = failed.iter().map(|(v, _)| v.slot.profile()).collect();
            lines.push(note_line(
                &names.join(","),
                &format!(
                    "{} ({}) was not adopted: {why}",
                    first.slot.profile(),
                    tilde(&first.token_file, home)
                ),
                "this call routes without it; the next call asks again",
            ));
        }
        let mut faulted: Vec<(Vec<&Vaulted>, &str, &str)> = Vec::new();
        for v in &self.vaulted {
            match &v.found {
                Found::Unlearned {
                    why,
                    rejected: true,
                    ..
                } => lines.push(note_line(
                    v.slot.profile(),
                    &format!(
                        "{} ({}) was not adopted: GitHub refused it: {why}",
                        v.slot.profile(),
                        tilde(&v.token_file, home)
                    ),
                    REFRESH_STEP,
                )),
                Found::Unusable { slug, why, .. } => lines.push(note_line(
                    v.slot.profile(),
                    &format!(
                        "{} ({}) is not used: {slug}: {why}",
                        v.slot.profile(),
                        tilde(&v.token_file, home)
                    ),
                    &v.next().unwrap_or_else(|| v.slot.sync_step()),
                )),
                Found::Faulted { why, next, .. } => {
                    match faulted
                        .iter_mut()
                        .find(|(_, w, n)| *w == why.as_str() && *n == next.as_str())
                    {
                        Some((group, _, _)) => group.push(v),
                        None => faulted.push((vec![v], why, next)),
                    }
                }
                _ => {}
            }
        }
        for (group, why, next) in faulted {
            let names: Vec<&str> = group.iter().map(|v| v.slot.profile()).collect();
            let subjects: Vec<String> = group
                .iter()
                .map(|v| format!("{} ({})", v.slot.profile(), tilde(&v.token_file, home)))
                .collect();
            let verb = if group.len() == 1 { "is" } else { "are" };
            lines.push(note_line(
                &names.join(","),
                &format!("{} {verb} not used: {why}", subjects.join(" and ")),
                next,
            ));
        }
        if let Some((names, why, next)) = &self.unrecorded {
            lines.push(note_line(names, why, next));
        }
        lines
    }

    /// Adoption of `vaulted`, nothing routed, left out or refused yet.
    #[cfg(unix)]
    fn bare(vaulted: Vec<Vaulted>, joined: bool) -> Adoption {
        Adoption {
            vaulted,
            validated: None,
            faults: Vec::new(),
            refusal: None,
            spent: Meter::default(),
            joined,
            pinned: Vec::new(),
            unrecorded: None,
        }
    }

    /// Each vault token found, and the routed rows: `base` (an agh.toml that says
    /// `vault = true`) joined by the adopted tokens' rows, judged by L1–L10. The two files'
    /// tokens, when neither's tier is known (`one_login`), are both left out first. When the
    /// rest break a rule, the most tokens whose rows do not (dev before release) are kept and
    /// each one left out is `Found::Faulted`, the faults its rows brought reported.
    #[cfg(unix)]
    fn judge(mut vaulted: Vec<Vaulted>, home: &Path, base: Option<&AghConfig>) -> Adoption {
        let joined = base.is_some();
        if let Some((slot, adopted, now, path)) = vaulted.iter().find_map(|v| match &v.found {
            Found::Changed { adopted, now, .. } => Some((v.slot, adopted, now, &v.token_file)),
            _ => None,
        }) {
            let name = slot.profile();
            let refusal = Error::refused(format!(
                "the vault's {name} token ({}) now speaks as {now}, and agh adopted it as \
                 {adopted}: agh never routes a token to another owner unasked",
                tilde(path, home)
            ))
            .with_next(format!(
                "agh auth remove {name} forgets {adopted}, and the next call adopts the token as \
                 {now}"
            ));
            return Adoption {
                refusal: Some(refusal),
                ..Adoption::bare(vaulted, joined)
            };
        }
        let mut faults = one_login(&mut vaulted);
        let adopted: Vec<Slot> = vaulted
            .iter()
            .filter(|v| matches!(v.found, Found::Adopted { .. }))
            .map(|v| v.slot)
            .collect();
        if adopted.is_empty() && base.is_none() {
            return Adoption {
                faults,
                ..Adoption::bare(vaulted, joined)
            };
        }
        let mut first_faults: Option<Vec<ConfigFault>> = None;
        let mut kept: Option<(Vec<Slot>, Validated)> = None;
        for keep in subsets(&adopted) {
            let rows = joined_rows(base, rows_of(&vaulted, &keep));
            match config::validate(&rows, home, &config::file_id) {
                Ok(validated) => {
                    kept = Some((keep, validated));
                    break;
                }
                Err(faults) => {
                    first_faults.get_or_insert(faults);
                }
            }
        }
        let Some((keep, validated)) = kept else {
            // Not even `base` alone validates: `config::load` refuses it before a call gets
            // here, and `agh policy check` reports its faults as agh.toml's.
            let refusal = config::invalid(
                "agh.toml breaks L1–L10, so the vault's rows cannot join it".to_string(),
                config::POLICY_CHECK,
            );
            return Adoption {
                faults,
                refusal: Some(refusal),
                ..Adoption::bare(vaulted, joined)
            };
        };
        let left_out = if keep.len() == adopted.len() {
            Vec::new()
        } else {
            first_faults.unwrap_or_default()
        };
        let why = list(&left_out);
        for v in &mut vaulted {
            if keep.contains(&v.slot) {
                continue;
            }
            if let Found::Adopted { login, fingerprint } = &v.found {
                v.found = Found::Faulted {
                    login: Some(login.clone()),
                    fingerprint: fingerprint.clone(),
                    why: why.clone(),
                    next: faulted_next(v.slot, joined),
                };
            }
        }
        faults.extend(left_out);
        Adoption {
            validated: Some(validated),
            faults,
            ..Adoption::bare(vaulted, joined)
        }
    }
}

impl Vaulted {
    /// The step that makes this token route, when it is found and does not: an unusable
    /// file's remedy, a left-out token's fix, `REFRESH_STEP` for a token GitHub refused, and
    /// for one whose login GitHub could not be reached for, the next call. `None` for an
    /// absent, adopted, unasked or changed one (a change is the call's refusal, which names
    /// its step).
    #[must_use]
    pub fn next(&self) -> Option<String> {
        match &self.found {
            Found::Unusable { remedy, .. } => {
                Some(remedy.clone().unwrap_or_else(|| self.slot.sync_step()))
            }
            Found::Faulted { next, .. } => Some(next.clone()),
            Found::Unlearned { rejected: true, .. } => Some(REFRESH_STEP.to_string()),
            Found::Unlearned { tried: true, .. } => {
                Some("the next call asks GitHub for its login again".to_string())
            }
            Found::Absent
            | Found::Adopted { .. }
            | Found::Unlearned { .. }
            | Found::Changed { .. } => None,
        }
    }
}

/// The step for a token left out because its rows break a rule beside the others: with no
/// agh.toml the rows are derived, so the cause is the vault's contents; beside an agh.toml
/// that says `vault = true`, its rows.
#[cfg(unix)]
fn faulted_next(slot: Slot, joined: bool) -> String {
    if joined {
        format!(
            "agh policy check names the rules agh.toml's rows break beside the vault's {}: \
             change those rows (agh auth remove NAME), or aterm secrets sync applies a corrected \
             vault",
            slot.profile()
        )
    } else {
        format!(
            "~/{} is not used until it holds a {} tier token of its own login: aterm secrets \
             sync applies the vault's current tokens",
            slot.file(),
            slot.tier().slug()
        )
    }
}

/// The vault's two files when neither token's tier is known: both hold one token, or one holds
/// the token or a token of the login the record keeps for the other (`run` left each out with
/// `pair_next`), or GitHub answered one login for the two tokens, which leaves both out here.
/// Keeping either would be unsafe: the dev slot's rows make its token every owner's first
/// reader, and the release slot's make its token alabsystems' writer. Their L7 fault, once for
/// each reason, or none.
#[cfg(unix)]
fn one_login(vaulted: &mut [Vaulted]) -> Vec<ConfigFault> {
    let shared = match &*vaulted {
        [first, second] => match (&first.found, &second.found) {
            (Found::Adopted { login: a, .. }, Found::Adopted { login: b, .. })
                if a.eq_ignore_ascii_case(b) =>
            {
                Some(a.clone())
            }
            _ => None,
        },
        _ => None,
    };
    let pair = pair_next();
    if let Some(login) = shared {
        let why = format!(
            "L7: the vault's dev and release tokens both speak as {login}, so neither's tier is \
             known and neither routes"
        );
        for v in vaulted.iter_mut() {
            if let Found::Adopted { login, fingerprint } = &v.found {
                v.found = Found::Faulted {
                    login: Some(login.clone()),
                    fingerprint: fingerprint.clone(),
                    why: why.clone(),
                    next: pair.clone(),
                };
            }
        }
    }
    let mut faults: Vec<ConfigFault> = Vec::new();
    for v in vaulted.iter() {
        if let Found::Faulted { why, next, .. } = &v.found
            && *next == pair
        {
            let what = why.strip_prefix("L7: ").unwrap_or(why).to_string();
            if !faults.iter().any(|f| f.what == what) {
                faults.push(ConfigFault { rule: "L7", what });
            }
        }
    }
    faults
}

/// `agh: note vault={names} why={why} next={next}`, each value as a receipt's.
fn note_line(names: &str, why: &str, next: &str) -> String {
    let mut line = format!("agh: note vault={names} why=");
    crate::cli::out::receipt_value(&mut line, why);
    line.push_str(" next=");
    crate::cli::out::receipt_value(&mut line, next);
    line
}

/// Each subset of `slots`, the largest first and, of one size, in slot order: the rows tried,
/// in turn, until one validates. The empty one is last.
#[cfg(unix)]
fn subsets(slots: &[Slot]) -> Vec<Vec<Slot>> {
    let n = slots.len();
    let mut all: Vec<Vec<Slot>> = (0..1u32 << n)
        .map(|mask| {
            slots
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, slot)| *slot)
                .collect()
        })
        .collect();
    all.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    all
}

/// `faults` as one text: each `{rule}: {what}`, joined with `; `.
#[cfg(unix)]
fn list(faults: &[ConfigFault]) -> String {
    faults
        .iter()
        .map(|f| format!("{}: {what}", f.rule, what = f.what))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The rows `agh auth add NAME --file VAULT_FILE --tier T --owner OWNER --write --drafts` writes
/// for each token in `keep`, as the canonical agh.toml routes (design §6.2): the dev token
/// reads first for every owner but the release floor's writer reads, the public owner's reads
/// included, then anonymous; the release token reads the public owner only while there is no
/// dev token.
#[cfg(unix)]
fn rows_of(vaulted: &[Vaulted], keep: &[Slot]) -> AghConfig {
    let mut config = AghConfig::default();
    let dev = keep.contains(&Slot::Dev);
    for v in vaulted.iter().filter(|v| keep.contains(&v.slot)) {
        let Found::Adopted { login, fingerprint } = &v.found else {
            continue;
        };
        let name = v.slot.profile();
        config.profiles.insert(
            name.to_string(),
            ProfileRow {
                token_file: Some(format!("~/{}", v.slot.file())),
                tier: v.slot.tier().slug().to_string(),
                login: Some(login.clone()),
                drafts_fp: Some(fingerprint.clone()),
                ..ProfileRow::default()
            },
        );
        let read = match v.slot {
            Slot::Release if dev => vec![Slot::Dev.profile().to_string(), "anonymous".into()],
            Slot::Dev | Slot::Release => vec![name.to_string()],
        };
        config.owners.push(OwnerRow {
            name: v.slot.owner(login),
            tier: v.slot.tier().slug().to_string(),
            read,
            write: WriteField::One(name.to_string()),
        });
    }
    if dev {
        config.default = Some(DefaultRow {
            read: vec![Slot::Dev.profile().to_string(), "anonymous".into()],
            write: None,
        });
    }
    config
}

/// `base`'s rows (an agh.toml that says `vault = true`) joined by `adopted`'s: a profile, an
/// owner row or `[default]` `base` has stands, and `vault` is cleared, since the joined rows
/// name the vault's profiles.
#[cfg(unix)]
fn joined_rows(base: Option<&AghConfig>, adopted: AghConfig) -> AghConfig {
    let mut config = base.cloned().unwrap_or_default();
    config.vault = false;
    for (name, row) in adopted.profiles {
        config.profiles.entry(name).or_insert(row);
    }
    for row in adopted.owners {
        if !config
            .owners
            .iter()
            .any(|own| own.name.eq_ignore_ascii_case(&row.name))
        {
            config.owners.push(row);
        }
    }
    if config.default.is_none() {
        config.default = adopted.default;
    }
    config
}

/// Adoption for a call: `None` when an agh.toml that does not say `vault = true` is there (or
/// could not be judged absent), and off unix, where no token file is read. A token whose login
/// the record does not hold for its fingerprint is learned with one `GET /user` as that token
/// through `gh` (its effect seam, its transport and its redactor), sent once with no retry, and
/// what was learned is recorded under adoption's own lock. A failed `GET /user` adopts nothing
/// for that token, records nothing, and leaves `note`'s line; once one could not reach GitHub,
/// the other token's is not sent. One token in both files is not sent at all.
#[must_use]
pub fn adopt(gh: &Github, dirs: &Dirs) -> Option<Adoption> {
    run(dirs, gh.env(), Some(gh))
}

/// Adoption as the record and the files stand, asking GitHub nothing: a token the record does
/// not hold for its fingerprint is `Unlearned` (`tried: false`). `agh policy check` and a call
/// under `--anonymous` read it.
#[must_use]
pub fn view(dirs: &Dirs, env: &dyn EnvSource) -> Option<Adoption> {
    run(dirs, env, None)
}

/// The text `agh auth add NAME` starts a new agh.toml from when there is none: `vault = true`,
/// so the vault's tokens keep joining as adoption derives them, while either vault token file
/// is there (usable or not). `None` when neither is, and when the new profile is the vault's
/// own (`dev` or `release`) or its token file (`file`) is one of the vault's files: that
/// agh.toml routes alone, as any explicit agh.toml does, so the file it names is routed once.
#[cfg(unix)]
#[must_use]
pub fn seed(dirs: &Dirs, name: &str, file: Option<&Path>) -> Option<String> {
    if Slot::named(name).is_some() || file.is_some_and(|file| is_vault_file(dirs, file)) {
        return None;
    }
    any_file(dirs).then(|| {
        "# agh.toml: routing only; no secret lives in this file. vault = true: the vault's\n\
         # tokens join these rows as agh adopts them with no agh.toml (design §6.10).\n\
         vault = true\n"
            .to_string()
    })
}

/// Whether either vault token file is there, usable or not.
#[cfg(unix)]
pub(crate) fn any_file(dirs: &Dirs) -> bool {
    Slot::ALL
        .into_iter()
        .any(|slot| std::fs::symlink_metadata(slot.path(&dirs.home)).is_ok())
}

/// Whether `path` ends in one of the vault's token file names (`~/.secrets/gh_access_token`,
/// `~/.secrets/gh_access_token_alabsystems`), judged by its components alone, where no home
/// directory is at hand to say whose.
#[must_use]
pub fn names_vault_path(path: &Path) -> bool {
    Slot::ALL
        .into_iter()
        .any(|slot| path.ends_with(slot.file()))
}

/// Whether `file` is one of the vault's two token files, as named or once both resolve.
#[cfg(unix)]
pub(crate) fn is_vault_file(dirs: &Dirs, file: &Path) -> bool {
    let resolved = std::fs::canonicalize(file).ok();
    Slot::ALL.into_iter().any(|slot| {
        let path = slot.path(&dirs.home);
        path == file
            || resolved.as_ref().is_some_and(|resolved| {
                std::fs::canonicalize(&path).ok().as_ref() == Some(resolved)
            })
    })
}

/// Off unix there is no permission check, so no token file is read (design §6.6).
#[cfg(not(unix))]
fn run(dirs: &Dirs, env: &dyn EnvSource, gh: Option<&Github>) -> Option<Adoption> {
    let _ = (dirs, env, gh);
    None
}

/// `adopt` and `view` on unix.
#[cfg(unix)]
fn run(dirs: &Dirs, env: &dyn EnvSource, gh: Option<&Github>) -> Option<Adoption> {
    let base = base(dirs)?;
    let joined = base.is_some();
    let (record, damaged) = match read_record(dirs) {
        Ok(record) => (record, None),
        Err(fault) if fault.damaged => (Record::default(), Some(fault)),
        Err(RecordFault { error, fault, .. }) => {
            let vaulted = Slot::ALL
                .into_iter()
                .map(|slot| Vaulted {
                    slot,
                    token_file: slot.path(&dirs.home),
                    found: Found::Absent,
                })
                .collect();
            return Some(Adoption {
                faults: vec![fault],
                refusal: Some(error),
                ..Adoption::bare(vaulted, joined)
            });
        }
    };
    let loaded: Vec<(Slot, std::result::Result<Arc<Credential>, Found>)> = Slot::ALL
        .into_iter()
        .map(|slot| (slot, load_slot(slot, dirs, env)))
        .collect();
    let one_token = matches!(
        loaded.as_slice(),
        [(_, Ok(a)), (_, Ok(b))] if a.fingerprint() == b.fingerprint()
    );
    let pair_why = if one_token {
        Some(ONE_TOKEN.to_string())
    } else {
        loaded.iter().find_map(|(slot, load)| {
            let cred = load.as_ref().ok()?;
            record
                .get(slot.other())
                .filter(|known| known.fingerprint == cred.fingerprint())
                .map(|_| misplaced_token(*slot))
        })
    };
    let mut learning = Learning {
        gh,
        home: &dirs.home,
        learned: Vec::new(),
        spent: Meter::default(),
        unreachable: None,
    };
    let mut pinned = Vec::new();
    let mut vaulted = Vec::new();
    for (slot, load) in loaded {
        let recorded = record.get(slot);
        let found = match load {
            Err(found) => found,
            Ok(cred) => match (&damaged, &pair_why) {
                (Some(damaged), _) => Found::Faulted {
                    login: None,
                    fingerprint: cred.fingerprint().to_string(),
                    why: format!("{}: {}", damaged.fault.rule, damaged.error.why),
                    next: damaged.error.next.clone(),
                },
                (None, Some(why)) => Found::Faulted {
                    login: [recorded, record.get(slot.other())]
                        .into_iter()
                        .flatten()
                        .find(|known| known.fingerprint == cred.fingerprint())
                        .map(|known| known.login.clone()),
                    fingerprint: cred.fingerprint().to_string(),
                    why: why.clone(),
                    next: pair_next(),
                },
                (None, None) => {
                    let found = learning.find(slot, &cred, recorded);
                    pinned.push(cred);
                    found
                }
            },
        };
        vaulted.push(Vaulted {
            slot,
            token_file: slot.path(&dirs.home),
            found,
        });
    }
    if let Some(why) = misplaced_login(&vaulted, &record) {
        for v in &mut vaulted {
            let (login, fingerprint) = match &v.found {
                Found::Adopted { login, fingerprint } => (Some(login.clone()), fingerprint),
                Found::Changed {
                    now, fingerprint, ..
                } => (Some(now.clone()), fingerprint),
                Found::Unlearned { fingerprint, .. } => (None, fingerprint),
                Found::Absent | Found::Unusable { .. } | Found::Faulted { .. } => continue,
            };
            v.found = Found::Faulted {
                login,
                fingerprint: fingerprint.clone(),
                why: why.clone(),
                next: pair_next(),
            };
        }
        learning.learned.clear();
    }
    let mut unrecorded = None;
    if !learning.learned.is_empty() {
        let (moved, refused) = record_learned(dirs, &learning.learned);
        for (slot, recorded) in moved {
            if let Some(v) = vaulted.iter_mut().find(|v| v.slot == slot)
                && let Found::Adopted { login, fingerprint } = &v.found
            {
                v.found = Found::Changed {
                    fingerprint: fingerprint.clone(),
                    kind: kind_of(&learning.learned, slot),
                    adopted: recorded,
                    now: login.clone(),
                };
            }
        }
        unrecorded = refused.map(|error| (learning.names(), error));
    } else if gh.is_none()
        && vaulted
            .iter()
            .any(|v| matches!(v.found, Found::Unlearned { .. }))
    {
        unrecorded = record_blocked(dirs).map(|error| (unlearned_names(&vaulted), error));
    }
    let mut adoption = Adoption::judge(vaulted, &dirs.home, base.as_ref());
    adoption.spent = learning.spent;
    adoption.pinned = pinned
        .into_iter()
        .filter(|cred| adoption.adopted(cred.profile()))
        .collect();
    if let Some(damaged) = damaged {
        adoption.faults.push(damaged.fault);
    }
    if let Some((names, error)) = unrecorded {
        let why = format!("the record could not be written: {}", error.why);
        adoption.faults.push(ConfigFault {
            rule: "file",
            what: format!(
                "{why}, so each call asks GitHub for the vault's logins again; remedy: {}",
                error.next
            ),
        });
        adoption.unrecorded = Some((names, why, error.next.clone()));
    }
    Some(adoption)
}

/// The kind of the token learned for `slot`.
#[cfg(unix)]
fn kind_of(learned: &[Learn], slot: Slot) -> TokenKind {
    learned
        .iter()
        .find(|l| l.slot == slot)
        .map_or(TokenKind::Unknown, |l| l.kind)
}

/// The profiles of the tokens whose login is not known, joined with `,`.
#[cfg(unix)]
fn unlearned_names(vaulted: &[Vaulted]) -> String {
    vaulted
        .iter()
        .filter(|v| matches!(v.found, Found::Unlearned { .. }))
        .map(|v| v.slot.profile())
        .collect::<Vec<_>>()
        .join(",")
}

/// Why neither token routes when `slot`'s file holds the token the record keeps for the other
/// file: the two files' contents are crossed, so neither's tier is known.
#[cfg(unix)]
fn misplaced_token(slot: Slot) -> String {
    format!(
        "L7: ~/{} holds the token agh recorded for ~/{}, so its tier is unknown and neither \
         routes",
        slot.file(),
        slot.other().file()
    )
}

/// Why neither token routes when one speaks (as learned or as recorded) as the login the record
/// keeps for the other file, whatever that file holds this call: `None` when none does. The
/// other file's token then may be absent, unusable or not learned this call, so the check
/// `one_login` makes of two adopted tokens would miss it.
#[cfg(unix)]
fn misplaced_login(vaulted: &[Vaulted], record: &Record) -> Option<String> {
    vaulted.iter().find_map(|v| {
        let login = match &v.found {
            Found::Adopted { login, .. } => login,
            Found::Changed { now, .. } => now,
            _ => return None,
        };
        let other = v.slot.other();
        record
            .get(other)
            .filter(|known| known.login.eq_ignore_ascii_case(login))
            .map(|_| {
                format!(
                    "L7: the vault's {} token speaks as {login}, the login agh recorded for its \
                     {} token, so neither's tier is known and neither routes",
                    v.slot.profile(),
                    other.profile()
                )
            })
    })
}

/// The rows adoption joins: `Some(None)` with no agh.toml, `Some(Some(rows))` for one that says
/// `vault = true`, and `None` for any other (which routes alone, or which `config::load`
/// refuses).
#[cfg(unix)]
fn base(dirs: &Dirs) -> Option<Option<AghConfig>> {
    match config::read_config(&dirs.config, config::CONFIG_FILE, &dirs.home) {
        config::ConfigText::Missing => Some(None),
        config::ConfigText::Text(text) => match config::parse(&text) {
            Ok(rows) if rows.vault => Some(Some(rows)),
            _ => None,
        },
        _ => None,
    }
}

/// One token's login learned this call, and the record's row `find` judged it against.
#[cfg(unix)]
struct Learn {
    slot: Slot,
    kind: TokenKind,
    recorded: Option<Learned>,
    row: Learned,
}

/// One vault token loaded by the private-file and token rules: its credential, or what
/// adoption reports of it (`Absent`, `Unusable`).
#[cfg(unix)]
fn load_slot(
    slot: Slot,
    dirs: &Dirs,
    env: &dyn EnvSource,
) -> std::result::Result<Arc<Credential>, Found> {
    let path = slot.path(&dirs.home);
    let profile = slot_profile(slot, &dirs.home);
    match super::load(&profile, env) {
        Ok(cred) => Ok(Arc::new(cred)),
        Err(LoadError::Missing) => Err(Found::Absent),
        Err(error) => Err(Found::Unusable {
            slug: error.slug(),
            why: error.to_string(),
            remedy: Some(remedy(&error, slot, &path, &dirs.home)),
        }),
    }
}

/// The one-file profile a vault token loads and learns as.
#[cfg(unix)]
fn slot_profile(slot: Slot, home: &Path) -> Profile {
    Profile::new(slot.profile(), Source::File(slot.path(home)), slot.tier())
}

/// What one call learns: the client it asks through (`None`: it asks nothing), the logins it
/// learned, the requests it sent, and why GitHub could not be reached, once it could not.
#[cfg(unix)]
struct Learning<'a> {
    gh: Option<&'a Github>,
    home: &'a Path,
    learned: Vec<Learn>,
    spent: Meter,
    unreachable: Option<String>,
}

#[cfg(unix)]
impl Learning<'_> {
    /// The profiles learned this call, joined with `,`.
    fn names(&self) -> String {
        self.learned
            .iter()
            .map(|l| l.slot.profile())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// What the loaded vault token `cred` is, learning its login when the record lacks it.
    /// Once GitHub could not be reached this call, a later token is not sent: it waits out no
    /// second connect deadline, and is `Unlearned` for the same reason.
    fn find(&mut self, slot: Slot, cred: &Arc<Credential>, recorded: Option<&Learned>) -> Found {
        let fingerprint = cred.fingerprint().to_string();
        if let Some(known) = recorded.filter(|known| known.fingerprint == fingerprint) {
            return Found::Adopted {
                login: known.login.clone(),
                fingerprint,
            };
        }
        let unlearned = |why: String, tried: bool, rejected: bool| Found::Unlearned {
            fingerprint: fingerprint.clone(),
            kind: cred.kind(),
            why,
            tried,
            rejected,
            rotated: recorded.is_some(),
        };
        let Some(gh) = self.gh else {
            let why = if recorded.is_some() {
                "rotated: the file holds another token than the one agh recorded; the next \
                 routed call learns its login again, and refuses it if that login changed"
            } else {
                "agh learns its login with GET /user on the next call that routes"
            };
            return unlearned(why.to_string(), false, false);
        };
        if let Some(why) = &self.unreachable {
            return unlearned(why.clone(), true, false);
        }
        let profile = slot_profile(slot, self.home);
        match learn(gh, &profile, cred, &mut self.spent) {
            Err(error) => {
                let why = format!("{}: {}", error.kind.slug(), error.why);
                if error.kind == Kind::CouldNotRun {
                    self.unreachable = Some(why.clone());
                }
                unlearned(why, true, error.kind == Kind::Unauthenticated)
            }
            Ok(now) => match recorded {
                Some(known) if !known.login.eq_ignore_ascii_case(&now) => Found::Changed {
                    fingerprint,
                    kind: cred.kind(),
                    adopted: known.login.clone(),
                    now,
                },
                _ => {
                    self.learned.push(Learn {
                        slot,
                        kind: cred.kind(),
                        recorded: recorded.cloned(),
                        row: Learned {
                            fingerprint: fingerprint.clone(),
                            login: now.clone(),
                        },
                    });
                    Found::Adopted {
                        login: now,
                        fingerprint,
                    }
                }
            },
        }
    }
}

/// `GET /user` as `profile` alone (the provisioning bind of `agh auth add`), sent once
/// (`Users::me_once`: the next call asks again) with exactly `cred`, the token adoption judged
/// (`Github::pin`): the login. What it sent is added to `spent`.
#[cfg(unix)]
fn learn(
    gh: &Github,
    profile: &Profile,
    cred: &Arc<Credential>,
    spent: &mut Meter,
) -> Result<String> {
    let validated = Validated {
        profiles: BTreeMap::from([(profile.name.clone(), profile.clone())]),
        owners: BTreeMap::new(),
        default_read: Vec::new(),
        targets: BTreeMap::new(),
        vault: false,
        origin: config::ConfigOrigin::Text,
        writers: Vec::new(),
    };
    let bound = gh.with_router(Router::from_validated(validated));
    bound.pin(cred.clone());
    let answered = bound.users().me_once(&profile.name);
    let meter = bound.meter();
    spent.requests = spent.requests.saturating_add(meter.requests);
    if meter.as_.is_some() {
        spent.as_.clone_from(&meter.as_);
    }
    if meter.rate.is_some() {
        spent.rate.clone_from(&meter.rate);
    }
    let user = answered?;
    let sent_as_loaded = matches!(
        &meter.as_,
        Some(IdentityRef::Profile { name, fingerprint: sent })
            if *name == profile.name && sent == cred.fingerprint()
    );
    if !sent_as_loaded {
        return Err(Error::refused(
            "GET /user was not sent as the token agh loaded",
        ));
    }
    if !config::is_owner_name(&user.login) {
        return Err(Error::refused(
            "GitHub answered a login that is no GitHub owner name",
        ));
    }
    Ok(user.login)
}

/// The remedy for a vault token that does not load, always a step: for a loose file the chmods
/// (a profile of agh's own would be an agh.toml, which ends adoption), for a link the file
/// itself, and for any other (content that is not one usable token, a file agh cannot read, or
/// a loose one no chmod of this user's fixes) the vault's rewrite (`Slot::sync_step`), since
/// the file is the secrets subscriber's.
#[cfg(unix)]
fn remedy(error: &LoadError, slot: Slot, path: &Path, home: &Path) -> String {
    use crate::identity::{Held, PrivateFix, private_fix};
    match error {
        LoadError::Loose(_) => match path
            .parent()
            .map(|dir| private_fix(dir, path, home, Held::Token))
        {
            Some(PrivateFix::Fix(chmods)) => chmods,
            Some(PrivateFix::NotAFile) => {
                format!("{} must be a regular file", tilde(path, home))
            }
            Some(PrivateFix::NoChmod | PrivateFix::Unseen) | None => slot.sync_step(),
        },
        LoadError::Symlink => format!(
            "{} must be the file itself, in a real directory of yours: replace the link",
            tilde(path, home)
        ),
        LoadError::Missing
        | LoadError::Malformed(_)
        | LoadError::Unsupported(_)
        | LoadError::Io(_)
        | LoadError::Unavailable(_) => slot.sync_step(),
    }
}

/// One token's login, by the fingerprint `GET /user` answered it for.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Learned {
    fingerprint: String,
    login: String,
}

/// The record's rows.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Record {
    dev: Option<Learned>,
    release: Option<Learned>,
}

#[cfg(unix)]
impl Record {
    fn get(&self, slot: Slot) -> Option<&Learned> {
        match slot {
            Slot::Dev => self.dev.as_ref(),
            Slot::Release => self.release.as_ref(),
        }
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut Option<Learned> {
        match slot {
            Slot::Dev => &mut self.dev,
            Slot::Release => &mut self.release,
        }
    }

    /// The record with every row that is not a fingerprint and an owner name dropped.
    fn checked(mut self) -> Record {
        for slot in Slot::ALL {
            let row = self.slot_mut(slot);
            if row.as_ref().is_some_and(|learned| {
                !config::is_fingerprint(&learned.fingerprint)
                    || !config::is_owner_name(&learned.login)
            }) {
                *row = None;
            }
        }
        self
    }

    /// The record's text: each row a fingerprint and an owner name, so none needs escaping.
    fn text(&self) -> String {
        let mut text = String::from(
            "# agh's vault adoption record (design §6.10): the login GET /user answered for each\n\
             # vault token, by the token's fingerprint. agh writes it.\n",
        );
        for slot in Slot::ALL {
            if let Some(learned) = self.get(slot) {
                text.push_str(&format!(
                    "\n[{}]\nfingerprint = \"{}\"\nlogin = \"{}\"\n",
                    slot.profile(),
                    learned.fingerprint,
                    learned.login
                ));
            }
        }
        text
    }
}

/// Why the record cannot be read.
#[cfg(unix)]
#[derive(Debug)]
struct RecordFault {
    /// The refusal.
    error: Error,
    /// What `agh policy check` reports.
    fault: ConfigFault,
    /// Whether the file is a private file of this user's that is not agh's record (it does not
    /// parse, or holds a row that is not a fingerprint and an owner name), as against one
    /// `read_private` refuses: adoption then routes neither token, and never rewrites it.
    damaged: bool,
}

/// The record: empty when there is none. A private file that is not agh's record (it does not
/// parse, or a row is not a fingerprint and an owner name) is `damaged`, since the guard
/// against a changed login rests on its rows; one `read_private` refuses is refused.
#[cfg(unix)]
fn read_record(dirs: &Dirs) -> std::result::Result<Record, RecordFault> {
    use crate::identity::{Held, PrivateFix, private_fix};
    use aterm_uds::private_file::{PrivateFileError, read_private};
    match read_private(&dirs.config, RECORD_FILE, RECORD_MAX) {
        Ok(bytes) => {
            let parsed = String::from_utf8(bytes)
                .ok()
                .and_then(|text| aterm_toml::from_str::<Record>(&text).ok());
            match parsed {
                Some(record) if record.clone().checked() == record => Ok(record),
                _ => {
                    let shown = tilde(&dirs.config.join(RECORD_FILE), &dirs.home);
                    let why = format!(
                        "{shown} is not agh's adoption record (it does not parse, or a row is \
                         not a fingerprint and a login), so agh cannot tell a vault token's \
                         changed login and routes neither"
                    );
                    let next = format!("remove {shown}, and agh learns each login again");
                    Err(RecordFault {
                        fault: ConfigFault {
                            rule: "file",
                            what: format!("{why}; remedy: {next}"),
                        },
                        error: config::invalid(why, next),
                        damaged: true,
                    })
                }
            }
        }
        Err(PrivateFileError::Missing) => Ok(Record::default()),
        Err(error) => {
            let file = dirs.config.join(RECORD_FILE);
            let shown = tilde(&file, &dirs.home);
            let remedy = match &error {
                PrivateFileError::NotPrivate(_) => {
                    match private_fix(&dirs.config, &file, &dirs.home, Held::Config(None)) {
                        PrivateFix::Fix(fix) => Some(fix),
                        PrivateFix::NotAFile | PrivateFix::NoChmod | PrivateFix::Unseen => None,
                    }
                }
                _ => None,
            };
            let remedy = remedy
                .unwrap_or_else(|| format!("make {shown} a private file of yours, or remove it"));
            let why = format!("{shown}: {error}");
            let fault = ConfigFault {
                rule: "file",
                what: format!("{why}; remedy: {remedy}"),
            };
            Err(RecordFault {
                error: config::invalid(why, remedy),
                fault,
                damaged: false,
            })
        }
    }
}

/// `record` written whole and atomically, 0600, in the private agh directory.
#[cfg(unix)]
fn write_record(dirs: &Dirs, record: &Record) -> std::io::Result<()> {
    if record.dev.is_none() && record.release.is_none() {
        return aterm_uds::private_file::remove_private(&dirs.config, RECORD_FILE).map(|_| ());
    }
    aterm_uds::private_file::write_private(&dirs.config, RECORD_FILE, record.text().as_bytes())
}

/// Adoption's own lock, under which the record is read, merged and written.
/// # Errors
/// Refused while another agh call writes the record, or when the lock is not private;
/// could_not_run on I/O.
#[cfg(unix)]
fn record_lock(dirs: &Dirs) -> Result<super::provision::AuthEdit> {
    super::provision::AuthEdit::take_named(dirs, RECORD_LOCK, LOCK_NAME, lock_busy)
}

/// What the record's lock is called in a refusal.
#[cfg(unix)]
const LOCK_NAME: &str = "the vault record lock";

/// The refusal while another agh call holds the record's lock.
#[cfg(unix)]
fn lock_busy() -> Error {
    Error::refused("another agh call is writing the vault adoption record")
        .with_next("run the command again: that write takes milliseconds")
}

/// What was learned, recorded under adoption's own lock: each row is written only while the
/// record still holds what `find` judged it against, or the same login, so two calls that
/// learned two logins for one slot never move it from one owner to the other unasked. Nothing
/// is written when the lock is held by another call, adoption no longer applies, or the record
/// cannot be read or written: the next call learns it again. Returns each slot whose record
/// moved to another login meanwhile, with that login (the call refuses it as
/// `Found::Changed`), and why nothing could be recorded, unless only another call's lock
/// stood in the way, which the next call outlasts.
#[cfg(unix)]
fn record_learned(dirs: &Dirs, learned: &[Learn]) -> (Vec<(Slot, String)>, Option<Error>) {
    let mut moved = Vec::new();
    let busy = std::cell::Cell::new(false);
    let lock = super::provision::AuthEdit::take_named(dirs, RECORD_LOCK, LOCK_NAME, || {
        busy.set(true);
        lock_busy()
    });
    let _lock = match lock {
        Ok(lock) => lock,
        Err(_) if busy.get() => return (moved, None),
        Err(error) => return (moved, Some(error)),
    };
    if base(dirs).is_none() {
        return (moved, None);
    }
    let mut record = match read_record(dirs) {
        Ok(record) => record,
        Err(fault) => return (moved, Some(fault.error)),
    };
    for learn in learned {
        let now = record.slot_mut(learn.slot);
        let same_login = now
            .as_ref()
            .is_some_and(|now| now.login.eq_ignore_ascii_case(&learn.row.login));
        if *now == learn.recorded || same_login {
            *now = Some(learn.row.clone());
        } else if let Some(other) = now.as_ref() {
            moved.push((learn.slot, other.login.clone()));
        }
    }
    let written = write_record(dirs, &record).err().map(|e| {
        let shown = tilde(&dirs.config, &dirs.home);
        Error::could_not_run(format!(
            "{}: {e}",
            tilde(&dirs.config.join(RECORD_FILE), &dirs.home)
        ))
        .with_next(format!(
            "make {shown} a private directory of yours that agh can write (chmod 700 {shown})"
        ))
    });
    (moved, written)
}

/// What would stop the record being written, judged without writing anything (`view`): the
/// agh directory open to others or not a private directory of this user's, or the lock file
/// occupied. `None` when nothing would, the directory missing included (agh makes it 0700).
#[cfg(unix)]
fn record_blocked(dirs: &Dirs) -> Option<Error> {
    use aterm_uds::private_file::{PrivateDir, judge_private_dir};
    let shown = tilde(&dirs.config, &dirs.home);
    match judge_private_dir(&dirs.config) {
        Ok(PrivateDir::Missing) => return None,
        Ok(PrivateDir::Private) => {}
        Ok(PrivateDir::Open(_)) => {
            let error = Error::refused(format!("{shown}: it is open to other users (want 0700)"));
            return Some(
                match super::chmod_remedy(&dirs.config, Path::new(""), &dirs.home) {
                    Some(remedy) => error.with_next(remedy),
                    None => error,
                },
            );
        }
        Err(e) => {
            let text = e
                .to_string()
                .replace(&dirs.config.display().to_string(), &shown);
            return Some(Error::refused(text).with_next(format!(
                "make {shown} a private directory of yours, or remove it"
            )));
        }
    }
    let lock = dirs.config.join(RECORD_LOCK);
    std::fs::symlink_metadata(&lock)
        .is_ok_and(|meta| meta.len() != 0)
        .then(|| {
            let shown = tilde(&lock, &dirs.home);
            Error::refused(format!(
                "{LOCK_NAME} {shown} is occupied by a nonempty file"
            ))
            .with_next(format!(
                "agh writes nothing into {shown}: remove it, and agh makes it again"
            ))
        })
}

/// `agh auth remove NAME` with no agh.toml (or one that says `vault = true`): the record
/// forgets the login adopted for the vault token NAME names, so the next call learns it again.
/// `false` when it held none.
/// # Errors
/// The record's refusal; refused while another agh call writes the record; could_not_run when
/// it cannot be written.
#[cfg(unix)]
pub(crate) fn forget_locked(dirs: &Dirs, slot: Slot) -> Result<bool> {
    let _lock = record_lock(dirs)?;
    let mut record = read_record(dirs).map_err(|fault| fault.error)?;
    if record.slot_mut(slot).take().is_none() {
        return Ok(false);
    }
    write_record(dirs, &record).map_err(|e| {
        Error::could_not_run(format!(
            "{}: {e}",
            tilde(&dirs.config.join(RECORD_FILE), &dirs.home)
        ))
    })?;
    Ok(true)
}

/// The record a test starts from: `(slot, fingerprint, login)` rows written as agh writes them.
/// # Errors
/// The write's.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn record_for_tests(dirs: &Dirs, rows: &[(Slot, &str, &str)]) -> std::io::Result<()> {
    if let Some(parent) = dirs.config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    aterm_uds::private_file::create_private_dir(&dirs.config)?;
    let mut record = Record::default();
    for (slot, fingerprint, login) in rows {
        *record.slot_mut(*slot) = Some(Learned {
            fingerprint: (*fingerprint).to_string(),
            login: (*login).to_string(),
        });
    }
    write_record(dirs, &record)
}

/// The record's lock, held until the guard drops, as another agh call holds it while it writes
/// the record.
/// # Errors
/// `record_lock`'s.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn hold_record_lock_for_tests(dirs: &Dirs) -> Result<impl Sized> {
    if let Some(parent) = dirs.config.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::could_not_run(e.to_string()))?;
    }
    record_lock(dirs)
}

/// The record's text, the seed agh.toml, and the rows adoption derives, against the rules they
/// are read by.
#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::{Adoption, Found, Learned, Record, Slot, Vaulted};

    fn adopted(home: &Path, slot: Slot, login: &str, fingerprint: &str) -> Vaulted {
        Vaulted {
            slot,
            token_file: slot.path(home),
            found: Found::Adopted {
                login: login.to_string(),
                fingerprint: fingerprint.to_string(),
            },
        }
    }

    /// The vault names the release token by the public owner it writes for.
    #[test]
    fn the_release_file_is_named_for_the_public_owner() {
        assert!(
            super::RELEASE_TOKEN_FILE
                .ends_with(&format!("_{}", aterm_types::channel::PUBLIC_OWNER))
        );
        assert_eq!(Slot::named("dev"), Some(Slot::Dev));
        assert_eq!(Slot::named("release"), Some(Slot::Release));
        assert_eq!(Slot::named("ext"), None);
    }

    /// A record agh writes reads back whole; `checked` drops a row that is not a fingerprint
    /// and an owner name, and `read_record` judges a file that loses a row so, or that does not
    /// parse, damaged, never an empty record.
    #[test]
    fn the_record_reads_back_and_a_row_it_cannot_trust_damages_it() {
        let record = Record {
            dev: Some(Learned {
                fingerprint: "0123456789ab".to_string(),
                login: "fixture-dev".to_string(),
            }),
            release: Some(Learned {
                fingerprint: "ba9876543210".to_string(),
                login: "fixture-release-bot".to_string(),
            }),
        };
        let read: Record = aterm_toml::from_str(&record.text()).expect("the record parses");
        assert_eq!(read.checked(), record);
        let forged = "[dev]\nfingerprint = \"0123456789ab\"\nlogin = \"a/b\"\n\
                      [release]\nfingerprint = \"XYZ\"\nlogin = \"fixture-release-bot\"\n";
        let read: Record = aterm_toml::from_str(forged).expect("the rows parse");
        assert_eq!(read.checked(), Record::default());

        let root = aterm_tempfile::Builder::new()
            .prefix("agh-vault-damaged-")
            .tempdir()
            .expect("a scratch root");
        let dirs = crate::config::Dirs::under(root.path());
        super::record_for_tests(&dirs, &[]).expect("the agh directory");
        for text in [forged, "[dev\n", "[later]\nfield = 1\n"] {
            aterm_uds::private_file::write_private(
                &dirs.config,
                super::RECORD_FILE,
                text.as_bytes(),
            )
            .expect("the record");
            let fault = super::read_record(&dirs).expect_err("damaged");
            assert!(
                fault.damaged && fault.fault.rule == "file",
                "{text}: {fault:?}"
            );
        }
    }

    /// The adopted rows route as the canonical agh.toml does (design §6.2): the dev token
    /// first for every owner's plain reads, the public owner's included, each token the only
    /// writer of its own owner.
    #[test]
    fn the_adopted_rows_route_as_the_canonical_config() {
        let home = Path::new("/nonexistent-home");
        let adoption = Adoption::judge(
            vec![
                adopted(home, Slot::Dev, "fixture-dev", "0123456789ab"),
                adopted(home, Slot::Release, "fixture-release-bot", "ba9876543210"),
            ],
            home,
            None,
        );
        let router = adoption.router().expect("the adopted rows validate");
        use crate::policy::ChainStep;
        for owner in ["fixture-ext", "alabsystems"] {
            assert_eq!(
                router.read_chain(Some(owner)),
                [ChainStep::Profile("dev".to_string()), ChainStep::Anonymous],
                "{owner}"
            );
        }
        assert_eq!(
            router.read_chain(Some("fixture-dev")),
            [ChainStep::Profile("dev".to_string())]
        );
        assert_eq!(router.writer_of("fixture-dev"), Some("dev"));
        assert_eq!(router.writer_of("alabsystems"), Some("release"));
        assert_eq!(router.writer_of("fixture-ext"), None);
    }

    /// The seed is `vault = true` alone, which parses and validates; it is `None` with no vault
    /// file, for a profile named `dev` or `release`, and for a vault file as `--file`.
    #[test]
    fn the_seed_joins_the_vault_and_names_no_row() {
        let root = aterm_tempfile::Builder::new()
            .prefix("agh-vault-seed-")
            .tempdir()
            .expect("a scratch root");
        let dirs = crate::config::Dirs::under(root.path());
        assert_eq!(super::seed(&dirs, "ext", None), None);
        let dev = Slot::Dev.path(&dirs.home);
        std::fs::create_dir_all(dev.parent().expect("the vault directory")).expect("the home");
        std::fs::write(&dev, "x").expect("a vault file");
        for name in ["dev", "release"] {
            assert_eq!(super::seed(&dirs, name, None), None, "{name} routes alone");
        }
        assert_eq!(
            super::seed(&dirs, "ext", Some(&dev)),
            None,
            "a vault file named as another profile's routes alone"
        );
        let seed = super::seed(&dirs, "ext", None).expect("a seed");
        let parsed = crate::config::parse(&seed).expect("the seed parses");
        assert!(parsed.vault);
        assert!(parsed.profiles.is_empty() && parsed.owners.is_empty());
        let validated =
            crate::config::validate(&parsed, &dirs.home, &|_| None).expect("the seed validates");
        assert!(validated.joins_vault());
    }

    /// An agh.toml that says `vault = true` leaves profiles `dev` and `release` to the vault:
    /// L10 refuses one it names, and the same rows without the key pass.
    #[test]
    fn a_vault_joining_agh_toml_names_no_vault_profile() {
        let home = Path::new("/nonexistent-home");
        let rows = "[profiles.dev]\ntoken_file = \"~/dev.token\"\ntier = \"dev\"\n";
        let joined = crate::config::parse(&format!("vault = true\n{rows}")).expect("parses");
        let faults = crate::config::validate(&joined, home, &|_| None).expect_err("L10");
        assert!(faults.iter().any(|f| f.rule == "L10"), "{faults:?}");
        let alone = crate::config::parse(rows).expect("parses");
        assert!(crate::config::validate(&alone, home, &|_| None).is_ok());
    }

    /// A dev token of the public owner breaks the floor: it is left out, reported, and the
    /// release token still routes.
    #[test]
    fn a_dev_token_of_the_public_owner_is_left_out() {
        let home = Path::new("/nonexistent-home");
        let adoption = Adoption::judge(
            vec![
                adopted(
                    home,
                    Slot::Dev,
                    aterm_types::channel::PUBLIC_OWNER,
                    "0123456789ab",
                ),
                adopted(home, Slot::Release, "fixture-release-bot", "ba9876543210"),
            ],
            home,
            None,
        );
        let router = adoption.router().expect("the release rows still route");
        assert!(adoption.faults().iter().any(|f| f.rule == "L2"));
        assert!(matches!(adoption.vaulted()[0].found, Found::Faulted { .. }));
        assert!(adoption.adopted("release") && !adoption.adopted("dev"));
        assert_eq!(router.writer_of("alabsystems"), Some("release"));
        assert!(router.profile("dev").is_none());
        let lines = adoption.note(home);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("agh: note vault=dev why="),
            "{lines:?}"
        );
    }

    /// Two vault tokens GitHub answers one login for: neither's tier is known, so neither
    /// routes, each is left out with the vault's step, and the L7 fault is reported once.
    #[test]
    fn two_tokens_of_one_login_route_neither() {
        let home = Path::new("/nonexistent-home");
        let adoption = Adoption::judge(
            vec![
                adopted(home, Slot::Dev, "fixture-dev", "0123456789ab"),
                adopted(home, Slot::Release, "Fixture-Dev", "ba9876543210"),
            ],
            home,
            None,
        );
        let router = adoption.router().expect("no refusal");
        assert!(!adoption.adopted_any());
        assert!(router.profile("dev").is_none() && router.profile("release").is_none());
        let rules: Vec<&str> = adoption.faults().iter().map(|f| f.rule).collect();
        assert_eq!(rules, ["L7"]);
        for vaulted in adoption.vaulted() {
            assert!(
                matches!(&vaulted.found, Found::Faulted { next, .. } if *next == super::pair_next()),
                "{vaulted:?}"
            );
        }
        let lines = adoption.note(home);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("agh: note vault=dev,release why="),
            "{lines:?}"
        );
        let mut quoted = String::new();
        crate::cli::out::receipt_value(&mut quoted, &super::pair_next());
        assert!(lines[0].ends_with(&format!(" next={quoted}")), "{lines:?}");
    }

    /// A login learned for a slot whose record another call filled meanwhile with another login
    /// is not written: the slot is reported moved, and the call refuses it. The same login (in
    /// any case) is written.
    #[test]
    fn a_learned_login_never_overwrites_another_recorded_meanwhile() {
        let root = aterm_tempfile::Builder::new()
            .prefix("agh-vault-race-")
            .tempdir()
            .expect("a scratch root");
        let dirs = crate::config::Dirs::under(root.path());
        super::record_for_tests(&dirs, &[(Slot::Dev, "0123456789ab", "fixture-x")])
            .expect("the record");
        let learn = |login: &str| super::Learn {
            slot: Slot::Dev,
            kind: crate::identity::TokenKind::Classic,
            recorded: None,
            row: Learned {
                fingerprint: "ba9876543210".to_string(),
                login: login.to_string(),
            },
        };
        let (moved, refused) = super::record_learned(&dirs, &[learn("fixture-y")]);
        assert_eq!(moved, vec![(Slot::Dev, "fixture-x".to_string())]);
        assert!(refused.is_none(), "{refused:?}");
        let record = super::read_record(&dirs).expect("the record reads");
        assert_eq!(
            record.dev.as_ref().map(|r| r.fingerprint.as_str()),
            Some("0123456789ab")
        );
        let (moved, refused) = super::record_learned(&dirs, &[learn("Fixture-X")]);
        assert!(moved.is_empty() && refused.is_none());
        let record = super::read_record(&dirs).expect("the record reads");
        assert_eq!(
            record.dev.as_ref().map(|r| r.fingerprint.as_str()),
            Some("ba9876543210")
        );
    }

    /// The subsets tried, largest first, dev before release.
    #[test]
    fn the_subsets_keep_the_most_tokens_dev_first() {
        assert_eq!(
            super::subsets(&[Slot::Dev, Slot::Release]),
            vec![
                vec![Slot::Dev, Slot::Release],
                vec![Slot::Dev],
                vec![Slot::Release],
                vec![],
            ]
        );
        assert_eq!(super::subsets(&[]), vec![Vec::<Slot>::new()]);
    }
}
