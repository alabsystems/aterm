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
    }
);

impl Kind {
    /// A sharp event goes out within the flusher's next pass while this Mac is under its pace
    /// (design §3.9): a sign-in, a sign-out, a wrong account, an ended login or a wall.
    pub const fn sharp(self) -> bool {
        matches!(
            self,
            Self::SignedIn | Self::SignedOut | Self::WrongAccount | Self::LoginEnded | Self::Wall
        )
    }
}

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
    }
);

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

/// One event (design §4.17 `ev`): `{t, k, a, c, s?, w?, p?, r?, rr?, u?, plan?, src, why?, to?}`,
/// `a` absent only on a `probe` of an unpooled CLI's unseen login.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Event {
    /// When, Unix seconds.
    pub t: u64,
    pub kind: Kind,
    /// The account it is about. Only a `probe` may name none: a read of an unpooled CLI (agy,
    /// Muse) whose login the reader did not see is attributed by its realm alone, which the CLI
    /// word fixes (design §11 step 9).
    pub account: Option<Pk>,
    /// The CLI word (`claude`, `codex`, `grok`, `kimi`, `agy`, `muse`, `codex-alab`).
    pub cli: Word<10>,
    /// The sub-account's `AccountKey`, where the account has workspaces.
    pub sub: Option<SubKey>,
    pub window: Option<Window>,
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
    /// The other account of a switch or a displacement.
    pub to: Option<Pk>,
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
        line.text(f, "k", self.kind.word())?;
        if let Some(p) = self.pct_x10 {
            line.uint(f, "p", u64::from(p))?;
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
        if let Some(w) = self.window {
            line.text(f, "w", w.word())?;
        }
        if let Some(why) = &self.why {
            line.text(f, "why", why.as_str())?;
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
        if let Some(why) = &self.why {
            text("why", why.as_str());
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
        const KEYS: [&str; 14] = [
            "a", "c", "k", "p", "plan", "r", "rr", "s", "src", "t", "to", "u", "w", "why",
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
        };
        // An event names its account; a probe may name only its realm, and then no workspace.
        if event.account.is_none() && (event.kind != Kind::Probe || event.sub.is_some()) {
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
