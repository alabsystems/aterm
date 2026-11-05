// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One name per machine: the grammar of a machine's number (`m8`), its nickname
//! (`Heron`) and its aliases, the default nickname, and the OS names derived from the
//! number and the hardware (`m8-macbook-pro-m5-128gb-8tb`).
//!
//! Portable and pure: nothing here reads the machine, the network or the clock, except
//! [`PlatformId::Host`], the one port through which the host's platform id, boot id and
//! container markers are read. `docs/DESIGN-machine-names-2026-11-04.md` §2, §4 and §6 are the
//! specification.

use std::fmt;

mod check;
mod choose;
mod long;
pub mod nouns;
mod platform;
mod settle;
mod sizes;

pub use check::{Verdict, WAS_MAX, check, renamed, taken};
pub use choose::{Blocked, Choice, Fleet, Kept, Labels, choose, top};
pub use long::{
    DNS_LABEL_MAX, Disk, LONG_CHIP_TIER, MODEL_WORDS, MachineFacts, NameOs, OsSlot, VM_WORD,
    WINDOWS_DESCRIPTION_MAX, chip_word, is_virtual, long_name, long_name_within, model_word,
    os_slots,
};
pub use platform::{
    BOOT_ID_SEAM, CONTAINER_MARKERS, PLATFORM_ID_SEAM, PlatformId, in_container_by, is_ephemeral,
};
pub use settle::{
    Anchor, Cap, Claim, Class, Entry, Holder, IMPLIED_SINCE, NodeId, Record, Settled, Standing,
    Was, next_stamp, settle,
};
pub use sizes::{disk_size, label_form, label_form_keeping, memory_size};

/// The most digits a [`Number`] has: `m999999`, which fits NetBIOS's 15 bytes and the
/// macOS name rule's `m[1-9][0-9]?…` pattern.
pub const NUMBER_MAX_DIGITS: usize = 6;

/// What a machine never numbered shows in place of its number. Display only: never a
/// record value, alias, OS name or table label, and `--host m0` names no machine.
pub const UNNUMBERED: &str = "m0";

/// Words no nickname or alias may be, because `--host` and the fleet book give them a
/// meaning of their own.
pub const RESERVED_WORDS: [&str; 3] = ["localhost", "self", "all"];

/// Why a word is not a [`Number`], [`Nickname`] or [`Alias`]: the verdict a rename
/// sheet or `aterm name` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// Nothing was given.
    Empty,
    /// Longer than the kind allows.
    TooLong,
    /// `m0`, which means "not numbered yet" and names no machine.
    Unnumbered,
    /// A number spelled with a leading zero (`m08`).
    LeadingZero,
    /// A number of more than [`NUMBER_MAX_DIGITS`] digits.
    TooManyDigits,
    /// Not `m` and digits.
    NotANumber,
    /// A character the kind does not allow, or a space where it may not stand.
    Characters,
    /// A nickname must start with a letter.
    StartsWithoutLetter,
    /// Shaped like a machine number (`m12`, `m8.2`), which only a NAME may be.
    NumberShaped,
    /// An account preference word `m<N>.<k>`, never a machine's alias.
    PreferenceWord,
    /// `node-…` or `n-<hex>`: a node's own address, which nothing may shadow.
    NodeWord,
    /// Eight or more hex digits alone, which reads as a node id.
    HexRun,
    /// One of [`RESERVED_WORDS`].
    Reserved,
    /// This machine's own NAME, which is no alias of it.
    OwnName,
    /// Another live machine's current NAME.
    LiveName(Number),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("nothing was given"),
            Self::TooLong => f.write_str("it is too long"),
            Self::Unnumbered => f.write_str("m0 names no machine"),
            Self::LeadingZero => f.write_str("a number has no leading zero"),
            Self::TooManyDigits => write!(f, "a number has at most {NUMBER_MAX_DIGITS} digits"),
            Self::NotANumber => f.write_str("a machine number is m and digits, such as m8"),
            Self::Characters => f.write_str("it holds a character or space it may not"),
            Self::StartsWithoutLetter => f.write_str("a nickname starts with a letter"),
            Self::NumberShaped => f.write_str("only a machine's number may look like one"),
            Self::PreferenceWord => f.write_str("m<N>.<k> is an account preference word"),
            Self::NodeWord => f.write_str("node-… and n-<hex> are node addresses"),
            Self::HexRun => f.write_str("eight or more hex digits read as a node id"),
            Self::Reserved => f.write_str("localhost, self and all are reserved"),
            Self::OwnName => f.write_str("it is this machine's own name"),
            Self::LiveName(n) => write!(f, "{n} is another machine's name"),
        }
    }
}

/// A machine's number, `m1` to `m999999`: its NAME, unique in a fleet once records meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Number(u32);

impl Number {
    /// The greatest number: [`NUMBER_MAX_DIGITS`] nines.
    pub const MAX: u32 = 999_999;

    /// `n` as a number, when it is 1 to [`Number::MAX`].
    #[must_use]
    pub const fn new(n: u32) -> Option<Self> {
        if n >= 1 && n <= Self::MAX {
            Some(Self(n))
        } else {
            None
        }
    }

    /// The number's value (`8` for `m8`).
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The number a person typed: `8`, `m8` and `M8` all read as `m8`, outer ASCII
    /// whitespace aside.
    ///
    /// # Errors
    /// The [`Refusal`] naming what is wrong: `m0` ([`Refusal::Unnumbered`]), `m08`,
    /// `m1234567`, or anything that is not `m` and digits.
    pub fn parse(word: &str) -> Result<Self, Refusal> {
        let word = word.trim_ascii();
        if word.is_empty() {
            return Err(Refusal::Empty);
        }
        let digits = word.strip_prefix(['m', 'M']).unwrap_or(word).as_bytes();
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return Err(Refusal::NotANumber);
        }
        Self::from_digits(digits)
    }

    /// The number a stored word starts with: `m` or `M`, then up to six digits with no
    /// leading zero, ended by the word's end or by a character that is not an ASCII
    /// letter or digit, after outer ASCII whitespace.
    ///
    /// `m8-macbook-pro-m5-…`, `M22.local`, macOS's conflict form `m4 (2)` and the
    /// starter's `m10 MacBook Pro …` give m8, m22, m4 and m10; `m3pro`, `Mac-mini-M4`,
    /// `10.0.1.20`, `m08-x` and a typed `8` give nothing.
    #[must_use]
    pub fn leading(word: &str) -> Option<Self> {
        let (digits, _) = leading_run(word.trim_ascii())?;
        Self::from_digits(digits).ok()
    }

    fn from_digits(digits: &[u8]) -> Result<Self, Refusal> {
        match digits {
            [] => Err(Refusal::NotANumber),
            [b'0'] => Err(Refusal::Unnumbered),
            [b'0', ..] => Err(Refusal::LeadingZero),
            _ if digits.len() > NUMBER_MAX_DIGITS => Err(Refusal::TooManyDigits),
            _ => {
                let n = digits
                    .iter()
                    .fold(0_u32, |n, d| n * 10 + u32::from(d - b'0'));
                Ok(Self(n))
            }
        }
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "m{}", self.0)
    }
}

impl std::str::FromStr for Number {
    type Err = Refusal;

    fn from_str(word: &str) -> Result<Self, Refusal> {
        Self::parse(word)
    }
}

/// The digits after a leading `m`/`M` and whatever follows them, when the run is
/// bounded by the end or by a character that is not an ASCII letter or digit.
fn leading_run(word: &str) -> Option<(&[u8], &[u8])> {
    let rest = match word.as_bytes() {
        [b'm' | b'M', rest @ ..] => rest,
        _ => return None,
    };
    let n = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let (digits, after) = rest.split_at(n);
    let bounded = after.first().is_none_or(|b| !b.is_ascii_alphanumeric());
    (n > 0 && bounded).then_some((digits, after))
}

/// Whether `word` is an account preference word `m<N>.<k>` (`m2.2`: "m2 prefers this
/// account, rank 2"), which is never a machine's name or alias.
#[must_use]
pub fn is_preference_word(word: &str) -> bool {
    let Some((machine, rank)) = word.split_once('.') else {
        return false;
    };
    Number::parse(machine).is_ok()
        && machine.starts_with(['m', 'M'])
        && !rank.is_empty()
        && rank.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `word` (any case) is a node's own address: `node-…`, or `n-` followed only by
/// hex digits (a mesh principal).
#[must_use]
pub fn is_node_word(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.starts_with("node-")
        || lower
            .strip_prefix("n-")
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_hex_run(word: &str) -> bool {
    word.len() >= 8 && word.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_reserved(word: &str) -> bool {
    RESERVED_WORDS.iter().any(|r| r.eq_ignore_ascii_case(word))
}

/// A machine's nickname: 1 to 24 characters and at most 64 bytes, starting with a
/// letter, of letters, digits, single inner spaces, `-` and `'`. Not unique: it
/// resolves under `--host` only while no other machine shares it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Nickname(String);

impl Nickname {
    /// The most characters a nickname has.
    pub const MAX_CHARS: usize = 24;
    /// The most bytes a nickname has.
    pub const MAX_BYTES: usize = 64;

    /// `word` as a nickname, stored as typed.
    ///
    /// # Errors
    /// The [`Refusal`]: a number shape (`m12`, `m8.2`, `m0`), a node word, eight or more
    /// hex digits alone, a reserved word, or a character or length outside the grammar.
    pub fn parse(word: &str) -> Result<Self, Refusal> {
        let mut chars = word.chars();
        let Some(first) = chars.next() else {
            return Err(Refusal::Empty);
        };
        if word.chars().count() > Self::MAX_CHARS || word.len() > Self::MAX_BYTES {
            return Err(Refusal::TooLong);
        }
        if word.eq_ignore_ascii_case(UNNUMBERED) {
            return Err(Refusal::Unnumbered);
        }
        if Number::parse(word).is_ok() || leading_run(word).is_some() || is_preference_word(word) {
            return Err(Refusal::NumberShaped);
        }
        if !first.is_alphabetic() {
            return Err(Refusal::StartsWithoutLetter);
        }
        let allowed = |c: char| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'');
        if !word.chars().all(allowed) || word.contains("  ") || word.ends_with(' ') {
            return Err(Refusal::Characters);
        }
        if is_node_word(word) {
            return Err(Refusal::NodeWord);
        }
        if is_hex_run(word) {
            return Err(Refusal::HexRun);
        }
        if is_reserved(word) {
            return Err(Refusal::Reserved);
        }
        Ok(Self(word.to_owned()))
    }

    /// The nickname as it was typed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The nickname as the menu bar shows it: an all-lowercase ASCII one (a drawn noun,
    /// `heron`) with its first letter capitalised, any other as typed.
    #[must_use]
    pub fn display(&self) -> String {
        let lower_ascii = self.0.is_ascii() && !self.0.bytes().any(|b| b.is_ascii_uppercase());
        if !lower_ascii {
            return self.0.clone();
        }
        let mut shown = self.0.clone();
        if let Some(first) = shown.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        shown
    }
}

impl fmt::Display for Nickname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// One of a machine's other names in the fleet's table: a DNS label (lowercase ASCII
/// letters, digits and inner hyphens, 1 to 63 bytes) that no two live machines share.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Alias(String);

impl Alias {
    /// `word` as an alias: ASCII case folded, outer ASCII whitespace and a `.local`
    /// suffix dropped (`Studio-2.local` is `studio-2`).
    ///
    /// # Errors
    /// The [`Refusal`]: a preference word (`m2.2`), `m0` or another zero-led or
    /// seven-digit number shape, a node word, eight or more hex digits alone, a reserved
    /// word, or anything that is not a DNS label.
    pub fn parse(word: &str) -> Result<Self, Refusal> {
        let mut word = word.trim_ascii().to_ascii_lowercase();
        if let Some(stem) = word.strip_suffix(".local") {
            word = stem.to_owned();
        }
        if word.is_empty() {
            return Err(Refusal::Empty);
        }
        if is_preference_word(&word) {
            return Err(Refusal::PreferenceWord);
        }
        if word.len() > DNS_LABEL_MAX {
            return Err(Refusal::TooLong);
        }
        let label = word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !word.starts_with('-')
            && !word.ends_with('-');
        if !label {
            return Err(Refusal::Characters);
        }
        if word.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Refusal::NumberShaped);
        }
        if let Some((digits, _)) = leading_run(&word)
            && let Err(refusal) = Number::from_digits(digits)
        {
            return Err(refusal);
        }
        if is_node_word(&word) {
            return Err(Refusal::NodeWord);
        }
        if is_hex_run(&word) {
            return Err(Refusal::HexRun);
        }
        if is_reserved(&word) {
            return Err(Refusal::Reserved);
        }
        Ok(Self(word))
    }

    /// The alias, lowercase.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this alias may name a machine whose NAME is `own`, beside machines now
    /// named `live`: it is neither that NAME nor another live machine's.
    ///
    /// # Errors
    /// [`Refusal::OwnName`] or [`Refusal::LiveName`].
    pub fn check(&self, own: Option<Number>, live: &[Number]) -> Result<(), Refusal> {
        let Ok(number) = Number::parse(&self.0) else {
            return Ok(());
        };
        if !self.0.starts_with('m') {
            return Ok(());
        }
        if own == Some(number) {
            return Err(Refusal::OwnName);
        }
        if live.contains(&number) {
            return Err(Refusal::LiveName(number));
        }
        Ok(())
    }
}

impl fmt::Display for Alias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The host of the machine-number models' waivers (`aterm_models_swarm`'s
/// `machine_number_*_model`): the actions no function here performs, each held as environment
/// by `model_tests`, which binds `settle`, `choose` and `check` to the rest.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberHolder",
        action = "Settle",
        reason = "check settles the view with the press's record inside its own windows, so settle here is held outside them: model_tests compares each Settle's holder with settle's (and settle is bound on MachineNumberEvidence, Group, Succession and Stripped)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberHolder",
        action = "Claim",
        reason = "Environment: an automatic claim made on a partitioned view; choose's pick is bound on MachineNumberChoose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberHolder",
        action = "Move",
        reason = "Environment: a displaced node's move off the number it lost to a number of its own, m(10 + i), that no other claim names; choose's pick for a displaced node is bound on MachineNumberChoose (Contest)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberEvidence",
        action = "Migrate1",
        reason = "Environment: an older build's migration (stage 2, section 7.1), not built yet; the no-reversal check it runs is settle's standing, bound on Settle"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberEvidence",
        action = "Migrate2",
        reason = "Environment: an older build's migration (stage 2, section 7.1), not built yet; the no-reversal check it runs is settle's standing, bound on Settle"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberEvidence",
        action = "Take",
        reason = "check carries MachineNumberHolder's presses and runs settle, this machine's Settle anchor, so the take is held outside the windows: model_tests compares check's verdict for node 3 in every settled state (HeldSeeded and HeldByOlderBuild included) with the Take guard, and the record renamed writes with the successor's"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberEvidence",
        action = "Refound",
        reason = "Environment: a sync's join of the vault's re-founded default fleet (stage 2), which keeps every claim's fields"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberGroup",
        action = "Migrate1",
        reason = "Environment: a node seeding its evidence NAME (stage 2, section 7.1)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberGroup",
        action = "Migrate2",
        reason = "Environment: a node seeding its evidence NAME (stage 2, section 7.1)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberGroup",
        action = "Resign1",
        reason = "Environment: a re-signing at a higher rev, which no record field settle reads carries"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberGroup",
        action = "Resign2",
        reason = "Environment: a re-signing at a higher rev, which no record field settle reads carries"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberSuccession",
        action = "Succeed",
        reason = "Environment: a node writing its machine's claim at sgen + 1 (stage 2's dormant-node read)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberSuccession",
        action = "Take",
        reason = "Environment: the successor's take, a press check holds on MachineNumberHolder's Take"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberSuccession",
        action = "Move",
        reason = "Environment: the successor's move to an automatic m20, where only the sgen it keeps matters; choose's pick for a node that needs a number is bound on MachineNumberChoose"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberSuccession",
        action = "TakeBack",
        reason = "Environment: the superseded node writing the successor's claim back at sgen + 1"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberSuccession",
        action = "StaleTake",
        reason = "Environment: the superseded node's take, on a view that lacked its successor, of the successor's claim at its own sgen (section 11.69); the press is check's, bound on MachineNumberHolder's Take"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "RelayStrips",
        reason = "Environment: a relay handing the reader an entry without its record or name"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "Fetch",
        reason = "Environment: the reader's first-hand read of the stripped node's book (reconcile, stage 2)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "Reclaim",
        reason = "Environment: the stripped node re-claiming its number at a later since"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "Roam",
        reason = "Environment: an address change of the older node on the reader's Mac"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "Down",
        reason = "Environment: the older node's listener stopping; it stays proven, and opaque, while it is live"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberStripped",
        action = "Upgrade",
        reason = "Environment: the older node upgrading or falling silent, after which no older entry is live on the reader's Mac"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Join",
        reason = "Environment: a machine holding m1 joining the view"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Arrive",
        reason = "Environment: a machine holding m5 joining the view"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Expire",
        reason = "Environment: that machine expiring into a tombstone (stage 2's fleet-retired)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Row",
        reason = "Environment: a planned computers row labelled m6"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Recall",
        reason = "Environment: the picker's history, a number it lost or one it kept as prev"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Contest",
        reason = "Environment: the picker's own record claiming m1 after the joiner's, so it is displaced once the joiner is in view"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Wait",
        reason = "Environment: another machine's migration that waits keeping a NAME as prev"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberChoose",
        action = "Rival",
        reason = "Environment: another machine naming the picker's want, by its admission label or an unknown claim"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberPress",
        action = "Settle",
        reason = "check settles the view with the press's record inside its own Rename window, so settle here is held outside it: model_tests compares each Settle's primary, standings and lost with settle's"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberPress",
        action = "Seed",
        reason = "Environment: the second node's migration seeding its evidence NAME (stage 2, section 7.1)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberPress",
        action = "Wait",
        reason = "Environment: the second node's migration that waits keeping its NAME as prev (stage 2, section 7.1)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "MachineNumberPress",
        action = "Drop",
        reason = "Environment: the second node publishing its next record with no NAME"
    )
)]
#[doc(hidden)]
pub fn machine_number_environment() {}

#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod settle_tests;
#[cfg(test)]
mod tests;
