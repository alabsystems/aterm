// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One journal event (design §4.17 `ev`): what a Mac saw while it used an account anyway, in a
//! closed vocabulary. An event is `Copy` and holds no heap value, every field a number, a word
//! from a fixed list or a bounded word of `[a-z0-9-]` (the account keys, the CLI, the plan), so
//! it can carry no address, URL, path, code or transcript text, and the spool's append encodes
//! it into a stack buffer ([`Event::line`]) without allocating. Times are Unix seconds.

use aterm_json::{Map, Value};

/// The longest encoded event (design §4.17: 192 B an event).
pub const MAX_EVENT: usize = crate::typed::owned::caps::EV_EVENT;

macro_rules! words {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $word:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            /// Every word, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Every word as written, in declaration order: a column's `Ty::Enum`.
            pub const WORDS: &'static [&'static str] = &[$($word),+];

            /// The word it is written as.
            pub const fn word(self) -> &'static str {
                match self { $(Self::$variant => $word),+ }
            }

            /// The value `word` names.
            pub fn parse(word: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.word() == word)
            }
        }
    };
}

words!(
    /// What happened (design §4.17).
    Kind {
        SessionStart = "session-start",
        SessionEnd = "session-end",
        SignedIn = "signed-in",
        SignedOut = "signed-out",
        SignInFailed = "sign-in-failed",
        LoginEnded = "login-ended",
        Renewed = "renewed",
        WrongAccount = "wrong-account",
        Displaced = "displaced",
        Auth = "auth",
        Quota = "quota",
        Wall = "wall",
        WallCleared = "wall-cleared",
        ResetUsed = "reset-used",
        PlanSeen = "plan-seen",
        Credits = "credits",
        Error = "error",
        CodeRequested = "code-requested",
        CodeReceived = "code-received",
        Switch = "switch",
        Enrolled = "enrolled",
        Unenrolled = "unenrolled",
        Probe = "probe",
        // A Claude Code setup-token's life on this Mac (DESIGN-claude-setup-token §4.6, §5.6,
        // §5.9): value-free, the account key alone naming it.
        TokenMinted = "token-minted",
        TokenServed = "token-served",
        TokenVerified = "token-verified",
        TokenRejected = "token-rejected",
        TokenFallback = "token-fallback",
        TokenOffered = "token-offered",
        TokenRemoved = "token-removed",
        TokenDiffer = "token-differ",
        TokenStopped = "token-stopped",
        // What a tab's CLI really runs on, read from its login file
        // (`docs/DESIGN-account-discovery-2026-11-09.md`): the observed account, the tab and the
        // harness that runs it.
        Seen = "seen",
        // The shared files' words (docs/DESIGN-shared-files-2026-11-11.md §10 file 5): a
        // sign-in a bot protection stopped, one no route could start, and an owner's write this
        // Mac published (`op`, `via`, `program`).
        SignInBlocked = "sign-in-blocked",
        NoRoute = "no-route",
        Decided = "decided",
    }
);

impl Kind {
    /// A sharp event goes out within the flusher's next pass while this Mac is under its pace
    /// (design §3.9): a sign-in, a sign-out (a displacement by the vendor's own sign-in among
    /// them), a wrong account, an ended login, a wall hit or cleared, or a blocked sign-in
    /// (docs/DESIGN-shared-files-2026-11-11.md §10, "Publication pace"), so the `login` or
    /// `usage` row it changes goes in the same record.
    pub const fn sharp(self) -> bool {
        matches!(
            self,
            Self::SignedIn
                | Self::SignedOut
                | Self::Displaced
                | Self::WrongAccount
                | Self::LoginEnded
                | Self::Wall
                | Self::WallCleared
                | Self::SignInBlocked
        )
    }

    /// Whether an event of this kind may name a tab (`x`) and its harness (`h`): a sighting, a
    /// wrong account and a session's start and end are about one tab's run.
    pub const fn names_tab(self) -> bool {
        matches!(
            self,
            Self::Seen | Self::WrongAccount | Self::SessionStart | Self::SessionEnd
        )
    }
}

/// The reason word of an `auth` event whose login reads signed in: the standing word a Mac
/// states with no sign-in of its own to report.
pub const AUTH_OK: &str = "ok";
/// The reason word of an `auth` event that withdraws its Mac's word: the login the word was
/// read of is no longer the account's there.
pub const AUTH_UNSEEN: &str = "unseen";

/// A Mac's sign-in word for one account (design §7: signed in, signed out, ended, wrong,
/// unknown; `failed` is a sign-in that bound no login on an account not signed in there).
pub const SIGNIN_IN: &str = "in";
pub const SIGNIN_OUT: &str = "out";
pub const SIGNIN_ENDED: &str = "ended";
pub const SIGNIN_WRONG: &str = "wrong";
pub const SIGNIN_FAILED: &str = "failed";
/// The Mac withdrew its word: it holds none, and an older event never restores one.
pub const SIGNIN_UNKNOWN: &str = "unknown";
/// Every sign-in word.
pub const SIGNIN_WORDS: [&str; 6] = [
    SIGNIN_IN,
    SIGNIN_OUT,
    SIGNIN_ENDED,
    SIGNIN_WRONG,
    SIGNIN_FAILED,
    SIGNIN_UNKNOWN,
];

/// The reason word a `sign-in-failed` event carries where the sign-in was blocked
/// (`AttemptOutcome::Blocked` in the harness), and an `error` where a blocked sign-in left a
/// login that still works here signed in.
pub const SIGNIN_BLOCKED: &str = "blocked";

/// The reason word a `seen` event carries on a first sighting of its (tab, account) pair.
pub const SEEN_FIRST: &str = "first";
/// The reason word a `seen` event carries when a tab's account changed.
pub const SEEN_CHANGED: &str = "changed";

words!(
    /// Where a figure or fact came from.
    Source {
        Statusline = "statusline",
        Rollout = "rollout",
        SessionUsage = "session-usage",
        CliOutput = "cli-output",
        SigninFlow = "signin-flow",
        Error = "error",
        Relay = "relay",
        Probe = "probe",
        // Where an observed login was read (`seen`): Claude Code's profile, a CLI's auth file,
        // a CLI's own log, or the vault token entry a token route runs on.
        Profile = "profile",
        AuthFile = "auth-file",
        CliLog = "cli-log",
        VaultToken = "vault-token",
        // A `decided` event's: an owner's write to a shared file, through `aterm file`.
        File = "file",
    }
);

words!(
    /// What a published shared-file write does, a `decided`'s verb (`via`), as the publisher
    /// reads it off the intent (`file::publish::via`): a cell written, a row born, a row's
    /// lifecycle ended, a revert, a person's resolve of a held write, or a Mac's statement of its
    /// own row. A face's commit is named by what each of its writes does. A word this build does
    /// not know drops the event whole, as an unknown kind does.
    Via {
        Set = "set",
        Add = "add",
        Rm = "rm",
        Revert = "revert",
        Resolve = "resolve",
        State = "state",
    }
);

/// The CLI word of a `decided`: its write went through `aterm file`, whatever program asked.
pub const DECIDED_CLI: &str = "aterm";

words!(
    /// How exactly a reset time is known (design §7: window identity tolerance).
    Res {
        Exact = "exact",
        Minute = "minute",
        Hour = "hour",
        Day = "day",
        Local = "local",
    }
);

words!(
    /// A limit window, in the plan collection's words.
    Window {
        FiveHour = "5h",
        Week = "7d",
        WeekModel = "7d-model",
        Day = "day",
        Month = "month",
        Credits = "credits",
    }
);

words!(
    /// The model whose own weekly bucket a `7d-model` wall is of (FL-269): the bucket's id as the
    /// harness names it (`harness::profile::Bucket::id`) and the survey's `model_walls=` writes
    /// it. A wall of Fable's, Opus's or Sonnet's week walls that model's sessions alone, never
    /// the account (`docs/OWNER_RULINGS.md`, 2026-11-11: "no, keep the same account and switch
    /// from fable to opus."). [`Self::Other`] is the week of a model the harness names no bucket
    /// of (`seven_day_other`, a `/usage` panel's `seven_day_<name>`): the harness takes its wall
    /// as the whole account's (`harness::accounts::Account::limited_until`), so every reader
    /// takes it as a wall for every session, and reads a bucket word it does not know as this
    /// one. A `7d-model` wall that names no model (an older build's) cannot say whose week it
    /// is, so no reader takes it as any model's wall or as the account's.
    ModelBucket {
        Fable = "seven_day_overage_included",
        Opus = "seven_day_opus",
        Sonnet = "seven_day_sonnet",
        Other = "seven_day_other",
    }
);

impl ModelBucket {
    /// The bucket a row's `model_walls` entry names by `word`: its own where this build has the
    /// word, else [`Self::Other`], a week of a model this build names no bucket of (a newer
    /// build's), whose wall this build takes as every session's, as it takes its own.
    #[must_use]
    pub fn read(word: &str) -> Self {
        Self::parse(word).unwrap_or(Self::Other)
    }
}

/// A bounded word of `N` bytes or fewer, held inline.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Word<const N: usize> {
    len: u8,
    bytes: [u8; N],
}

impl<const N: usize> Word<N> {
    /// `text` when it is 1 to `N` bytes of `[a-z0-9-]`.
    pub fn new(text: &str) -> Option<Self> {
        let raw = text.as_bytes();
        if raw.is_empty()
            || raw.len() > N
            || !raw
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        {
            return None;
        }
        let mut bytes = [0; N];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(Self {
            len: u8::try_from(raw.len()).ok()?,
            bytes,
        })
    }

    /// The word.
    pub fn as_str(&self) -> &str {
        // Only `[a-z0-9-]` bytes are ever stored.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl<const N: usize> std::fmt::Debug for Word<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An account or sub-account key: `ac-<16hex>` or `sa-<16hex>` (design §5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pk(Word<19>);

impl Pk {
    /// `text` when it is an account or sub-account key.
    pub fn new(text: &str) -> Option<Self> {
        let (prefix, hex) = text.split_at_checked(3)?;
        ((prefix == "ac-" || prefix == "sa-")
            && hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some(())?;
        Word::new(text).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Debug for Pk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A sub-account's observed `AccountKey`: 12 lower-case hex digits.
pub type SubKey = Word<12>;

/// One event (design §4.17 `ev`): `{t, k, a, c, s?, w?, m?, p?, r?, rr?, u?, plan?, src, why?,
/// to?, x?, h?, op?, via?, pg?}`, `a` absent only on a `probe` of an unpooled CLI's unseen login
/// and on a `decided`, `m` only on a `wall`, `wall-cleared` or `reset-used` of the `7d-model`
/// window, `x` and `h` only on a kind that [names a tab](Kind::names_tab), and `op`, `via` and
/// `pg` on a `decided` and on nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Event {
    /// When, Unix seconds.
    pub t: u64,
    pub kind: Kind,
    /// The account it is about. Only a `probe` may name none: a read of an unpooled CLI (agy,
    /// Muse) whose login the reader did not see is attributed by its realm alone, which the CLI
    /// word fixes (design §11 step 9).
    pub account: Option<Pk>,
    /// The CLI word (`claude`, `codex`, `grok`, `kimi`, `agy`, `muse`, `codex-alab`); `aterm`
    /// on a `decided`, whose write went through `aterm file`.
    pub cli: Word<10>,
    /// The sub-account's `AccountKey`, where the account has workspaces.
    pub sub: Option<SubKey>,
    pub window: Option<Window>,
    /// The model a `7d-model` wall, its clearing or a refill of that window is of
    /// ([`ModelBucket`]).
    pub model: Option<ModelBucket>,
    /// Tenths of a percent used.
    pub pct_x10: Option<u16>,
    /// The window's reset, Unix seconds.
    pub reset: Option<u64>,
    pub res: Option<Res>,
    /// Until when a wall stands, Unix seconds.
    pub until: Option<u64>,
    /// The plan word the CLI reported (`max`, `pro`, `plus`).
    pub plan: Option<Word<12>>,
    pub src: Source,
    /// A short reason word.
    pub why: Option<Word<16>>,
    /// The other account of a switch or a displacement; the believed account of a
    /// `wrong-account` a tab's observation contradicted.
    pub to: Option<Pk>,
    /// The tab: 8 hex of its id (`docs/DESIGN-account-discovery-2026-11-09.md`).
    pub tab: Option<Word<8>>,
    /// The harness word of the tab's session.
    pub harness: Option<Word<16>>,
    /// A `decided`'s operation: the first 8 bytes of its digest, 16 lower-case hex.
    pub op: Option<Word<16>>,
    /// A `decided`'s verb: what the published intent does ([`Via`]).
    pub via: Option<Via>,
    /// A `decided`'s author, as the intent says it (its `by`: `person`, `program`, `editor`,
    /// `harness`, an agent's word): a claim, never an admission condition
    /// (docs/DESIGN-shared-files-2026-11-11.md §5). A word of `aterm file --by`'s grammar,
    /// `[a-z][a-z0-9-]{0,19}` ([`aterm_file::name::part`]); a build that held it to sixteen
    /// bytes refuses a longer one.
    pub program: Option<Word<20>>,
}

impl Event {
    /// The sign-in word this event states of its account on the Mac that logged it, where it
    /// states one: the one mapping every fold reads (this Mac's book, the window's readers and
    /// the relay), so no two of them can read one event two ways.
    ///
    /// An `auth` event states the login's standing word: `ok` is `in` (a login that reads
    /// working again with no sign-in to report) and `unseen` withdraws the Mac's word; any other
    /// reason (`epoch`) states none. A `seen` states none: a profile file names its account long
    /// after the login ended, so a sighting never says `in` over an `ended`; and a
    /// `wrong-account` that names a tab is a belief corrected, not a sign-in word.
    pub fn signin(&self) -> Option<&'static str> {
        match self.kind {
            Kind::SignedIn | Kind::Renewed => Some(SIGNIN_IN),
            Kind::SignedOut | Kind::Displaced => Some(SIGNIN_OUT),
            Kind::LoginEnded => Some(SIGNIN_ENDED),
            Kind::WrongAccount if self.tab.is_none() => Some(SIGNIN_WRONG),
            Kind::SignInFailed | Kind::SignInBlocked | Kind::NoRoute => Some(SIGNIN_FAILED),
            Kind::Auth => match self.why.as_ref().map(Word::as_str) {
                Some(AUTH_OK) => Some(SIGNIN_IN),
                Some(AUTH_UNSEEN) => Some(SIGNIN_UNKNOWN),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether this event may name a model ([`ModelBucket`]): a wall, its clearing or a refill
    /// (`reset-used`) of the `7d-model` window, the one window whose walls and refills are a
    /// model's own week's (FL-271: a refill of the Sonnet week refills no Fable week).
    pub fn names_model(&self) -> bool {
        matches!(self.kind, Kind::Wall | Kind::WallCleared | Kind::ResetUsed)
            && self.window == Some(Window::WeekModel)
    }

    /// Whether the flusher sends this event out on its next pass (design §3.9): a sharp kind, a
    /// sign-in a bot protection or a person's step stopped (why `blocked`: a `sign-in-failed`,
    /// or the `error` the harness notes where the login it would have renewed still works here;
    /// the forms builds before SF-08 read, since they drop the `sign-in-blocked` kind), or a
    /// `seen` of a tab's first sighting or changed account. An hourly refresh of a sighting
    /// waits for the routine flush.
    pub fn sharp(&self) -> bool {
        let why = self.why.as_ref().map(Word::as_str);
        self.kind.sharp()
            || (matches!(self.kind, Kind::SignInFailed | Kind::Error)
                && why == Some(SIGNIN_BLOCKED))
            || (self.kind == Kind::Seen && matches!(why, Some(SEEN_FIRST | SEEN_CHANGED)))
    }
}

/// A fixed buffer that refuses to overflow, so encoding never allocates.
struct Line<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Line<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.at.checked_add(bytes.len())?;
        self.buf.get_mut(self.at..end)?.copy_from_slice(bytes);
        self.at = end;
        Some(())
    }

    fn number(&mut self, mut n: u64) -> Option<()> {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.put(&digits[i..])
    }

    fn key(&mut self, first: &mut bool, key: &str) -> Option<()> {
        if !*first {
            self.put(b",")?;
        }
        *first = false;
        self.put(b"\"")?;
        self.put(key.as_bytes())?;
        self.put(b"\":")
    }

    fn text(&mut self, first: &mut bool, key: &str, word: &str) -> Option<()> {
        self.key(first, key)?;
        self.put(b"\"")?;
        self.put(word.as_bytes())?;
        self.put(b"\"")
    }

    fn uint(&mut self, first: &mut bool, key: &str, n: u64) -> Option<()> {
        self.key(first, key)?;
        self.number(n)
    }
}

impl Event {
    /// The event as one canonical JSON object (keys in byte order, as `aterm_json` writes an
    /// object) into `buf`, without allocating: its length, or `None` when it passes
    /// [`MAX_EVENT`] or `buf`. Every string is a checked word, so nothing needs escaping.
    pub fn encode(&self, buf: &mut [u8]) -> Option<usize> {
        let mut line = Line { buf, at: 0 };
        let mut first = true;
        let f = &mut first;
        line.put(b"{")?;
        if let Some(account) = &self.account {
            line.text(f, "a", account.as_str())?;
        }
        line.text(f, "c", self.cli.as_str())?;
        if let Some(h) = &self.harness {
            line.text(f, "h", h.as_str())?;
        }
        line.text(f, "k", self.kind.word())?;
        if let Some(m) = self.model {
            line.text(f, "m", m.word())?;
        }
        if let Some(op) = &self.op {
            line.text(f, "op", op.as_str())?;
        }
        if let Some(p) = self.pct_x10 {
            line.uint(f, "p", u64::from(p))?;
        }
        if let Some(pg) = &self.program {
            line.text(f, "pg", pg.as_str())?;
        }
        if let Some(plan) = &self.plan {
            line.text(f, "plan", plan.as_str())?;
        }
        if let Some(r) = self.reset {
            line.uint(f, "r", r)?;
        }
        if let Some(rr) = self.res {
            line.text(f, "rr", rr.word())?;
        }
        if let Some(s) = &self.sub {
            line.text(f, "s", s.as_str())?;
        }
        line.text(f, "src", self.src.word())?;
        line.uint(f, "t", self.t)?;
        if let Some(to) = &self.to {
            line.text(f, "to", to.as_str())?;
        }
        if let Some(u) = self.until {
            line.uint(f, "u", u)?;
        }
        if let Some(via) = self.via {
            line.text(f, "via", via.word())?;
        }
        if let Some(w) = self.window {
            line.text(f, "w", w.word())?;
        }
        if let Some(why) = &self.why {
            line.text(f, "why", why.as_str())?;
        }
        if let Some(x) = &self.tab {
            line.text(f, "x", x.as_str())?;
        }
        line.put(b"}")?;
        (line.at <= MAX_EVENT).then_some(line.at)
    }

    /// The event as a JSON value, as an `ev` batch holds it.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        let mut text = |k: &str, v: &str| {
            o.insert(k.to_owned(), Value::from(v));
        };
        if let Some(account) = &self.account {
            text("a", account.as_str());
        }
        text("c", self.cli.as_str());
        text("k", self.kind.word());
        text("src", self.src.word());
        if let Some(plan) = &self.plan {
            text("plan", plan.as_str());
        }
        if let Some(rr) = self.res {
            text("rr", rr.word());
        }
        if let Some(s) = &self.sub {
            text("s", s.as_str());
        }
        if let Some(to) = &self.to {
            text("to", to.as_str());
        }
        if let Some(w) = self.window {
            text("w", w.word());
        }
        if let Some(m) = self.model {
            text("m", m.word());
        }
        if let Some(why) = &self.why {
            text("why", why.as_str());
        }
        if let Some(x) = &self.tab {
            text("x", x.as_str());
        }
        if let Some(h) = &self.harness {
            text("h", h.as_str());
        }
        if let Some(op) = &self.op {
            text("op", op.as_str());
        }
        if let Some(via) = self.via {
            text("via", via.word());
        }
        if let Some(pg) = &self.program {
            text("pg", pg.as_str());
        }
        o.insert("t".to_owned(), Value::from(self.t));
        for (k, v) in [
            ("p", self.pct_x10.map(u64::from)),
            ("r", self.reset),
            ("u", self.until),
        ] {
            if let Some(v) = v {
                o.insert(k.to_owned(), Value::from(v));
            }
        }
        Value::Object(o)
    }

    /// The event a value holds, when every field is well formed and nothing else is there: a
    /// peer's modified build cannot slip text past the vocabulary.
    pub fn from_value(value: &Value) -> Option<Self> {
        let o = value.as_object()?;
        const KEYS: [&str; 20] = [
            "a", "c", "h", "k", "m", "op", "p", "pg", "plan", "r", "rr", "s", "src", "t", "to",
            "u", "via", "w", "why", "x",
        ];
        if o.keys().any(|k| !KEYS.contains(&k.as_str())) {
            return None;
        }
        let text = |k: &str| o.get(k).map(|v| v.as_str()).map(|s| s.ok_or(()));
        let uint = |k: &str| o.get(k).map(|v| v.as_u64().ok_or(()));
        fn opt<T>(v: Option<Result<T, ()>>) -> Option<Option<T>> {
            match v {
                None => Some(None),
                Some(Ok(v)) => Some(Some(v)),
                Some(Err(())) => None,
            }
        }
        let word = |k: &str| opt(text(k));
        let event = Self {
            t: uint("t")?.ok()?,
            kind: Kind::parse(text("k")?.ok()?)?,
            account: match word("a")? {
                None => None,
                Some(a) => Some(Pk::new(a)?),
            },
            cli: Word::new(text("c")?.ok()?)?,
            sub: match word("s")? {
                None => None,
                Some(s) => Some(Word::new(s).filter(|w: &SubKey| {
                    w.as_str().len() == 12 && w.as_str().bytes().all(|b| b.is_ascii_hexdigit())
                })?),
            },
            window: match word("w")? {
                None => None,
                Some(w) => Some(Window::parse(w)?),
            },
            model: match word("m")? {
                None => None,
                Some(m) => Some(ModelBucket::parse(m)?),
            },
            pct_x10: match opt(uint("p"))? {
                None => None,
                Some(p) => Some(u16::try_from(p).ok().filter(|p| *p <= 1000)?),
            },
            reset: opt(uint("r"))?,
            res: match word("rr")? {
                None => None,
                Some(w) => Some(Res::parse(w)?),
            },
            until: opt(uint("u"))?,
            plan: match word("plan")? {
                None => None,
                Some(w) => Some(Word::new(w)?),
            },
            src: Source::parse(text("src")?.ok()?)?,
            why: match word("why")? {
                None => None,
                Some(w) => Some(Word::new(w)?),
            },
            to: match word("to")? {
                None => None,
                Some(w) => Some(Pk::new(w)?),
            },
            tab: match word("x")? {
                None => None,
                Some(w) => Some(Word::new(w).filter(|x: &Word<8>| {
                    x.as_str().len() == 8 && x.as_str().bytes().all(|b| b.is_ascii_hexdigit())
                })?),
            },
            harness: match word("h")? {
                None => None,
                Some(w) => Some(Word::new(w)?),
            },
            op: match word("op")? {
                None => None,
                Some(w) => Some(Word::new(w).filter(|op: &Word<16>| {
                    op.as_str().len() == 16 && op.as_str().bytes().all(|b| b.is_ascii_hexdigit())
                })?),
            },
            via: match word("via")? {
                None => None,
                Some(w) => Some(Via::parse(w)?),
            },
            program: match word("pg")? {
                None => None,
                Some(w) => Some(Word::new(w).filter(|_| aterm_file::name::part(w))?),
            },
        };
        // Only a kind about one tab's run names the tab and its harness.
        if (event.tab.is_some() || event.harness.is_some()) && !event.kind.names_tab() {
            return None;
        }
        if event.model.is_some() && !event.names_model() {
            return None;
        }
        // A `decided` names its operation, verb and author, and nothing else does. It is about a
        // write to a shared file through `aterm file` (`c` `aterm`, `src` `file`, the one source
        // no other kind has) and names nothing else: no account, window, figure, reason or tab.
        let decided = event.op.is_some() && event.via.is_some() && event.program.is_some();
        let any = event.op.is_some() || event.via.is_some() || event.program.is_some();
        if (event.kind == Kind::Decided) != decided || (event.kind != Kind::Decided && any) {
            return None;
        }
        if (event.kind == Kind::Decided) != (event.src == Source::File) {
            return None;
        }
        if event.kind == Kind::Decided
            && (event.cli.as_str() != DECIDED_CLI
                || event.account.is_some()
                || event.sub.is_some()
                || event.window.is_some()
                || event.model.is_some()
                || event.pct_x10.is_some()
                || event.reset.is_some()
                || event.res.is_some()
                || event.until.is_some()
                || event.plan.is_some()
                || event.why.is_some()
                || event.to.is_some()
                || event.tab.is_some()
                || event.harness.is_some())
        {
            return None;
        }
        // An event names its account; a probe may name only its realm, and then no workspace;
        // a `decided` is about a write to a shared file, not an account.
        if event.account.is_none()
            && !(event.kind == Kind::Decided || (event.kind == Kind::Probe && event.sub.is_none()))
        {
            return None;
        }
        let mut buf = [0; MAX_EVENT];
        event.encode(&mut buf)?;
        Some(event)
    }

    /// The event one spool line holds.
    pub fn from_line(line: &[u8]) -> Option<Self> {
        if line.len() > MAX_EVENT {
            return None;
        }
        Self::from_value(&aterm_json::from_slice(line).ok()?)
    }
}
