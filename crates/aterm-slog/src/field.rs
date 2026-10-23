// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keys, tiers, caps and values (design §3.1, §3.6).

use crate::vocab::{
    ACT_WORDS, ASK_WORDS, BOX_WORDS, CAUSE_WORDS, CHANGE_WORDS, CLASS_WORDS, CLS_WORDS, DIR_WORDS,
    DUR_WORDS, END_WORDS, HEALED_BY_WORDS, HELD_WORDS, MAIL_KIND_WORDS, NEED_WORDS, NET_WORDS,
    ORIGIN_WORDS, OUT_WORDS, PHASE_WORDS, POOL_WORDS, SEV_WORDS, SOURCE_WORDS, STATE_WORDS,
    ShortId, VERB_WORDS, WALL_WORDS, WHY_WORDS, WORD_WORDS, lookup,
};

/// Where a value may be stored (§3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// **W**: ids, counts, closed words.
    Wire,
    /// **W\***: program and model identifiers, on the bus only when presence runs in `meta` mode.
    WireMeta,
    /// **H**: host facts (cwd, mark text, mail parties).
    Host,
    /// **C**: content (the OSC title).
    Content,
}

/// The characters a token key keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    /// `[a-z0-9.-]` (ASCII letters are lowered first): `ver`, `model`, `effort`, `agent`.
    Lower,
    /// `[a-z0-9.:,-]`: rollups such as `tools=shell:9,edit:8`.
    Rollup,
    /// Any character but a hidden one (`aterm_messages::text::is_hidden_char`).
    Detail,
}

/// A key's value shape and bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    /// A word of a closed list; an unknown word decodes to `unknown`.
    Word(&'static [&'static str]),
    /// An unsigned count or id.
    U,
    /// A signed number (an exit code).
    I,
    /// A [`ShortId`].
    Id,
    /// A sanitized token whose percent-encoding is at most this many bytes.
    Tok(Charset, usize),
    /// Free text whose percent-encoding is at most this many bytes.
    Text(usize),
}

/// Declares [`Key`], its words and the [`FIELDS`] table from one list.
macro_rules! keys {
    ($($variant:ident $word:literal $tier:ident $cap:expr,)*) => {
        /// A field key.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Key {
            $(#[doc = concat!("`", $word, "=`")] $variant,)*
        }

        /// Every key with its tier and cap, in [`Key`] order: THE table.
        pub static FIELDS: &[(Key, Tier, Cap)] = &[
            $((Key::$variant, Tier::$tier, $cap),)*
        ];

        impl Key {
            /// The wire word.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(Key::$variant => $word,)*
                }
            }

            /// The key a wire word names, or `None` (a reader skips it).
            #[must_use]
            pub fn from_word(word: &str) -> Option<Key> {
                match word {
                    $($word => Some(Key::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

keys! {
    Out "out" Wire Cap::Word(OUT_WORDS),
    Orphan "orphan" Wire Cap::U,
    Code "code" Wire Cap::I,
    Blk "blk" Wire Cap::U,
    Ledger "ledger" Wire Cap::U,
    DurMs "dur_ms" Wire Cap::U,
    Dur "dur" Wire Cap::Word(DUR_WORDS),
    Lost "lost" Wire Cap::U,
    Steps "steps" Wire Cap::U,
    Appr "appr" Wire Cap::U,
    Qs "qs" Wire Cap::U,
    Walls "walls" Wire Cap::U,
    Stalls "stalls" Wire Cap::U,
    Mail "mail" Wire Cap::U,
    Errs "errs" Wire Cap::U,
    Tools "tools" Wire Cap::Tok(Charset::Rollup, 64),
    Box "box" Wire Cap::Word(BOX_WORDS),
    Wall "wall" Wire Cap::Word(WALL_WORDS),
    Cls "cls" Wire Cap::Word(CLS_WORDS),
    Need "need" Wire Cap::Word(NEED_WORDS),
    Word "word" Wire Cap::Word(WORD_WORDS),
    Rev "rev" Wire Cap::U,
    Gen "gen" Wire Cap::Id,
    Fp "fp" Wire Cap::Id,
    N "n" Wire Cap::U,
    Pct "pct" Wire Cap::U,
    State "state" Wire Cap::Word(STATE_WORDS),
    Phase "phase" Wire Cap::Word(PHASE_WORDS),
    Why "why" Wire Cap::Word(WHY_WORDS),
    After "after" Wire Cap::U,
    Cont "cont" Wire Cap::Id,
    Cause "cause" Host Cap::Id,
    CauseKind "cause_k" Wire Cap::Word(CAUSE_WORDS),
    Re "re" Host Cap::Id,
    Epoch "epoch" Wire Cap::U,
    Carried "carried" Wire Cap::U,
    Shed "shed" Wire Cap::U,
    Ttl "ttl" Wire Cap::U,
    Rule "rule" Wire Cap::Id,
    Verb "verb" Wire Cap::Word(VERB_WORDS),
    Dir "dir" Wire Cap::Word(DIR_WORDS),
    MailKind "kind" Wire Cap::Word(MAIL_KIND_WORDS),
    Inbox "inbox" Wire Cap::U,
    MailIn "mail_in" Wire Cap::U,
    MailOut "mail_out" Wire Cap::U,
    Vref "vref" Host Cap::Id,
    VdurMs "vdur_ms" Wire Cap::U,
    TokIn "tok_in" Wire Cap::U,
    TokOut "tok_out" Wire Cap::U,
    Confirmed "confirmed" Wire Cap::U,
    Prev "prev" Wire Cap::Id,
    Lineage "lineage" Wire Cap::Id,
    Parent "parent" Host Cap::Id,
    Field "field" Wire Cap::Tok(Charset::Lower, 24),
    Open "open" Wire Cap::Tok(Charset::Rollup, 64),
    Needs "needs" Wire Cap::Tok(Charset::Rollup, 200),
    Upto "upto" Wire Cap::U,
    Prog "prog" WireMeta Cap::Tok(Charset::Detail, 48),
    Agent "agent" WireMeta Cap::Tok(Charset::Lower, 32),
    Detail "detail" WireMeta Cap::Tok(Charset::Detail, 48),
    Ver "ver" WireMeta Cap::Tok(Charset::Lower, 32),
    Model "model" WireMeta Cap::Tok(Charset::Lower, 32),
    Effort "effort" WireMeta Cap::Tok(Charset::Lower, 32),
    Cwd "cwd" Host Cap::Text(256),
    Text "text" Host Cap::Text(160),
    As "as" Host Cap::Text(40),
    Value "value" Host Cap::Text(256),
    Vid "vid" Host Cap::Text(64),
    Tool "tool" Host Cap::Text(64),
    From "from" Host Cap::Text(72),
    To "to" Host Cap::Text(72),
    Off "off" Host Cap::U,
    Title "title" Content Cap::Text(240),
    H5 "h5" Wire Cap::U,
    D7 "d7" Wire Cap::U,
    Skipped "skipped" Wire Cap::U,
    Dropped "dropped" Wire Cap::U,
    Attention "attention" Wire Cap::U,
    Act "act" Wire Cap::Word(ACT_WORDS),
    Reached "reached" Host Cap::U,
    PrevEnd "prev_end" Wire Cap::Word(END_WORDS),
    CfgKey "key" Wire Cap::Id,
    OpBy "op_by" Host Cap::Id,
    Pool "pool" Host Cap::Word(POOL_WORDS),
    Build "build" Wire Cap::U,
    Run "run" Wire Cap::U,
    Displaced "displaced" Wire Cap::I,
    At "at" Wire Cap::U,
    Bytes "bytes" Wire Cap::U,
    Producer "producer" Wire Cap::U,
    Reverted "reverted" Wire Cap::Tok(Charset::Rollup, 300),
    Origin "origin" Wire Cap::Word(ORIGIN_WORDS),
    Ask "ask" Wire Cap::Word(ASK_WORDS),
    Coalesced "coalesced" Wire Cap::U,
    Holder "holder" Host Cap::Id,
    Via "via" Host Cap::Text(256),
    Idem "idem" Host Cap::Text(64),
    Reason "reason" Content Cap::Text(212),
    AttnText "attn_text" Content Cap::Text(256),
    TopicName "topic_name" Content Cap::Text(64),
    Closer "closer" Content Cap::Text(64),
    Change "change" Wire Cap::Word(CHANGE_WORDS),
    Since "since" Content Cap::Text(64),
    Dl "dl" Wire Cap::U,
    HoldOrigin "hold_origin" Wire Cap::Tok(Charset::Rollup, 64),
    Floor "floor" Host Cap::U,
    Seen "seen" Wire Cap::U,
    Moved "moved" Wire Cap::U,
    Net "net" Wire Cap::Word(NET_WORDS),
    AfterMs "after_ms" Wire Cap::U,
    ResizeEpoch "resize_epoch" Wire Cap::U,
    Pushed "pushed" Wire Cap::I,
    Cut "cut" Wire Cap::U,
    Drew "drew" Wire Cap::U,
    Class "class" Wire Cap::Word(CLASS_WORDS),
    HealedBy "healed_by" Wire Cap::Word(HEALED_BY_WORDS),
    Source "source" Wire Cap::Word(SOURCE_WORDS),
    FromPg "from_pg" Host Cap::I,
    ToPg "to_pg" Host Cap::I,
    KeySeq "key_seq" Wire Cap::U,
    Reply "reply" Content Cap::Text(64),
    HarnessAct "harness_act" Host Cap::Text(64),
    LandAt "land_at" Host Cap::I,
    Sev "sev" Wire Cap::Word(SEV_WORDS),
    Held "held" Wire Cap::Word(HELD_WORDS),
    // Appended so no discriminant moves: a `row` mark's
    // fields, a timeline row the log holds as its row alone. The ring's
    // kind as a token (`row=job-change`, a space a `-`), WIRE; and the
    // row's whole payload (`row_text=`, the ring's 256 bytes, a space and
    // an escape's `%` each three encoded), CONTENT, RAM only as the ring
    // row it stands for always was: never journaled, carried or published.
    RowKind "row" Wire Cap::Tok(Charset::Lower, 32),
    RowText "row_text" Content Cap::Text(360),
}

impl Key {
    /// The key's row of [`FIELDS`].
    fn row(self) -> &'static (Key, Tier, Cap) {
        // `FIELDS` is declared in `Key` order (one macro list), so the
        // discriminant is the row; `fields_table_is_in_key_order_and_names_are_unique` holds it.
        &FIELDS[self as usize]
    }

    /// Where this key's value may be stored.
    #[must_use]
    pub fn tier(self) -> Tier {
        self.row().1
    }

    /// The value's shape and bound.
    #[must_use]
    pub fn cap(self) -> Cap {
        self.row().2
    }
}

/// The bytes `s` takes percent-encoded (`aterm_codec::pct::encode`'s rule: a byte that is not ASCII-graphic, and `%`, takes three).
#[must_use]
pub fn encoded_len(s: &str) -> usize {
    s.bytes()
        .map(|b| {
            if b.is_ascii_graphic() && b != b'%' {
                1
            } else {
                3
            }
        })
        .sum()
}

/// The longest prefix of `s`, cut at a character boundary, whose percent-encoding fits in `cap` bytes.
#[must_use]
pub fn clamp_encoded(s: &str, cap: usize) -> &str {
    let mut used = 0usize;
    for (at, ch) in s.char_indices() {
        let mut buf = [0u8; 4];
        let need = encoded_len(ch.encode_utf8(&mut buf));
        if used + need > cap {
            return &s[..at];
        }
        used += need;
    }
    s
}

/// Whether `name` (a chosen name: an `as=`) is one of the actor words a connection's class names ([`crate::vocab::RESERVED_ACTOR_WORDS`]), in any case and around any space.
#[must_use]
pub fn is_reserved_name(name: &str) -> bool {
    crate::vocab::is_reserved_actor_word(&name.trim().to_ascii_lowercase())
}

/// A token value: already through its key's sanitizer and cap.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundedToken(String);

impl BoundedToken {
    /// `raw` sanitized to `key`'s charset and clamped to its cap, or `None` when `key` is not a token key or nothing survives.
    #[must_use]
    pub fn new(key: Key, raw: &str) -> Option<BoundedToken> {
        let Cap::Tok(charset, cap) = key.cap() else {
            return None;
        };
        let kept: String = match charset {
            Charset::Lower => raw
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
                .collect(),
            Charset::Rollup => raw
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|c| {
                    c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || matches!(c, '.' | ':' | ',' | '-')
                })
                .collect(),
            Charset::Detail => raw
                .chars()
                .filter(|c| !aterm_messages::text::is_hidden_char(*c))
                .collect::<String>()
                .trim()
                .to_string(),
        };
        let clamped = clamp_encoded(&kept, cap);
        (!clamped.is_empty()).then(|| BoundedToken(clamped.to_string()))
    }

    /// The token's text (not yet encoded).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A free-text value of an H or C key: hidden characters removed, clamped to its key's cap.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundedText(String);

impl BoundedText {
    /// `raw` sanitized and clamped for `key`, or `None` when `key` holds no text (only H and C keys do) or nothing survives.
    #[must_use]
    pub fn new(key: Key, raw: &str) -> Option<BoundedText> {
        let Cap::Text(cap) = key.cap() else {
            return None;
        };
        if key.tier() < Tier::Host {
            return None;
        }
        let kept: String = raw
            .chars()
            .filter(|c| !aterm_messages::text::is_hidden_char(*c))
            .collect();
        let clamped = clamp_encoded(&kept, cap);
        if key == Key::As && is_reserved_name(clamped) {
            return None;
        }
        (!clamped.is_empty()).then(|| BoundedText(clamped.to_string()))
    }

    /// The text (not yet encoded).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A field value (§3.1, corrected).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    /// A word of the key's closed list.
    Word(&'static str),
    /// An unsigned number.
    U(u64),
    /// A signed number.
    I(i64),
    /// A short id.
    Id(ShortId),
    /// A sanitized token (W and W\* dynamic values).
    Tok(BoundedToken),
    /// Free text (H and C keys only).
    Text(BoundedText),
}

impl Val {
    /// Owned content bytes, excluding spare capacity and allocator overhead.
    pub(crate) fn heap_len(&self) -> usize {
        match self {
            Val::Tok(t) => t.as_str().len(),
            Val::Text(t) => t.as_str().len(),
            Val::Word(_) | Val::U(_) | Val::I(_) | Val::Id(_) => 0,
        }
    }

    /// Whether this value has the shape `cap` declares (a word must be one of the cap's list; `unknown` is always allowed, since a reader puts it in place of a word it does not know).
    #[must_use]
    pub fn fits(&self, cap: Cap) -> bool {
        match (self, cap) {
            (Val::Word(w), Cap::Word(list)) => {
                *w == crate::vocab::UNKNOWN || lookup(list, w).is_some()
            }
            (Val::U(_), Cap::U) | (Val::I(_), Cap::I) | (Val::Id(_), Cap::Id) => true,
            (Val::Tok(t), Cap::Tok(_, cap)) => encoded_len(t.as_str()) <= cap,
            (Val::Text(t), Cap::Text(cap)) => encoded_len(t.as_str()) <= cap,
            _ => false,
        }
    }
}

/// Whether `val` is held to `key` itself (see [`Fields::set`]): a token or
/// text is a fixed point of `key`'s own sanitizer, and an id below the host
/// tier names no broker offset.
fn held_to_key(key: Key, val: &Val) -> bool {
    match val {
        Val::Tok(t) => BoundedToken::new(key, t.as_str()).as_ref() == Some(t),
        Val::Text(t) => BoundedText::new(key, t.as_str()).as_ref() == Some(t),
        Val::Id(id) => key.tier() >= Tier::Host || !id.as_str().contains('@'),
        Val::Word(_) | Val::U(_) | Val::I(_) => true,
    }
}

/// The most fields a record carries.
pub const MAX_FIELDS: usize = 12;

/// Why a field was not set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The value does not have the key's shape (or the word is not in its list, or the text does not survive the sanitizer).
    Shape(Key),
    /// The record already carries [`MAX_FIELDS`] fields.
    Full,
}

/// A record's fields: at most [`MAX_FIELDS`] `(key, value)` pairs, each key once, each value already of its key's shape, in insertion order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fields(Vec<(Key, Val)>);

impl Fields {
    /// No fields.
    #[must_use]
    pub const fn new() -> Fields {
        Fields(Vec::new())
    }

    /// Set `key` to `val` (replacing a value it had), or refuse a value of the wrong shape or a thirteenth key.
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `val` does not fit `key`'s cap or `key` itself, [`FieldError::Full`] when the fields are full.
    pub fn set(&mut self, key: Key, val: Val) -> Result<(), FieldError> {
        if !val.fits(key.cap()) || !held_to_key(key, &val) {
            return Err(FieldError::Shape(key));
        }
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = val;
            return Ok(());
        }
        if self.0.len() >= MAX_FIELDS {
            return Err(FieldError::Full);
        }
        self.0.push((key, val));
        Ok(())
    }

    /// Builder form of [`Fields::set`].
    ///
    /// # Errors
    /// As [`Fields::set`].
    pub fn with(mut self, key: Key, val: Val) -> Result<Fields, FieldError> {
        self.set(key, val)?;
        Ok(self)
    }

    /// Set a word-valued key to the closed word `word` names.
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `key` is not word-valued or `word` is not in its list.
    pub fn word(self, key: Key, word: &str) -> Result<Fields, FieldError> {
        let Cap::Word(list) = key.cap() else {
            return Err(FieldError::Shape(key));
        };
        let w = lookup(list, word).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Word(w))
    }

    /// Set an unsigned key.
    ///
    /// # Errors
    /// As [`Fields::set`].
    pub fn u(self, key: Key, n: u64) -> Result<Fields, FieldError> {
        self.with(key, Val::U(n))
    }

    /// Set a signed key.
    ///
    /// # Errors
    /// As [`Fields::set`].
    pub fn i(self, key: Key, n: i64) -> Result<Fields, FieldError> {
        self.with(key, Val::I(n))
    }

    /// Set an id key.
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `id` is not a [`ShortId`] or `key` holds no id.
    pub fn id(self, key: Key, id: &str) -> Result<Fields, FieldError> {
        let id = ShortId::new(id).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Id(id))
    }

    /// Set a token key, sanitizing and clamping `raw`.
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `key` holds no token or nothing survives.
    pub fn tok(self, key: Key, raw: &str) -> Result<Fields, FieldError> {
        let t = BoundedToken::new(key, raw).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Tok(t))
    }

    /// Set `cause=` to `id` (`@<off>` for mail, a record's seq) and `cause_k=` to its class (`mail`, `rec`).
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `id` is not a [`ShortId`], [`FieldError::Full`] when the fields are full.
    pub fn cause(self, id: &str) -> Result<Fields, FieldError> {
        let class = if id.starts_with('@') {
            "mail"
        } else if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
            "rec"
        } else {
            crate::vocab::UNKNOWN
        };
        self.id(Key::Cause, id)?.word(Key::CauseKind, class)
    }

    /// Set a text key (H or C), sanitizing and clamping `raw`.
    ///
    /// # Errors
    /// [`FieldError::Shape`] when `key` holds no text or nothing survives.
    pub fn text(self, key: Key, raw: &str) -> Result<Fields, FieldError> {
        let t = BoundedText::new(key, raw).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Text(t))
    }

    /// The value of `key`, if set.
    #[must_use]
    pub fn get(&self, key: Key) -> Option<&Val> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// The word `key` holds, if it holds one.
    #[must_use]
    pub fn get_word(&self, key: Key) -> Option<&'static str> {
        match self.get(key) {
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// The number `key` holds, if it holds an unsigned one.
    #[must_use]
    pub fn get_u(&self, key: Key) -> Option<u64> {
        match self.get(key) {
            Some(Val::U(n)) => Some(*n),
            _ => None,
        }
    }

    /// The token or text `key` holds, if any.
    #[must_use]
    pub fn get_str(&self, key: Key) -> Option<&str> {
        match self.get(key) {
            Some(Val::Tok(t)) => Some(t.as_str()),
            Some(Val::Text(t)) => Some(t.as_str()),
            Some(Val::Id(id)) => Some(id.as_str()),
            Some(Val::Word(w)) => Some(w),
            _ => None,
        }
    }

    /// Remove `key`.
    pub fn remove(&mut self, key: Key) {
        self.0.retain(|(k, _)| *k != key);
    }

    /// The fields in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &(Key, Val)> {
        self.0.iter()
    }

    /// How many fields are set.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether none is set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
