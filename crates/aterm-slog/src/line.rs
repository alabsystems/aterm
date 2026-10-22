// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The line encoding (design §3.2): one record per line of `k=v` tokens,
//! values percent-encoded with the control wire's escape, at most
//! [`MAX_RECORD_BYTES`] per record; the `v=2` bus body uses the same tokens.
//!
//! ```text
//! 412 open t17 turn par=r3 by=human src=screen conf=strong t=1760545012345
//! v=2 t=1760545012345 ep=2 seq=412 ms=97 op=open node=t17 k=turn par=r3 by=human src=screen conf=strong
//! ```
//!
//! [`encode`] at a [`Tier`] emits only keys at or below it ([`FIELDS`]), so
//! `Tier::Wire` can never carry an H or C key. [`decode`] is lossless for
//! the keys it knows, skips tokens it does not, and maps an unknown word to
//! `unknown`.

use std::fmt::Write as _;

use aterm_codec::pct;

use crate::field::{BoundedText, BoundedToken, Cap, FIELDS, Fields, Key, Tier, Val};
use crate::rec::Rec;
use crate::vocab::{Actor, Conf, Kind, Node, Op, ShortId, Src, lookup_or_unknown};

/// The longest encoded record; a larger one is refused (§3.2).
pub const MAX_RECORD_BYTES: usize = 512;

/// Why a record was not encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The encoding would take this many bytes, over [`MAX_RECORD_BYTES`].
    TooLarge(usize),
    /// A record the bus mirror does not carry ([`Rec::is_bus_class`]): a
    /// mail mark, a tool step, a verdict. The second wall behind the class
    /// itself ([`encode_mirror`]), so a producer that misclassifies a record
    /// still cannot put its wire-tier `kind=`, `dir=` or `inbox=` on the bus.
    NotBusClass,
}

/// Why a line did not decode to a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No `seq` (or not a number).
    Seq,
    /// No operation, or an operation this reader does not know.
    Op,
    /// No node, or not a node id.
    Node,
    /// No kind.
    Kind,
    /// A bus body of a version this reader does not know (kept opaque, §3.4).
    Opaque,
    /// Longer than [`MAX_RECORD_BYTES`] (a partly written or foreign line).
    TooLong,
}

/// `rec` as one log line, carrying only the fields at or below `tier`.
///
/// # Errors
///
/// [`EncodeError::TooLarge`] when the line would pass [`MAX_RECORD_BYTES`].
pub fn encode(rec: &Rec, tier: Tier) -> Result<String, EncodeError> {
    encode_with_table(rec, tier, FIELDS)
}

/// [`encode`] reading tiers from `table` instead of [`FIELDS`]: the seam the
/// tier leak test's negative control uses (it marks one H key as W and
/// watches the leak appear). A key `table` does not list is treated as
/// content and emitted only at [`Tier::Content`].
///
/// # Errors
///
/// As [`encode`].
pub fn encode_with_table(
    rec: &Rec,
    tier: Tier,
    table: &[(Key, Tier, Cap)],
) -> Result<String, EncodeError> {
    let mut out = String::with_capacity(96);
    let _ = write!(
        out,
        "{} {} {} {}",
        rec.seq,
        rec.op.word(),
        rec.node,
        rec.k.word()
    );
    if let Some(par) = rec.par {
        let _ = write!(out, " par={par}");
    }
    push_fields(&mut out, &rec.f, tier, table);
    if rec.by != Actor::Nobody {
        out.push_str(" by=");
        out.push_str(rec.by.word());
    }
    let _ = write!(
        out,
        " src={} conf={} t={}",
        rec.src.word(),
        rec.conf.word(),
        rec.t
    );
    checked(out)
}

/// `rec` as a `v=2` bus body (§3.2): the same tokens in `k=v` form, with the
/// session's epoch and, when given, its turn-class counter `ms`. The bus
/// never carries an H or C key, so `tier` is held at [`Tier::WireMeta`].
///
/// # Errors
///
/// As [`encode`].
pub fn encode_bus(rec: &Rec, tier: Tier, ep: u32, ms: Option<u64>) -> Result<String, EncodeError> {
    let tier = tier.min(Tier::WireMeta);
    let mut out = String::with_capacity(112);
    let _ = write!(out, "v=2 t={} ep={ep} seq={}", rec.t, rec.seq);
    if let Some(ms) = ms {
        let _ = write!(out, " ms={ms}");
    }
    let _ = write!(
        out,
        " op={} node={} k={}",
        rec.op.word(),
        rec.node,
        rec.k.word()
    );
    if let Some(par) = rec.par {
        let _ = write!(out, " par={par}");
    }
    push_fields(&mut out, &rec.f, tier, FIELDS);
    if rec.by != Actor::Nobody {
        out.push_str(" by=");
        out.push_str(rec.by.word());
    }
    let _ = write!(out, " src={} conf={}", rec.src.word(), rec.conf.word());
    checked(out)
}

/// `rec` as the bus mirror's `v=2` body (§5.5, phase 4): [`encode_bus`] for
/// a record of the bus class only, with an edge's actor written as its
/// stable alias. `alias` maps a session's sid to the token the
/// bus shows for it (`e<8 hex>`, a keyed hash only this node can make), so
/// two records by one edge still name one actor and the sid stays local;
/// `None` writes `by=-`. No other actor is rewritten: `human ctl harness
/// bridge aterm vendor` name a connection's class, not a session.
///
/// # Errors
///
/// [`EncodeError::NotBusClass`] for a record the mirror does not carry, and
/// otherwise as [`encode`].
pub fn encode_mirror(
    rec: &Rec,
    tier: Tier,
    ep: u32,
    ms: Option<u64>,
    alias: &dyn Fn(&ShortId) -> Option<ShortId>,
) -> Result<String, EncodeError> {
    if !rec.is_bus_class() {
        return Err(EncodeError::NotBusClass);
    }
    match rec.by {
        Actor::Sid(sid) => {
            let mut aliased = rec.clone();
            aliased.by = alias(&sid).map_or(Actor::Nobody, Actor::sid);
            encode_bus(&aliased, tier, ep, ms)
        }
        _ => encode_bus(rec, tier, ep, ms),
    }
}

/// The bytes a record takes at `tier` (what the ring's byte budget counts),
/// or the refusal.
///
/// # Errors
///
/// As [`encode`].
pub fn encoded_size(rec: &Rec, tier: Tier) -> Result<usize, EncodeError> {
    encode(rec, tier).map(|s| s.len())
}

fn checked(out: String) -> Result<String, EncodeError> {
    if out.len() > MAX_RECORD_BYTES {
        Err(EncodeError::TooLarge(out.len()))
    } else {
        Ok(out)
    }
}

/// The tier `table` gives `key`: the key's own row when `table` is
/// [`FIELDS`] (no scan), else the row `table` lists.
fn tier_in(table: &[(Key, Tier, Cap)], key: Key) -> Tier {
    if std::ptr::eq(table, FIELDS) {
        return key.tier();
    }
    table
        .iter()
        .find(|row| row.0 == key)
        .map_or(Tier::Content, |row| row.1)
}

fn push_fields(out: &mut String, f: &Fields, tier: Tier, table: &[(Key, Tier, Cap)]) {
    for (key, val) in f.iter() {
        if tier_in(table, *key) > tier {
            continue;
        }
        out.push(' ');
        out.push_str(key.word());
        out.push('=');
        push_val(out, val);
    }
}

pub(crate) fn push_val(out: &mut String, val: &Val) {
    match val {
        Val::Word(w) => out.push_str(w),
        Val::U(n) => {
            let _ = write!(out, "{n}");
        }
        Val::I(n) => {
            let _ = write!(out, "{n}");
        }
        Val::Id(id) => out.push_str(id.as_str()),
        Val::Tok(t) => pct::encode_into(out, t.as_str()),
        Val::Text(t) => pct::encode_into(out, t.as_str()),
    }
}

/// The value `raw` decodes to for `key`, or `None` when it does not have the
/// key's shape (the token is then skipped).
pub(crate) fn decode_val(key: Key, raw: &str) -> Option<Val> {
    match key.cap() {
        Cap::Word(list) => Some(Val::Word(lookup_or_unknown(list, raw))),
        Cap::U => raw.parse().ok().map(Val::U),
        Cap::I => raw.parse().ok().map(Val::I),
        Cap::Id => ShortId::new(raw).map(Val::Id),
        Cap::Tok(..) => BoundedToken::new(key, &pct::decode(raw)).map(Val::Tok),
        Cap::Text(_) => BoundedText::new(key, &pct::decode(raw)).map(Val::Text),
    }
}

/// Fold one `k=v` token that is not a header token into `f`; unknown keys and
/// malformed values are skipped.
fn decode_field(f: &mut Fields, k: &str, v: &str) {
    if let Some(key) = Key::from_word(k)
        && let Some(val) = decode_val(key, v)
    {
        let _ = f.set(key, val);
    }
}

/// One log line back to its record.
///
/// # Errors
///
/// A [`DecodeError`] for a line with no seq, an unknown operation, no node
/// id or no kind, or one longer than [`MAX_RECORD_BYTES`].
pub fn decode(line: &str) -> Result<Rec, DecodeError> {
    if line.len() > MAX_RECORD_BYTES {
        return Err(DecodeError::TooLong);
    }
    let mut toks = line.split(' ').filter(|t| !t.is_empty());
    let seq: u64 = toks
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or(DecodeError::Seq)?;
    let op = toks.next().and_then(Op::from_word).ok_or(DecodeError::Op)?;
    let node = toks.next().and_then(Node::parse).ok_or(DecodeError::Node)?;
    let k = toks.next().map(Kind::from_word).ok_or(DecodeError::Kind)?;
    let mut rec = Rec {
        seq,
        t: 0,
        op,
        node,
        par: None,
        k,
        src: Src::Unknown,
        conf: Conf::Unknown,
        by: Actor::Nobody,
        f: Fields::new(),
    };
    for tok in toks {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "par" => rec.par = Node::parse(v),
            "by" => rec.by = Actor::from_word(v),
            "src" => rec.src = Src::from_word(v),
            "conf" => rec.conf = Conf::from_word(v),
            "t" => rec.t = v.parse().unwrap_or(0),
            _ => decode_field(&mut rec.f, k, v),
        }
    }
    Ok(rec)
}

/// A decoded bus body: the record and the session's epoch and turn-class
/// counter. A body this build cannot place ([`decode_bus`]: an `op=` word or
/// a `node=` id it does not know) holds a mark of kind `unknown` on the
/// session, which no bus class holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusRec {
    /// The session's epoch (`ep=`).
    pub ep: u32,
    /// The turn-class counter (`ms=`), when the body carries one.
    pub ms: Option<u64>,
    /// The record.
    pub rec: Rec,
}

impl BusRec {
    /// THE RECORD OF A BODY THIS BUILD CANNOT PLACE ([`decode_bus`]): a mark
    /// of kind `unknown` on the session, its node and parent unread and its
    /// fields dropped. No bus class holds it ([`Rec::is_bus_class`]), so a
    /// reader folds nothing of it and still holds its `ms`
    /// (`aterm_slog::bus::BusSpace::offer`); its `t`, `by`, `src` and `conf`
    /// are the body's.
    fn unplaced(mut self) -> BusRec {
        self.rec.op = Op::Mark;
        self.rec.node = Node::SESSION;
        self.rec.par = None;
        self.rec.k = Kind::Unknown;
        self.rec.f = Fields::new();
        self
    }
}

/// A `v=2` bus body back to its record.
///
/// UNKNOWN WORDS ARE NOT LOSS (§3.4: new keys and words never change `v`): a
/// fleet runs mixed builds, and a newer writer may name an operation or a
/// node level this build does not know. A body whose `seq` reads and whose
/// `op=` word, or (its operation known) whose `node=` id, this build does
/// not know decodes to a record it cannot place — a mark of kind `unknown`
/// on the session, its fields dropped, which no bus class holds — with its
/// `ep`, `seq` and `ms`: a reader holds that `ms` and folds nothing of it,
/// as it does a record of a kind it does not know. Unknown keys are skipped
/// and unknown words of a known key read `unknown`, as [`decode`] reads them.
///
/// # Errors
///
/// [`DecodeError::Opaque`] for a body of another version (a reader keeps it
/// opaque, §3.4); [`DecodeError::Seq`], [`DecodeError::Op`],
/// [`DecodeError::Node`] or [`DecodeError::Kind`] for a body with no seq, no
/// operation, no node or no kind; [`DecodeError::TooLong`] past
/// [`MAX_RECORD_BYTES`].
pub fn decode_bus(body: &str) -> Result<BusRec, DecodeError> {
    if body.len() > MAX_RECORD_BYTES {
        return Err(DecodeError::TooLong);
    }
    let mut toks = body.split(' ').filter(|t| !t.is_empty());
    if toks.next() != Some("v=2") {
        return Err(DecodeError::Opaque);
    }
    let (mut seq, mut op, mut node, mut kind) = (None, None, None, None);
    let mut out = BusRec {
        ep: 0,
        ms: None,
        rec: Rec {
            seq: 0,
            t: 0,
            op: Op::Mark,
            node: Node::SESSION,
            par: None,
            k: Kind::Unknown,
            src: Src::Unknown,
            conf: Conf::Unknown,
            by: Actor::Nobody,
            f: Fields::new(),
        },
    };
    for tok in toks {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        match k {
            "t" => out.rec.t = v.parse().unwrap_or(0),
            "ep" => out.ep = v.parse().unwrap_or(0),
            "seq" => seq = v.parse().ok(),
            "ms" => out.ms = v.parse().ok(),
            "op" => op = Some(Op::from_word(v)),
            "node" => node = Some(Node::parse(v)),
            "k" => kind = Some(Kind::from_word(v)),
            "par" => out.rec.par = Node::parse(v),
            "by" => out.rec.by = Actor::from_word(v),
            "src" => out.rec.src = Src::from_word(v),
            "conf" => out.rec.conf = Conf::from_word(v),
            _ => decode_field(&mut out.rec.f, k, v),
        }
    }
    out.rec.seq = seq.ok_or(DecodeError::Seq)?;
    // A word this build does not know is a record it cannot place, never
    // a body that is not there (the doc).
    let Some(op) = op.ok_or(DecodeError::Op)? else {
        return Ok(out.unplaced());
    };
    out.rec.op = op;
    let Some(node) = node.ok_or(DecodeError::Node)? else {
        return Ok(out.unplaced());
    };
    out.rec.node = node;
    out.rec.k = kind.ok_or(DecodeError::Kind)?;
    Ok(out)
}
