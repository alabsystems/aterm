// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keys, tiers, caps and values (design §3.1, §3.6). [`FIELDS`] is the one
//! table that declares every key: its tier (where its value may be stored)
//! and its cap (the value's shape and bound). Nothing else decides either.

use crate::vocab::{
    ACT_WORDS, ASK_WORDS, BOX_WORDS, CAUSE_WORDS, CHANGE_WORDS, CLASS_WORDS, CLS_WORDS, DIR_WORDS,
    DUR_WORDS, END_WORDS, HEALED_BY_WORDS, MAIL_KIND_WORDS, NEED_WORDS, NET_WORDS, ORIGIN_WORDS,
    OUT_WORDS, PHASE_WORDS, POOL_WORDS, SOURCE_WORDS, STATE_WORDS, ShortId, VERB_WORDS, WALL_WORDS,
    WHY_WORDS, WORD_WORDS, lookup,
};

/// Where a value may be stored (§3.6). Ordered: a reply, journal or body at
/// one tier carries every key at that tier or below and nothing above.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// **W**: ids, counts, closed words. RAM, journal, bus.
    Wire,
    /// **W\***: program and model identifiers, on the bus only when presence
    /// runs in `meta` mode.
    WireMeta,
    /// **H**: host facts (cwd, mark text, mail parties). RAM and journal,
    /// never the bus.
    Host,
    /// **C**: content (the OSC title). RAM only.
    Content,
}

/// The characters a token key keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    /// `[a-z0-9.-]` (ASCII letters are lowered first): `ver`, `model`,
    /// `effort`, `agent`.
    Lower,
    /// `[a-z0-9.:,-]`: rollups such as `tools=shell:9,edit:8`.
    Rollup,
    /// Any character but a hidden one (`aterm_messages::text::is_hidden_char`):
    /// a program name or a command's detail word, percent-encoded on the
    /// wire.
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
    /// Free text whose percent-encoding is at most this many bytes. Only an
    /// H or C key may hold text.
    Text(usize),
}

/// Declares [`Key`], its words and the [`FIELDS`] table from one list.
macro_rules! keys {
    ($($variant:ident $word:literal $tier:ident $cap:expr,)*) => {
        /// A field key. Closed: a reader skips a key it does not know.
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

            /// The key a wire word names, or `None` (a reader skips it): one
            /// `match` over the same list, so a decode costs no scan of the
            /// table (a carried log is line-decoded inside the update's
            /// freeze, design §5.2).
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
    // A crash gap's last seq the journal holds (`gap why=crash after=<seq>`,
    // design §5.4): what came after it in the dead process is not known.
    After "after" Wire Cap::U,
    Cont "cont" Wire Cap::Id,
    // `cause=@<off>|<seq>` and `re=<seq|@off>` may name mail by its broker
    // offset, which stays on the host (§3.6, §5.5): the bus learns only the
    // class of a cause (`cause_k=mail`), set beside it by `Fields::cause`.
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
    // The transcript's vendor ref: a vendor id, H (§3.6).
    Vref "vref" Host Cap::Id,
    VdurMs "vdur_ms" Wire Cap::U,
    TokIn "tok_in" Wire Cap::U,
    TokOut "tok_out" Wire Cap::U,
    Confirmed "confirmed" Wire Cap::U,
    Prev "prev" Wire Cap::Id,
    Lineage "lineage" Wire Cap::Id,
    // The session that spawned this one (`SessionHandle::parent`), on the
    // session's open: a sid, like `prev=` and `lineage=`. The design writes it
    // `par=`, but `par=` is the line's header token for a node's parent, and a
    // session's parent is no node of its log.
    Parent "parent" Wire Cap::Id,
    Field "field" Wire Cap::Tok(Charset::Lower, 24),
    // A bus snapshot's open path (phase 4, §5.5): `<node>:<kind>` for the
    // deepest open run, turn and need-bearing step, comma-separated; only
    // bus-class nodes (ruling S17).
    Open "open" Wire Cap::Tok(Charset::Rollup, 64),
    // A bus snapshot's open needs, most urgent first:
    // `<word>.<node>.<kind>.<by>.<src>`, comma-separated, cut whole at the cap.
    Needs "needs" Wire Cap::Tok(Charset::Rollup, 200),
    // A bus snapshot's `ms` head: the snapshot is the fold of every bus-class
    // record of its space at or below it, so a reader seeds from it a view
    // whose covers all lie at or below it (phase 4).
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
    Value "value" Host Cap::Text(160),
    Vid "vid" Host Cap::Text(64),
    Tool "tool" Host Cap::Text(64),
    // A mail's sender and a post's address (`mail from= to=`): a principal
    // at a node, `s-<32>@n-<32>` (69 bytes), and a post's `@` before it, 70
    // (`aterm_types::fabric_grammar::PRINCIPAL_MAX`; phase 6, the review of
    // 6F-6: every value the grammar admits is held whole).
    From "from" Host Cap::Text(72),
    To "to" Host Cap::Text(72),
    Off "off" Host Cap::U,
    Title "title" Content Cap::Text(240),
    // Phase 3B, appended so no discriminant moves. The usage tap's reading
    // (`mark - usage h5= d7=`, percent of the 5-hour and 7-day limits).
    H5 "h5" Wire Cap::U,
    D7 "d7" Wire Cap::U,
    // A source mark's counts (`source field=transcript skipped= dropped=`,
    // ruling S31): what was skipped at a bind without a watermark, and
    // transcript events dropped (an over-long line, a restart, a ninth
    // parallel tool, an unmapped result, a queue overflow). Never `lost=`:
    // they were never records, so no reader folds them as ring loss (S27).
    Skipped "skipped" Wire Cap::U,
    Dropped "dropped" Wire Cap::U,
    // Phase 5 (5C-1), appended so no discriminant moves: a task mark's
    // supervisor-attention flag (`task attention=0|1`). The task's exit code is
    // `code=`, as a command's is.
    Attention "attention" Wire Cap::U,
    // Phase 5 (5D-2, ruling T1), appended: what an acted control input says
    // it means (`mark - input verb=key act=approve`).
    Act "act" Wire Cap::Word(ACT_WORDS),
    // Phase 5 (5D-4, ruling T39), appended: a person's `human` mark that no
    // input reaching the program came with (`mark - human reached=0`: a view
    // scroll, a selection, a composition). The fold's mail link reads it as
    // no answer to waiting mail; absent, as before, the input reached the
    // program. Host only: the link it serves has no wire (T10b).
    Reached "reached" Host Cap::U,
    // Phase 5 (5J, ruling T27), appended: the node log's facts. How the
    // previous run ended (`start prev_end=`, a caught signal's number in
    // `code=`), and a config key that changed (`config key=`): the schema's
    // NAME, a static word of aterm's own, never its value and never a name
    // a person chose (a map's member).
    PrevEnd "prev_end" Wire Cap::Word(END_WORDS),
    CfgKey "key" Wire Cap::Id,
    // Phase 6 (6C-2, ruling U6), appended: a turn's close restates who opened
    // it (`op_by=<actor word>`), so a reader holding the close alone, its open
    // evicted, still names the turn's author. The log writes it, never a
    // caller ([`crate::log_owned`]). HOST only: an edge's actor is a raw sid,
    // and the bus aliases only a record's own `by=`, so `op_by=` never
    // crosses the fabric. Printed as text by the local `brief`, never parsed
    // back into an actor, and the fold never reads it.
    OpBy "op_by" Host Cap::Id,
    // Phase 6 (6C-6, ruling U18), appended: whose count a `dropped=` is when
    // it is not its mark's author's own (`pool=shared`: the shared pool the
    // mark limiter's owed-only table spilled into, `crate::limit`). A closed
    // word; HOST only, as the input marks that carry it are.
    Pool "pool" Host Cap::Word(POOL_WORDS),
    // Phase 6 (6F-2, plan item U2; rulings U11-U13), appended so no
    // discriminant moves: the fields of the timeline ring's rows their marks
    // carry once the log holds every fact the ring holds (6F-4..6F-9).
    // WIRE. A handoff's predecessor build (`handoff build=`, the ring's
    // `from_build=`).
    Build "build" Wire Cap::U,
    // The resize ledger's render verdict (`render run= displaced= at=`): its
    // run, the rows it displaced (signed) and when, in ms.
    Run "run" Wire Cap::U,
    Displaced "displaced" Wire Cap::I,
    At "at" Wire Cap::U,
    // The bytes a handback or a reset wrote (`modes bytes=`).
    Bytes "bytes" Wire Cap::U,
    // A keyed write's producer (`in-doubt producer=`, `pty_idem`'s number).
    Producer "producer" Wire Cap::U,
    // The modes a handback or a reset put back (`modes reverted=alt,kitty`),
    // the engine's own mode names: every one of them at once is under 300
    // bytes, and the row's 256-byte payload shows less (the review of 6F-7).
    Reverted "reverted" Wire Cap::Tok(Charset::Rollup, 300),
    // A hold's origin (`advisory origin=`).
    Origin "origin" Wire Cap::Word(ORIGIN_WORDS),
    // What a program's `blocked` report waits on (`program ask=`, ruling
    // U17): OSC 7501's `kind=`, which on a record is the mail kind's key.
    Ask "ask" Wire Cap::Word(ASK_WORDS),
    // How many earlier values a throttled mark class wrote over unwritten
    // (`program coalesced=`, `shown coalesced=`; ruling U44): the newest is
    // always written, and this counts the ones it stands for.
    Coalesced "coalesced" Wire Cap::U,
    // HOST. The holder of a self-claim's act (`act holder=`): an id the
    // server sets, never a caller's text.
    Holder "holder" Host Cap::Id,
    // A post's relay hops (`mail via=`) and the caller's own key for it
    // (`mail idem=`, the post's `key=` token; `key=` is the config key's).
    // `via=` holds more than the row's 256-byte payload can show of it, and
    // its writer cuts it to what the record leaves room for (the grammar
    // admits 560 bytes beside a 70-byte `to=` and a 64-byte `idem=`).
    Via "via" Host Cap::Text(256),
    Idem "idem" Host Cap::Text(64),
    // CONTENT, RAM only (rulings U11-U13): never journaled, carried or
    // published, as the title is. A hold's or an advisory's reason, the
    // supervisor's attention text beside its `fp=`, a topic's name beside
    // its `fp=`, and the raw actor that closed a session (`closer=`, beside
    // the close's `by=`, which says `-` for a sid no one minted). A reason
    // is a pct token of at most 128 bytes whose every `%` starts an escape
    // (`aterm_gui_fabric`'s `reason_token`): 42 escapes, five encoded bytes
    // each, and two more bytes, 212 (the review of 6F-6).
    Reason "reason" Content Cap::Text(212),
    AttnText "attn_text" Content Cap::Text(256),
    TopicName "topic_name" Content Cap::Text(64),
    Closer "closer" Content Cap::Text(64),
    // Phase 6 (6F-5, plan item U3; ruling U11), appended so no discriminant
    // moves: a topic opt-in's move (`topic change=add|drop`, beside its
    // name's `fp=`; not `op=`, the record's own token in `log --json`), and
    // the `since=` token an add resumes from, CONTENT as the name is: a
    // topic is a fingerprint wherever its record leaves RAM.
    Change "change" Wire Cap::Word(CHANGE_WORDS),
    Since "since" Content Cap::Text(64),
    // Phase 6 (6F-6, plan item U3), appended so no discriminant moves: the
    // fabric rows' fields their marks carry. A post's deadline (`mail dl=`,
    // ms) and the origins a hold leaves standing (`hold hold_origin=fleet,
    // local`, the origin words comma-joined); an `inbox seen`'s refill floor
    // (`mail-seen floor=`, a bus offset, so HOST as `off=` is) and its
    // watermark (`seen=`, an inbox id); and whether the act moved anything
    // (`mail-seen moved=0|1`: the fold folds only moves).
    Dl "dl" Wire Cap::U,
    HoldOrigin "hold_origin" Wire Cap::Tok(Charset::Rollup, 64),
    Floor "floor" Host Cap::U,
    Seen "seen" Wire Cap::U,
    Moved "moved" Wire Cap::U,
    // Phase 6 (6F-7, plan item U4b), appended so no discriminant moves: the
    // session rows' fields their marks carry. The resize ledger's verdict:
    // a run's `net=` and `after_ms=` (beside 6F-2's `run= displaced= at=`),
    // and an epoch's `resize_epoch= pushed= cut= drew= class= healed_by=`
    // (not `epoch=`, the log's own epoch, nor `by=`, the record's actor).
    Net "net" Wire Cap::Word(NET_WORDS),
    AfterMs "after_ms" Wire Cap::U,
    ResizeEpoch "resize_epoch" Wire Cap::U,
    Pushed "pushed" Wire Cap::I,
    Cut "cut" Wire Cap::U,
    Drew "drew" Wire Cap::U,
    Class "class" Wire Cap::Word(CLASS_WORDS),
    HealedBy "healed_by" Wire Cap::Word(HEALED_BY_WORDS),
    // A manual reset's source (`modes source=ctl|menu`), and HOST, a
    // handback's foreground groups (`modes from_pg= to_pg=`): a process
    // group is this machine's.
    Source "source" Wire Cap::Word(SOURCE_WORDS),
    FromPg "from_pg" Host Cap::I,
    ToPg "to_pg" Host Cap::I,
    // A keyed write in doubt: its key's sequence (`in-doubt key_seq=`; not
    // `seq=`, the record's own), and CONTENT, RAM only as the ring's row
    // is, the first two words of its reply (`reply=ERR-write`), which its
    // writer bounds once, for the row and the mark both.
    KeySeq "key_seq" Wire Cap::U,
    Reply "reply" Content Cap::Text(64),
    // Appended after the review of 6F-7, so no discriminant moves: a
    // self-claim's act as its holder told it (`act harness_act=<act>`, an
    // owner token of up to 64 bytes of `[A-Za-z0-9._:@/-]`), HOST as its
    // `holder=` is: `rule=` is a wire id, which holds no `@` and no `/`, so
    // the act itself, and an act with no word (`word=unknown`), is read from
    // here, the `harness-act` row's whole.
    HarnessAct "harness_act" Host Cap::Text(64),
}

impl Key {
    /// The key's row of [`FIELDS`].
    fn row(self) -> &'static (Key, Tier, Cap) {
        // `FIELDS` is declared in `Key` order (one macro list), so the
        // discriminant is the row; `fields_table_is_in_key_order` holds it.
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

/// The bytes `s` takes percent-encoded (`aterm_codec::pct::encode`'s rule:
/// a byte that is not ASCII-graphic, and `%`, takes three).
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

/// The longest prefix of `s`, cut at a character boundary, whose
/// percent-encoding fits in `cap` bytes. The timeline's clamp
/// (`session_timeline.rs`'s `clamp_payload`) never bisects a `%XX` escape;
/// this one also never bisects a character, so a clamped value decodes to a
/// prefix of what was written.
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

/// Whether `name` (a chosen name: an `as=`) is one of the actor words a
/// connection's class names ([`crate::vocab::RESERVED_ACTOR_WORDS`]), in any
/// case and around any space.
#[must_use]
pub fn is_reserved_name(name: &str) -> bool {
    crate::vocab::is_reserved_actor_word(&name.trim().to_ascii_lowercase())
}

/// A token value: already through its key's sanitizer and cap.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundedToken(String);

impl BoundedToken {
    /// `raw` sanitized to `key`'s charset and clamped to its cap, or `None`
    /// when `key` is not a token key or nothing survives.
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

/// A free-text value of an H or C key: hidden characters removed, clamped
/// to its key's cap.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BoundedText(String);

impl BoundedText {
    /// `raw` sanitized and clamped for `key`, or `None` when `key` holds no
    /// text (only H and C keys do) or nothing survives. An `as=` (the name a
    /// claim's author declares, printed right after its `by=`) is never one
    /// of the actor words a connection's class names
    /// ([`crate::vocab::RESERVED_ACTOR_WORDS`], in any case): `by=- as=harness`
    /// would read as the harness to a program or an agent reading the log.
    /// Every path that builds one — a mark's fields, the line decoder, the
    /// fold snapshot — comes through here, so none can hold it.
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

    /// Whether this value has the shape `cap` declares (a word must be one
    /// of the cap's list; `unknown` is always allowed, since a reader puts
    /// it in place of a word it does not know).
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
    /// The value does not have the key's shape (or the word is not in its
    /// list, or the text does not survive the sanitizer).
    Shape(Key),
    /// The record already carries [`MAX_FIELDS`] fields.
    Full,
}

/// A record's fields: at most [`MAX_FIELDS`] `(key, value)` pairs, each key
/// once, each value already of its key's shape, in insertion order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fields(Vec<(Key, Val)>);

impl Fields {
    /// No fields.
    #[must_use]
    pub fn new() -> Fields {
        Fields(Vec::new())
    }

    /// Set `key` to `val` (replacing a value it had), or refuse a value of
    /// the wrong shape or a thirteenth key.
    ///
    /// A token or text is held to `key` itself, not only to its cap: it must
    /// be what `key`'s own sanitizer keeps ([`BoundedToken::new`],
    /// [`BoundedText::new`]), so a value sanitized for a W\* key never rides
    /// under a W key. A W or W\* id never holds an `@<off>` broker offset:
    /// an offset names mail, which crosses the bus only as counts (§5.5).
    ///
    /// # Errors
    ///
    /// [`FieldError::Shape`] when `val` does not fit `key`'s cap or `key`
    /// itself, [`FieldError::Full`] when the fields are full.
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
    ///
    /// As [`Fields::set`].
    pub fn with(mut self, key: Key, val: Val) -> Result<Fields, FieldError> {
        self.set(key, val)?;
        Ok(self)
    }

    /// Set a word-valued key to the closed word `word` names.
    ///
    /// # Errors
    ///
    /// [`FieldError::Shape`] when `key` is not word-valued or `word` is not
    /// in its list.
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
    ///
    /// As [`Fields::set`].
    pub fn u(self, key: Key, n: u64) -> Result<Fields, FieldError> {
        self.with(key, Val::U(n))
    }

    /// Set a signed key.
    ///
    /// # Errors
    ///
    /// As [`Fields::set`].
    pub fn i(self, key: Key, n: i64) -> Result<Fields, FieldError> {
        self.with(key, Val::I(n))
    }

    /// Set an id key.
    ///
    /// # Errors
    ///
    /// [`FieldError::Shape`] when `id` is not a [`ShortId`] or `key` holds
    /// no id.
    pub fn id(self, key: Key, id: &str) -> Result<Fields, FieldError> {
        let id = ShortId::new(id).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Id(id))
    }

    /// Set a token key, sanitizing and clamping `raw`.
    ///
    /// # Errors
    ///
    /// [`FieldError::Shape`] when `key` holds no token or nothing survives.
    pub fn tok(self, key: Key, raw: &str) -> Result<Fields, FieldError> {
        let t = BoundedToken::new(key, raw).ok_or(FieldError::Shape(key))?;
        self.with(key, Val::Tok(t))
    }

    /// Set `cause=` to `id` (`@<off>` for mail, a record's seq) and
    /// `cause_k=` to its class (`mail`, `rec`): the id stays on the host,
    /// the class crosses the bus (§5.5).
    ///
    /// # Errors
    ///
    /// [`FieldError::Shape`] when `id` is not a [`ShortId`],
    /// [`FieldError::Full`] when the fields are full.
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
    ///
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
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether none is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
