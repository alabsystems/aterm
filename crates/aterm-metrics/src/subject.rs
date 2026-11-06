// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subjects of `metrics.log` (design §4.14, §4.6): where each Mac's own rows live.
//!
//! ```text
//! /m/n-<id>/local/raw/<metric>              raw samples                 stays on the Mac
//! /m/n-<id>/local/journal                   the account event log       stays; read on demand
//! /m/n-<id>/shared/1m/<metric>              one summary per minute      relayed to every Mac
//! /m/n-<id>/shared/1h/<metric>              one summary per hour        relayed
//! /m/n-<id>/shared/quota/<cli>/<account>    an account's current row    relayed
//! /m/n-<id>/shared/quota/agy/<home>         an agy home's current row   relayed
//! /m/n-<id>/shared/now/presence             presence (aterm-mesh, B7c)  relayed
//! /m/n-<id>/shared/now/needs                sessions that need a person relayed
//! /m/n-<id>/shared/now/unread               the pools it could not read relayed
//! /m/n-<id>/shared/now/place/<account>      where an account is placed  relayed
//! ```
//!
//! **The owner segment** is the Mac's node id, `n-` and the first 8 bytes of its node public
//! key in lowercase hex: astream's `identity_principal`, which also names the Mac in the
//! fabric, in presence and on the transport. Every subject has exactly one writer, the Mac it
//! names, and a relayed row is accepted only under its origin's grant (`relay:/m/{owner}/
//! shared/>`), so no row has two writers and no merge conflict can happen.
//!
//! **Nothing in a subject grows with history.** Metric names come from the compiled
//! [`Metric`], accounts are named by the registry's row key ([`AccountKey`], never an
//! address), agy homes by their launch key ([`HomeKey`]), and a window's start goes in the
//! record's body. A Mac uses at most [`Metric::ALL`]'s 30 summary subjects and one quota
//! subject per account it holds, far under astream's 4,096 subjects per producer.
//!
//! [`Subject::parse`] is the one reader and `Display` the one writer: every subject this
//! module accepts is the canonical spelling of exactly one [`Subject`] value.

use std::fmt;

use astream_wire::summary::{MetricName, Origin, WindowLen};

/// Every subject's first segment.
pub const ROOT: &str = "m";

/// Every row any Mac shares: what a view reads (`last_all`) and a relay forwards.
pub const SHARED_FILTER: &str = "/m/*/shared/>";

/// Why a string is not a metrics subject, or a word not one of its segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubjectError {
    /// Not `/m/<node>/…`.
    Root,
    /// The owner segment is not `n-` and 16 lowercase hex digits.
    Node,
    /// A class this grammar does not name (`local/…`, `shared/…`).
    Class,
    /// A metric name [`Metric::ALL`] does not hold.
    Metric,
    /// A summary subject whose metric is kept at another width.
    Width,
    /// A CLI word [`Cli::ALL`] does not hold. (An account key under `agy` is a
    /// [`SubjectError::Home`]: agy's quota subjects name a home.)
    Cli,
    /// An account key that is not `ac-` or `sa-` and 16 lowercase hex digits.
    Account,
    /// An agy home key that is neither `home` nor `h` and 16 lowercase hex digits.
    Home,
    /// A `now` name this grammar does not name.
    Now,
    /// Segments left over, or too few.
    Shape,
}

impl fmt::Display for SubjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SubjectError::Root => "not a metrics subject (/m/<node>/…)",
            SubjectError::Node => "the owner is not a node id (n-<16 hex>)",
            SubjectError::Class => "not a metrics class (local/raw, local/journal, shared/…)",
            SubjectError::Metric => "not a metric this build names",
            SubjectError::Width => "this metric is kept at another width",
            SubjectError::Cli => "not a CLI this build names",
            SubjectError::Account => "not an account key (ac-<16 hex> or sa-<16 hex>)",
            SubjectError::Home => "not an agy home key (home or h<16 hex>)",
            SubjectError::Now => "not a now row this build names",
            SubjectError::Shape => "the subject has too many or too few segments",
        })
    }
}

impl std::error::Error for SubjectError {}

/// Format `bytes` as lowercase hex into `out` (`out.len()` must be `2 * bytes.len()`).
pub(crate) const fn format_hex_lower<const N: usize, const M: usize>(
    prefix: &[u8],
    bytes: &[u8; N],
) -> [u8; M] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; M];
    let mut i = 0;
    while i < prefix.len() {
        out[i] = prefix[i];
        i += 1;
    }
    let mut j = 0;
    while j < N {
        let b = bytes[j];
        out[prefix.len() + 2 * j] = DIGITS[(b >> 4) as usize];
        out[prefix.len() + 2 * j + 1] = DIGITS[(b & 0x0f) as usize];
        j += 1;
    }
    out
}

/// Exactly `N` bytes from `2N` lowercase hex digits; uppercase is refused, so each value has
/// one spelling.
pub(crate) fn parse_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if pairs.len() != N || !rest.is_empty() {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (slot, &[hi, lo]) in out.iter_mut().zip(pairs) {
        *slot = (nibble(hi)? << 4) | nibble(lo)?;
    }
    Some(out)
}

/// A Mac's node id on the mesh: `n-` and the first 8 bytes of its node public key in
/// lowercase hex (astream's `identity_principal`; aterm-net's `node_principal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node([u8; 8]);

impl Node {
    /// The node id of the Ed25519 public key `key`.
    #[must_use]
    pub fn of_key(key: &[u8; 32]) -> Node {
        let mut id = [0u8; 8];
        id.copy_from_slice(&key[..8]);
        Node(id)
    }

    /// The node id `n-<16 lowercase hex>`; anything else is `None`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Node> {
        parse_hex::<8>(word.strip_prefix("n-")?).map(Node)
    }

    /// The id's 8 bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 8] {
        self.0
    }

    /// The canonical 18-byte `n-<16 lowercase hex>` representation.
    #[must_use]
    pub(crate) const fn to_ascii_bytes(self) -> [u8; 18] {
        format_hex_lower::<8, 18>(b"n-", &self.0)
    }

    /// The same name as an astream summary origin.
    #[must_use]
    pub fn origin(self) -> Origin {
        let buf = self.to_ascii_bytes();
        let s = std::str::from_utf8(&buf).unwrap_or_default();
        Origin::new(s).unwrap_or_else(|e| unreachable!("a node id is an origin name: {e}"))
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let buf = self.to_ascii_bytes();
        f.write_str(std::str::from_utf8(&buf).unwrap_or_default())
    }
}

/// The five CLIs whose accounts the harness keeps (`aterm-agent` `ai::connector::Cli::ALL`),
/// spelled as their connector ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cli {
    /// Claude Code.
    Claude,
    /// Codex CLI and codex-alab.
    Codex,
    /// Grok.
    Grok,
    /// Kimi.
    Kimi,
    /// agy (Antigravity): its quota is per home, not per account ([`Subject::Agy`]).
    Agy,
}

impl Cli {
    /// Every CLI, in the connector's order.
    pub const ALL: [Cli; 5] = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi, Cli::Agy];

    /// The connector id.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
            Cli::Grok => "grok",
            Cli::Kimi => "kimi",
            Cli::Agy => "agy",
        }
    }

    /// The CLI whose connector id is `word`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Cli> {
        Cli::ALL.into_iter().find(|c| c.word() == word)
    }

    /// Whether its accounts have quota rows of their own. agy's realm has no registry key
    /// (`aterm-db` `typed::keys::canonical_realm` refuses it), so agy publishes per home.
    #[must_use]
    pub const fn has_accounts(self) -> bool {
        !matches!(self, Cli::Agy)
    }
}

impl fmt::Display for Cli {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// The registry's row key for an account: `ac-` and 8 bytes of SHA-256 over its realm and
/// claims email (`aterm-db` `typed::keys::account_pk`), or `sa-` for a workspace's
/// sub-account (`subaccount_pk`). An unkeyed hash: it names a row, it does not hide the
/// address from a member who can guess it (§4.14 "Who can read what").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountKey {
    sub: bool,
    id: [u8; 8],
}

impl AccountKey {
    /// `ac-<16 lowercase hex>` or `sa-<16 lowercase hex>`.
    #[must_use]
    pub fn parse(word: &str) -> Option<AccountKey> {
        let (sub, hex) = match word.split_at_checked(3)? {
            ("ac-", hex) => (false, hex),
            ("sa-", hex) => (true, hex),
            _ => return None,
        };
        Some(AccountKey {
            sub,
            id: parse_hex::<8>(hex)?,
        })
    }

    /// Whether it is a workspace's sub-account key (`sa-`).
    #[must_use]
    pub const fn is_sub(self) -> bool {
        self.sub
    }

    /// The canonical 19-byte `(ac|sa)-<16 lowercase hex>` representation.
    #[must_use]
    pub(crate) const fn to_ascii_bytes(self) -> [u8; 19] {
        let prefix = if self.sub { b"sa-" } else { b"ac-" };
        format_hex_lower::<8, 19>(prefix, &self.id)
    }
}

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let buf = self.to_ascii_bytes();
        f.write_str(std::str::from_utf8(&buf).unwrap_or_default())
    }
}

/// An agy home's launch key (`aterm-agent` `harness::agy_quota_store::launch_key`): `home`
/// for the person's own, or `h` and an FNV-1a/64 of the HOME and flags agy runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HomeKey {
    /// The person's own home, run with no flags.
    Home,
    /// Another home or another set of flags, by its hash.
    Hashed(u64),
}

impl HomeKey {
    /// `home` or `h<16 lowercase hex>`.
    #[must_use]
    pub fn parse(word: &str) -> Option<HomeKey> {
        if word == "home" {
            return Some(HomeKey::Home);
        }
        parse_hex::<8>(word.strip_prefix('h')?).map(|b| HomeKey::Hashed(u64::from_be_bytes(b)))
    }
}

impl fmt::Display for HomeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HomeKey::Home => f.write_str("home"),
            HomeKey::Hashed(h) => {
                let bytes = h.to_be_bytes();
                let buf = format_hex_lower::<8, 17>(b"h", &bytes);
                f.write_str(std::str::from_utf8(&buf).unwrap_or_default())
            }
        }
    }
}

/// The width a summary subject holds: one record per closed window of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Width {
    /// `1m`: a few totals, kept 2 hours.
    Minute,
    /// `1h`: per CLI, model family and token class, kept 8 days.
    Hour,
}

impl Width {
    /// Both widths, finest first.
    pub const ALL: [Width; 2] = [Width::Minute, Width::Hour];

    /// The subject segment.
    #[must_use]
    pub const fn segment(self) -> &'static str {
        match self {
            Width::Minute => "1m",
            Width::Hour => "1h",
        }
    }

    /// astream's window length.
    #[must_use]
    pub const fn window_len(self) -> WindowLen {
        match self {
            Width::Minute => WindowLen::Minute,
            Width::Hour => WindowLen::Hour,
        }
    }

    /// The width of `len`, when a subject holds it.
    #[must_use]
    pub const fn of(len: WindowLen) -> Option<Width> {
        match len {
            WindowLen::Minute => Some(Width::Minute),
            WindowLen::Hour => Some(Width::Hour),
            WindowLen::Day | WindowLen::Week => None,
        }
    }
}

/// A model family, as the harness reads it from a model id (`harness::upgrade_models::
/// family_version`), closed: a family this build does not name is `other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// Claude Fable.
    Fable,
    /// Claude Mythos.
    Mythos,
    /// Claude Opus.
    Opus,
    /// Claude Sonnet.
    Sonnet,
    /// Claude Haiku.
    Haiku,
    /// A Claude model with no family word above, or none at all (`unknown`, an older id).
    Other,
    /// Every Codex model: the harness reads no family from a GPT id.
    Gpt,
}

impl Family {
    /// Claude's families.
    pub const CLAUDE: [Family; 6] = [
        Family::Fable,
        Family::Mythos,
        Family::Opus,
        Family::Sonnet,
        Family::Haiku,
        Family::Other,
    ];

    /// The word in a metric name.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Family::Fable => "fable",
            Family::Mythos => "mythos",
            Family::Opus => "opus",
            Family::Sonnet => "sonnet",
            Family::Haiku => "haiku",
            Family::Other => "other",
            Family::Gpt => "gpt",
        }
    }

    /// The family a Claude model's family word names: `other` for any word this build does
    /// not know, so a new family never makes a new subject.
    #[must_use]
    pub fn of_claude(word: Option<&str>) -> Family {
        Family::CLAUDE
            .into_iter()
            .find(|f| Some(f.word()) == word)
            .unwrap_or(Family::Other)
    }
}

/// A token class (`harness::usage::TokenUsage`): the four that add up to a message's tokens.
/// `cache_write_1h` is a part of `cache_write`, so it is not a class of its own here: the
/// classes of one message sum to its total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenClass {
    /// Uncached input.
    Input,
    /// Output.
    Output,
    /// Input read from the prompt cache.
    CacheRead,
    /// Input written to the prompt cache, both lifetimes.
    CacheWrite,
}

impl TokenClass {
    /// Every class.
    pub const ALL: [TokenClass; 4] = [
        TokenClass::Input,
        TokenClass::Output,
        TokenClass::CacheRead,
        TokenClass::CacheWrite,
    ];

    /// The word in a metric name.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            TokenClass::Input => "input",
            TokenClass::Output => "output",
            TokenClass::CacheRead => "cache_read",
            TokenClass::CacheWrite => "cache_write",
        }
    }
}

/// What a summary measures: a compiled name, so no subject grows with history (§4.6).
///
/// Only Claude and Codex write token counts the harness reads (Grok, Kimi and agy have no
/// token reader), so only they have metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Metric {
    /// `<cli>.tokens`, per minute: one sample per Claude message or Codex turn, valued at its
    /// tokens of every class. The summary's count is the messages or turns, its sum the
    /// tokens.
    Tokens(Cli),
    /// `<cli>.<family>.<class>`, per hour: one sample per message or turn of a model of that
    /// family, valued at its tokens of that class (zero included, so each class's count is the
    /// messages).
    Class(Cli, Family, TokenClass),
}

/// The number of metrics this build names.
pub const METRIC_COUNT: usize = 2 + (Family::CLAUDE.len() + 1) * TokenClass::ALL.len();

impl Metric {
    /// Every metric this build names: the two minute totals, then Claude's 24 and Codex's 4
    /// hourly classes.
    pub const ALL: [Metric; METRIC_COUNT] = {
        let mut all = [Metric::Tokens(Cli::Claude); METRIC_COUNT];
        all[1] = Metric::Tokens(Cli::Codex);
        let mut at = 2;
        let mut f = 0;
        while f < Family::CLAUDE.len() {
            let mut c = 0;
            while c < TokenClass::ALL.len() {
                all[at] = Metric::Class(Cli::Claude, Family::CLAUDE[f], TokenClass::ALL[c]);
                at += 1;
                c += 1;
            }
            f += 1;
        }
        let mut c = 0;
        while c < TokenClass::ALL.len() {
            all[at] = Metric::Class(Cli::Codex, Family::Gpt, TokenClass::ALL[c]);
            at += 1;
            c += 1;
        }
        all
    };

    /// The CLI it counts for.
    #[must_use]
    pub const fn cli(self) -> Cli {
        match self {
            Metric::Tokens(cli) | Metric::Class(cli, _, _) => cli,
        }
    }

    /// The width its summaries are kept at.
    #[must_use]
    pub const fn width(self) -> Width {
        match self {
            Metric::Tokens(_) => Width::Minute,
            Metric::Class(..) => Width::Hour,
        }
    }

    /// The name as a static string, as a subject segment and a summary's metric.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Metric::Tokens(Cli::Claude) => "claude.tokens",
            Metric::Tokens(Cli::Codex) => "codex.tokens",
            Metric::Tokens(Cli::Grok) => "grok.tokens",
            Metric::Tokens(Cli::Kimi) => "kimi.tokens",
            Metric::Tokens(Cli::Agy) => "agy.tokens",
            Metric::Class(Cli::Claude, Family::Fable, TokenClass::Input) => "claude.fable.input",
            Metric::Class(Cli::Claude, Family::Fable, TokenClass::Output) => "claude.fable.output",
            Metric::Class(Cli::Claude, Family::Fable, TokenClass::CacheRead) => {
                "claude.fable.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Fable, TokenClass::CacheWrite) => {
                "claude.fable.cache_write"
            }
            Metric::Class(Cli::Claude, Family::Mythos, TokenClass::Input) => "claude.mythos.input",
            Metric::Class(Cli::Claude, Family::Mythos, TokenClass::Output) => {
                "claude.mythos.output"
            }
            Metric::Class(Cli::Claude, Family::Mythos, TokenClass::CacheRead) => {
                "claude.mythos.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Mythos, TokenClass::CacheWrite) => {
                "claude.mythos.cache_write"
            }
            Metric::Class(Cli::Claude, Family::Opus, TokenClass::Input) => "claude.opus.input",
            Metric::Class(Cli::Claude, Family::Opus, TokenClass::Output) => "claude.opus.output",
            Metric::Class(Cli::Claude, Family::Opus, TokenClass::CacheRead) => {
                "claude.opus.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Opus, TokenClass::CacheWrite) => {
                "claude.opus.cache_write"
            }
            Metric::Class(Cli::Claude, Family::Sonnet, TokenClass::Input) => "claude.sonnet.input",
            Metric::Class(Cli::Claude, Family::Sonnet, TokenClass::Output) => {
                "claude.sonnet.output"
            }
            Metric::Class(Cli::Claude, Family::Sonnet, TokenClass::CacheRead) => {
                "claude.sonnet.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Sonnet, TokenClass::CacheWrite) => {
                "claude.sonnet.cache_write"
            }
            Metric::Class(Cli::Claude, Family::Haiku, TokenClass::Input) => "claude.haiku.input",
            Metric::Class(Cli::Claude, Family::Haiku, TokenClass::Output) => "claude.haiku.output",
            Metric::Class(Cli::Claude, Family::Haiku, TokenClass::CacheRead) => {
                "claude.haiku.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Haiku, TokenClass::CacheWrite) => {
                "claude.haiku.cache_write"
            }
            Metric::Class(Cli::Claude, Family::Other, TokenClass::Input) => "claude.other.input",
            Metric::Class(Cli::Claude, Family::Other, TokenClass::Output) => "claude.other.output",
            Metric::Class(Cli::Claude, Family::Other, TokenClass::CacheRead) => {
                "claude.other.cache_read"
            }
            Metric::Class(Cli::Claude, Family::Other, TokenClass::CacheWrite) => {
                "claude.other.cache_write"
            }
            Metric::Class(Cli::Codex, Family::Gpt, TokenClass::Input) => "codex.gpt.input",
            Metric::Class(Cli::Codex, Family::Gpt, TokenClass::Output) => "codex.gpt.output",
            Metric::Class(Cli::Codex, Family::Gpt, TokenClass::CacheRead) => "codex.gpt.cache_read",
            Metric::Class(Cli::Codex, Family::Gpt, TokenClass::CacheWrite) => {
                "codex.gpt.cache_write"
            }
            Metric::Class(..) => "",
        }
    }

    /// The name, as a subject segment and a summary's metric.
    #[must_use]
    pub fn name(self) -> String {
        let s = self.as_str();
        if !s.is_empty() {
            return s.to_owned();
        }
        match self {
            Metric::Tokens(cli) => format!("{cli}.tokens"),
            Metric::Class(cli, family, class) => {
                format!("{cli}.{}.{}", family.word(), class.word())
            }
        }
    }

    /// The name as an astream summary metric.
    #[must_use]
    pub fn metric_name(self) -> MetricName {
        let s = self.as_str();
        if !s.is_empty() {
            return MetricName::new(s)
                .unwrap_or_else(|e| unreachable!("a compiled metric name is a summary name: {e}"));
        }
        MetricName::new(&self.name())
            .unwrap_or_else(|e| unreachable!("a compiled metric name is a summary name: {e}"))
    }

    /// The metric named `name`, when this build names it.
    #[must_use]
    pub fn parse(name: &str) -> Option<Metric> {
        let mut parts = name.split('.');
        let cli = Cli::parse(parts.next()?)?;
        let second = parts.next()?;
        match parts.next() {
            None if second == "tokens" => Metric::tokens(cli),
            Some(class_word) if parts.next().is_none() => {
                let family = match cli {
                    Cli::Claude => Family::CLAUDE.into_iter().find(|f| f.word() == second)?,
                    Cli::Codex if second == Family::Gpt.word() => Family::Gpt,
                    _ => return None,
                };
                let class = TokenClass::ALL
                    .into_iter()
                    .find(|c| c.word() == class_word)?;
                Metric::class(cli, family, class)
            }
            _ => None,
        }
    }

    /// Whether this build names `self` in [`Metric::ALL`].
    #[must_use]
    pub(crate) const fn is_named(self) -> bool {
        match self {
            Metric::Tokens(Cli::Claude | Cli::Codex) => true,
            Metric::Tokens(_) => false,
            Metric::Class(..) => !self.as_str().is_empty(),
        }
    }

    /// The minute total of `cli`'s tokens, when this build names it.
    #[must_use]
    pub fn tokens(cli: Cli) -> Option<Metric> {
        let m = Metric::Tokens(cli);
        m.is_named().then_some(m)
    }

    /// The hourly class metric of `cli`, `family` and `class`, when this build names it.
    #[must_use]
    pub fn class(cli: Cli, family: Family, class: TokenClass) -> Option<Metric> {
        let m = Metric::Class(cli, family, class);
        m.is_named().then_some(m)
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.as_str();
        if !s.is_empty() {
            return f.write_str(s);
        }
        match *self {
            Metric::Tokens(cli) => write!(f, "{cli}.tokens"),
            Metric::Class(cli, family, class) => {
                write!(f, "{cli}.{}.{}", family.word(), class.word())
            }
        }
    }
}

/// A row under `/m/<node>/shared/now/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Now {
    /// The Mac's presence row, every 30 seconds: aterm-mesh's (B7c), not a codec here.
    Presence,
    /// The sessions that need a person, the held sessions and the managers the Fleet menu
    /// shows ([`crate::row::NeedsRow`]).
    Needs,
    /// The account pools this Mac could not read ([`crate::row::UnreadRow`]).
    Unread,
    /// Where an account is placed here (`fill`, `borrow`, …: [`crate::row::PlaceRow`]).
    Place(AccountKey),
}

/// Which retention class a subject falls to ([`crate::rules`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// Raw samples: the prefix rule.
    Raw,
    /// The account journal.
    Journal,
    /// Minute summaries.
    Minute,
    /// Hour summaries.
    Hour,
    /// Quota rows, accounts' and agy homes'.
    Quota,
    /// `now` rows.
    Now,
}

impl Class {
    /// Whether its rows leave the Mac.
    #[must_use]
    pub const fn travels(self) -> bool {
        !matches!(self, Class::Raw | Class::Journal)
    }

    /// Whether the broker keeps only each subject's newest row, by its origin's sequence.
    #[must_use]
    pub const fn keeps_last(self) -> bool {
        matches!(self, Class::Quota | Class::Now)
    }
}

/// One subject of `metrics.log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subject {
    /// `/m/<node>/local/raw/<metric>`: samples from a source that keeps no file of its own.
    Raw {
        /// The Mac.
        node: Node,
        /// What the samples measure.
        metric: Metric,
    },
    /// `/m/<node>/local/journal`: the account event log, in the journal's closed vocabulary.
    Journal {
        /// The Mac.
        node: Node,
    },
    /// `/m/<node>/shared/<1m|1h>/<metric>`: one window record per closed window.
    Window {
        /// The Mac.
        node: Node,
        /// The metric, whose width the subject names.
        metric: Metric,
    },
    /// `/m/<node>/shared/quota/<cli>/<account>`: an account's current row.
    Quota {
        /// The Mac.
        node: Node,
        /// The CLI, never agy.
        cli: Cli,
        /// The account.
        account: AccountKey,
    },
    /// `/m/<node>/shared/quota/agy/<home>`: an agy home's current pools.
    Agy {
        /// The Mac.
        node: Node,
        /// The home.
        home: HomeKey,
    },
    /// `/m/<node>/shared/now/<name>`.
    Now {
        /// The Mac.
        node: Node,
        /// Which row.
        now: Now,
    },
}

impl Subject {
    /// The Mac that writes it.
    #[must_use]
    pub const fn node(&self) -> Node {
        match *self {
            Subject::Raw { node, .. }
            | Subject::Journal { node }
            | Subject::Window { node, .. }
            | Subject::Quota { node, .. }
            | Subject::Agy { node, .. }
            | Subject::Now { node, .. } => node,
        }
    }

    /// Its retention class.
    #[must_use]
    pub const fn class(&self) -> Class {
        match self {
            Subject::Raw { .. } => Class::Raw,
            Subject::Journal { .. } => Class::Journal,
            Subject::Window { metric, .. } => match metric.width() {
                Width::Minute => Class::Minute,
                Width::Hour => Class::Hour,
            },
            Subject::Quota { .. } | Subject::Agy { .. } => Class::Quota,
            Subject::Now { .. } => Class::Now,
        }
    }

    /// An account's quota subject; `None` for agy, whose quota is per home.
    #[must_use]
    pub const fn quota(node: Node, cli: Cli, account: AccountKey) -> Option<Subject> {
        if cli.has_accounts() {
            Some(Subject::Quota { node, cli, account })
        } else {
            None
        }
    }

    /// Read a subject; the one reader of this grammar.
    ///
    /// # Errors
    /// [`SubjectError`] naming the first segment the grammar refuses.
    pub fn parse(subject: &str) -> Result<Subject, SubjectError> {
        let rest = subject.strip_prefix('/').ok_or(SubjectError::Root)?;
        let mut seg = rest.split('/');
        if seg.next() != Some(ROOT) {
            return Err(SubjectError::Root);
        }
        let node = seg.next().and_then(Node::parse).ok_or(SubjectError::Node)?;
        let parsed = match (seg.next(), seg.next()) {
            (Some("local"), Some("raw")) => Subject::Raw {
                node,
                metric: seg
                    .next()
                    .and_then(Metric::parse)
                    .ok_or(SubjectError::Metric)?,
            },
            (Some("local"), Some("journal")) => Subject::Journal { node },
            (Some("shared"), Some(width @ ("1m" | "1h"))) => {
                let metric = seg
                    .next()
                    .and_then(Metric::parse)
                    .ok_or(SubjectError::Metric)?;
                if metric.width().segment() != width {
                    return Err(SubjectError::Width);
                }
                Subject::Window { node, metric }
            }
            (Some("shared"), Some("quota")) => {
                let cli = seg.next().and_then(Cli::parse).ok_or(SubjectError::Cli)?;
                let key = seg.next().ok_or(SubjectError::Shape)?;
                if cli.has_accounts() {
                    Subject::Quota {
                        node,
                        cli,
                        account: AccountKey::parse(key).ok_or(SubjectError::Account)?,
                    }
                } else {
                    Subject::Agy {
                        node,
                        home: HomeKey::parse(key).ok_or(SubjectError::Home)?,
                    }
                }
            }
            (Some("shared"), Some("now")) => {
                let now = match seg.next() {
                    Some("presence") => Now::Presence,
                    Some("needs") => Now::Needs,
                    Some("unread") => Now::Unread,
                    Some("place") => Now::Place(
                        seg.next()
                            .and_then(AccountKey::parse)
                            .ok_or(SubjectError::Account)?,
                    ),
                    _ => return Err(SubjectError::Now),
                };
                Subject::Now { node, now }
            }
            _ => return Err(SubjectError::Class),
        };
        if seg.next().is_some() {
            return Err(SubjectError::Shape);
        }
        Ok(parsed)
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{ROOT}/{}/", self.node())?;
        match self {
            Subject::Raw { metric, .. } => write!(f, "local/raw/{metric}"),
            Subject::Journal { .. } => f.write_str("local/journal"),
            Subject::Window { metric, .. } => {
                write!(f, "shared/{}/{metric}", metric.width().segment())
            }
            Subject::Quota { cli, account, .. } => write!(f, "shared/quota/{cli}/{account}"),
            Subject::Agy { home, .. } => write!(f, "shared/quota/{}/{home}", Cli::Agy),
            Subject::Now { now, .. } => match now {
                Now::Presence => f.write_str("shared/now/presence"),
                Now::Needs => f.write_str("shared/now/needs"),
                Now::Unread => f.write_str("shared/now/unread"),
                Now::Place(account) => write!(f, "shared/now/place/{account}"),
            },
        }
    }
}

/// The filter of every row `node` shares: the owner filter of its relay grant
/// (`relay:/m/{owner}/shared/>`).
#[must_use]
pub fn shared_of(node: Node) -> String {
    format!("/{ROOT}/{node}/shared/>")
}

/// The filter of every row that stays on `node`.
#[must_use]
pub fn local_of(node: Node) -> String {
    format!("/{ROOT}/{node}/local/>")
}
