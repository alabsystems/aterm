// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPace over a join phrase (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.4): the suite X25519
//! with SHA-512 of one pinned draft, [`DRAFT`], whose `generator_string`, `ISK` and
//! initiator-responder transcript encodings this copies byte for byte, then the design's key
//! confirmation and bundle key, from ring.
//!
//! J, the joining Mac, is the draft's initiator A; M, the member, is its responder B.
//!
//! 1. J → M: J's nonce, on the request the person Allowed ([`Joiner::start`]).
//! 2. M → J: M's nonce ([`Phrases::begin`]). `sid = lv_cat(nonce_J, nonce_M)` is fixed from
//!    here. Neither side has a scalar or a share before it holds both nonces.
//! 3. J → M: `Y_J` ([`Joiner::on_nonce`]). M → J: `Y_M` ([`Phrases::share`]). Each is
//!    `X25519(y, g)`, where `g` is Elligator 2 of the first 32 bytes of SHA-512 over
//!    `generator_string(DSI, PRS, CI, sid)` and `PRS` is the phrase's canonical form.
//! 4. Each side computes `K = X25519(y, Y_peer)`. An all-zero `K` (a low-order peer value)
//!    aborts the run. `ISK = SHA-512(lv_cat(DSI_ISK, sid, K) || transcript_ir(Y_J, "", Y_M,
//!    ""))`.
//! 5. J → M: `T_J` ([`JoinerShared::on_share`]). M checks it before it answers `T_M`
//!    ([`Phrases::confirm`]), and J checks `T_M` before it accepts anything
//!    ([`JoinerConfirming::on_confirm`]). `T_X = HMAC-SHA256(HKDF-SHA256(ISK, "X"),
//!    transcript_ir(Y_J, "", Y_M, ""))`.
//! 6. M → J: the bundle, sealed with ChaCha20-Poly1305 under `HKDF-SHA256(ISK, "bundle")`
//!    ([`MemberSession::seal_bundle`], [`JoinerSession::open_bundle`]).
//!
//! The member runs every step through its [`Phrases`] book, which holds each phrase it shows: a
//! phrase starts at most one run, a run that fails or aborts burns it, and an expired phrase
//! admits nothing (the model `JoinPhrase`, `aterm_models_swarm::join_phrase_model`).
//!
//! Constant time: the generator's first SHA-512 block, which holds the phrase, is laid out by
//! masks ([`first_block`]), so neither the phrase's bytes nor its length steer a branch, a loop
//! or an index; Elligator 2 and X25519 are [`crate::x25519`]'s; the all-zero `K` test folds every
//! byte before its one branch; and tags are checked with ring's constant-time `hmac::verify`.
//! What does branch is public: the lengths of public values, and whether a run failed.

use crate::commons::binding::Binding;
use crate::phrase::{Canonical, Phrase, mask_eq, mask_lt};
use crate::{Error, Result, SecretBytes, x25519};
use aterm_time::Instant;
use ring::{aead, digest, hkdf, hmac};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

/// The draft revision whose encodings and test vectors this follows.
pub const DRAFT: &str = "draft-irtf-cfrg-cpace-21";

/// `G_X25519.DSI`.
const DSI: &[u8] = b"CPace255";

/// `G.DSI || "_ISK"`, the ISK's domain separator.
const DSI_ISK: &[u8] = b"CPace255_ISK";

/// SHA-512's input block, the draft's `H.s_in_bytes`.
const SHA512_BLOCK: usize = 128;

/// The longest `PRS` whose `generator_string` keeps DSI, PRS and the zero padding in the first
/// SHA-512 block: `zero_bytes(len_zpad)` is `PRS_MAX - len(PRS)` long.
const PRS_MAX: usize = SHA512_BLOCK - 2 - (1 + DSI.len());

/// The first part of `CI`, which names this protocol (§3.3.4 step 3).
const CI_LABEL: &[u8] = b"ALab join v1";

/// A nonce's length; `sid` joins J's and M's.
pub const NONCE_LEN: usize = 16;

/// A nonce J or M contributes to `sid`.
pub type Nonce = [u8; NONCE_LEN];

/// A CPace share `Y`: an X25519 u-coordinate.
pub type Share = [u8; 32];

/// A confirmation tag's length.
pub const TAG_LEN: usize = 32;

/// A confirmation tag, `T_J` or `T_M`.
pub type Tag = [u8; TAG_LEN];

/// How long a phrase shown for an Allowed request lives.
pub const ALLOW_LIFETIME: Duration = Duration::from_secs(5 * 60);

/// How long an invite's phrase lives.
pub const INVITE_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The most live phrases one member holds.
pub const MAX_LIVE: usize = 4;

/// The draft's `prepend_len`: `data` after its length in LEB128 (its Appendix A.1.1).
fn prepend_len(out: &mut Vec<u8>, data: &[u8]) {
    let mut length = data.len();
    loop {
        let low = (length & 0x7f) as u8;
        length >>= 7;
        if length == 0 {
            out.push(low);
            break;
        }
        out.push(low | 0x80);
    }
    out.extend_from_slice(data);
}

/// The draft's `lv_cat`: each part after its length (its Appendix A.1.3). The caller wipes the
/// result when a part is secret.
fn lv_cat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|part| part.len() + 2).sum());
    for part in parts {
        prepend_len(&mut out, part);
    }
    out
}

/// The draft's `transcript_ir(Ya, ADa, Yb, ADb)` (its Appendix A.3.4).
fn transcript_ir(ya: &[u8], ada: &[u8], yb: &[u8], adb: &[u8]) -> Vec<u8> {
    let mut transcript = lv_cat(&[ya, ada]);
    transcript.extend_from_slice(&lv_cat(&[yb, adb]));
    transcript
}

/// The first SHA-512 block of `generator_string(DSI, PRS, CI, sid, 128)`: `lv_cat(DSI, PRS,
/// zero_bytes(PRS_MAX - len))`, where `PRS` is `prs[..len]`. Every byte is chosen by masks over
/// all of `prs`, so `len` and the bytes steer no branch, loop or index; `prs.len()` is public.
fn first_block(prs: &[u8], len: usize) -> Zeroizing<[u8; SHA512_BLOCK]> {
    assert!(prs.len() <= PRS_MAX, "a PRS buffer past the first block");
    let mut block = Zeroizing::new([0u8; SHA512_BLOCK]);
    block[0] = DSI.len() as u8;
    block[1..=DSI.len()].copy_from_slice(DSI);
    let at = 1 + DSI.len();
    // Both lengths are below 128, so each is its own one-byte LEB128 prefix.
    block[at] = len as u8;
    let padding = (PRS_MAX - len) as u8;
    for k in 0..=PRS_MAX {
        let byte = prs.get(k).copied().unwrap_or(0);
        block[at + 1 + k] = (mask_lt(k, len) & byte) | (mask_eq(k, len) & padding);
    }
    block
}

/// The draft's `calculate_generator` for G_X25519 with SHA-512 (its §8.2): Elligator 2 of the
/// first 32 bytes of SHA-512 over `generator_string(DSI, PRS, CI, sid)`, read by
/// `decodeUCoordinate` for 255 bits. `PRS` is `prs[..len]`.
fn calculate_generator(prs: &[u8], len: usize, ci: &[u8], sid: &[u8]) -> [u8; 32] {
    let mut hash = digest::Context::new(&digest::SHA512);
    hash.update(first_block(prs, len).as_ref());
    hash.update(&lv_cat(&[ci, sid]));
    let mut u = Zeroizing::new([0u8; 32]);
    u.copy_from_slice(&hash.finish().as_ref()[..32]);
    u[31] &= 0x7f;
    x25519::elligator2(&u)
}

/// The generator for a canonical phrase.
fn generator(prs: &Canonical, ci: &ChannelId, sid: &[u8]) -> [u8; 32] {
    calculate_generator(&prs.bytes, prs.len, &ci.0, sid)
}

/// `K = X25519(y, Y_peer)`, or `None` for the all-zero `K` a low-order peer value gives (the
/// draft's `G.I`), tested over every byte before the one branch.
fn agree(scalar: &[u8; 32], peer: &Share) -> Option<Zeroizing<[u8; 32]>> {
    let k = Zeroizing::new(x25519::x25519(scalar, peer));
    (!aterm_digest::ct_eq(k.as_ref(), &[0u8; 32])).then_some(k)
}

/// The draft's `ISK`: SHA-512 over `lv_cat(DSI_ISK, sid, K) || transcript`.
fn isk(sid: &[u8], k: &[u8; 32], transcript: &[u8]) -> Zeroizing<[u8; 64]> {
    let prefix = SecretBytes::new(lv_cat(&[DSI_ISK, sid, k]));
    let mut hash = digest::Context::new(&digest::SHA512);
    hash.update(prefix.as_slice());
    hash.update(transcript);
    let mut out = Zeroizing::new([0u8; 64]);
    out.copy_from_slice(hash.finish().as_ref());
    out
}

/// A fresh random value.
fn random<const N: usize>(what: &str) -> Result<Zeroizing<[u8; N]>> {
    let mut out = Zeroizing::new([0u8; N]);
    aterm_uds::rand::fill(out.as_mut())
        .map_err(|why| Error::new(format!("no randomness for a join {what}: {why}")))?;
    Ok(out)
}

/// The draft's channel identifier `CI` for one join (§3.3.4 step 3): an `lv_cat` of
/// "ALab join v1", M's device key, J's binding, J's node key and the genesis digest. A key
/// swapped anywhere in it gives a different generator, so the run fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelId(Vec<u8>);

impl ChannelId {
    /// The `CI` of the join of the Mac whose request carries `joiner` and `joiner_node`, at the
    /// member whose device key is `member_device`, in the network of `genesis`.
    #[must_use]
    pub fn new(
        member_device: &[u8; 32],
        joiner: &Binding,
        joiner_node: &[u8; 32],
        genesis: &[u8; 32],
    ) -> Self {
        Self(lv_cat(&[
            CI_LABEL,
            member_device,
            &joiner.bytes(),
            joiner_node,
            genesis,
        ]))
    }
}

/// The keys one run derives from its `ISK`, and the transcript the tags cover.
struct Keys {
    joiner: hmac::Key,
    member: hmac::Key,
    bundle: aead::LessSafeKey,
    transcript: Vec<u8>,
}

impl Keys {
    /// The keys of a run whose shares were `joiner_share` then `member_share`.
    fn derive(sid: &[u8], k: &[u8; 32], joiner_share: &Share, member_share: &Share) -> Self {
        let transcript = transcript_ir(joiner_share, b"", member_share, b"");
        let isk = isk(sid, k, &transcript);
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(isk.as_ref());
        let mac = |label: &[u8]| {
            hmac::Key::from(
                prk.expand(&[label], hmac::HMAC_SHA256)
                    .expect("32 bytes is within HKDF-SHA-256's output bound"),
            )
        };
        let info = [b"bundle".as_slice()];
        let bundle = prk
            .expand(&info, &aead::CHACHA20_POLY1305)
            .expect("32 bytes is within HKDF-SHA-256's output bound");
        Self {
            joiner: mac(b"J"),
            member: mac(b"M"),
            bundle: aead::LessSafeKey::new(aead::UnboundKey::from(bundle)),
            transcript,
        }
    }

    fn tag(key: &hmac::Key, transcript: &[u8]) -> Tag {
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(hmac::sign(key, transcript).as_ref());
        tag
    }
}

/// The one nonce a bundle key ever seals under: each run seals one bundle.
fn bundle_nonce() -> aead::Nonce {
    aead::Nonce::assume_unique_for_key([0u8; 12])
}

/// J before M's nonce: it has sent its own and holds the phrase the person typed.
pub struct Joiner {
    prs: Canonical,
    ci: ChannelId,
    nonce: Nonce,
}

impl Joiner {
    /// J's first message: a fresh nonce, sent on its request. `ci` is what J's own request makes.
    /// No share exists yet, because `sid` is not fixed until M's nonce arrives.
    ///
    /// # Errors
    /// When the system's random source fails.
    pub fn start(phrase: &Phrase, ci: ChannelId) -> Result<(Self, Nonce)> {
        let nonce = *random::<NONCE_LEN>("nonce")?;
        Ok(Self::start_with(phrase, ci, nonce))
    }

    fn start_with(phrase: &Phrase, ci: ChannelId, nonce: Nonce) -> (Self, Nonce) {
        let joiner = Self {
            prs: phrase.canonical(),
            ci,
            nonce,
        };
        (joiner, nonce)
    }

    /// M's nonce fixes `sid`, and only now does J make its scalar and its share `Y_J`.
    ///
    /// # Errors
    /// When the system's random source fails.
    pub fn on_nonce(self, member_nonce: &Nonce) -> Result<(JoinerShared, Share)> {
        let scalar = random::<32>("scalar")?;
        Ok(self.on_nonce_with(member_nonce, scalar))
    }

    fn on_nonce_with(
        self,
        member_nonce: &Nonce,
        scalar: Zeroizing<[u8; 32]>,
    ) -> (JoinerShared, Share) {
        let sid = lv_cat(&[&self.nonce, member_nonce]);
        let mut g = generator(&self.prs, &self.ci, &sid);
        let share = x25519::x25519(&scalar, &g);
        g.zeroize();
        (JoinerShared { sid, scalar, share }, share)
    }
}

/// J after sending `Y_J`.
pub struct JoinerShared {
    sid: Vec<u8>,
    scalar: Zeroizing<[u8; 32]>,
    share: Share,
}

impl JoinerShared {
    /// M's share gives `K`, `ISK` and J's tag `T_J`, which J sends.
    ///
    /// # Errors
    /// When `Y_M` is a low-order value (an all-zero `K`): the run aborts.
    pub fn on_share(self, member_share: &Share) -> Result<(JoinerConfirming, Tag)> {
        let k = agree(&self.scalar, member_share).ok_or_else(low_order)?;
        let keys = Keys::derive(&self.sid, &k, &self.share, member_share);
        let tag = Keys::tag(&keys.joiner, &keys.transcript);
        Ok((JoinerConfirming { keys }, tag))
    }
}

/// J after sending `T_J`, waiting for `T_M`.
pub struct JoinerConfirming {
    keys: Keys,
}

impl JoinerConfirming {
    /// M's tag. J accepts nothing from M before this matches.
    ///
    /// # Errors
    /// When `T_M` does not match: M did not hold the phrase J typed, or the run was tampered
    /// with.
    pub fn on_confirm(self, member_tag: &Tag) -> Result<JoinerSession> {
        hmac::verify(&self.keys.member, &self.keys.transcript, member_tag)
            .map_err(|_| wrong_phrase())?;
        Ok(JoinerSession {
            bundle: self.keys.bundle,
        })
    }
}

/// J once both tags matched: it opens M's one bundle.
pub struct JoinerSession {
    bundle: aead::LessSafeKey,
}

impl JoinerSession {
    /// The bundle M sealed in this run.
    ///
    /// # Errors
    /// When the sealed bytes were not sealed under this run's bundle key, or were changed.
    pub fn open_bundle(self, sealed: &[u8]) -> Result<SecretBytes> {
        let mut plain = sealed.to_vec();
        let opened = self
            .bundle
            .open_in_place(bundle_nonce(), aead::Aad::empty(), &mut plain)
            .map(|opened| opened.len());
        match opened {
            Ok(len) => {
                plain.truncate(len);
                Ok(SecretBytes::new(plain))
            }
            Err(_) => {
                plain.zeroize();
                Err(Error::new(
                    "the join bundle did not open under this run's key",
                ))
            }
        }
    }
}

/// M once it admitted: it seals one bundle for J.
pub struct MemberSession {
    bundle: aead::LessSafeKey,
}

impl MemberSession {
    /// `plain` sealed for J under this run's bundle key. The session ends here, so the key seals
    /// once.
    ///
    /// # Errors
    /// When the bundle is too large to seal.
    pub fn seal_bundle(self, plain: &[u8]) -> Result<Vec<u8>> {
        let mut sealed = plain.to_vec();
        if self
            .bundle
            .seal_in_place_append_tag(bundle_nonce(), aead::Aad::empty(), &mut sealed)
            .is_err()
        {
            sealed.zeroize();
            return Err(Error::new("the join bundle is too large to seal"));
        }
        Ok(sealed)
    }
}

fn low_order() -> Error {
    Error::new("the other Mac sent a low-order value, so this join run stopped")
}

fn wrong_phrase() -> Error {
    Error::new("That phrase was wrong or another Mac tried it. Allow again for a new one.")
}

/// What a phrase in a member's [`Phrases`] book is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhraseState {
    /// Shown, and no run has started with it.
    Live,
    /// One run started with it, so no other can.
    Running,
    /// A run confirmed it, and it admitted that run's Mac.
    Admitted,
    /// A run with it failed, aborted or confirmed after it expired, or it was cancelled before
    /// any run (its row re-allowed, ignored or closed, or its sheet never opened): it admits no
    /// one.
    Burned,
}

/// One phrase's handle in its member's book, unique within the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhraseId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct Slot {
    id: PhraseId,
    phrase: Phrase,
    bound: Option<ChannelId>,
    expires: Instant,
    state: PhraseState,
}

/// M's side of a run after [`Phrases::begin`], waiting for `Y_J`.
pub struct MemberRun {
    id: PhraseId,
    sid: Vec<u8>,
    scalar: Zeroizing<[u8; 32]>,
    share: Share,
}

/// M's side of a run after [`Phrases::share`], waiting for `T_J`.
pub struct MemberConfirming {
    id: PhraseId,
    keys: Keys,
}

/// The phrases one member shows (§3.3.4 "Limits on phrases"): at most [`MAX_LIVE`] that can
/// still admit, live or running, each one-time. A phrase starts at most one run; that run admits only if it confirms while
/// the phrase lives, and any other end burns it. A phrase shown for an Allowed request starts
/// a run only for that request's `CI`.
#[derive(Default)]
pub struct Phrases {
    slots: Vec<Slot>,
}

impl Phrases {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow first: a new phrase for the request a person Allowed, whose `CI` is `ci`. It lives
    /// [`ALLOW_LIFETIME`].
    ///
    /// # Errors
    /// When this member already holds [`MAX_LIVE`] live phrases, or randomness fails.
    pub fn allow(&mut self, ci: ChannelId, now: Instant) -> Result<PhraseId> {
        self.issue(Phrase::generate()?, Some(ci), ALLOW_LIFETIME, now)
    }

    /// Phrase first: a new phrase for whichever Mac uses it first. It lives
    /// [`INVITE_LIFETIME`].
    ///
    /// # Errors
    /// When this member already holds [`MAX_LIVE`] live phrases, or randomness fails.
    pub fn invite(&mut self, now: Instant) -> Result<PhraseId> {
        self.issue(Phrase::generate()?, None, INVITE_LIFETIME, now)
    }

    fn issue(
        &mut self,
        phrase: Phrase,
        bound: Option<ChannelId>,
        lifetime: Duration,
        now: Instant,
    ) -> Result<PhraseId> {
        // A spent or expired phrase admits no one and gives up its place; a running one keeps
        // its place until it expires, so a run in flight is never cut off.
        self.slots.retain(|slot| {
            matches!(slot.state, PhraseState::Live | PhraseState::Running) && now < slot.expires
        });
        if self.slots.len() >= MAX_LIVE {
            return Err(Error::new(format!(
                "this Mac already shows {MAX_LIVE} join phrases; use or let one expire first"
            )));
        }
        let id = PhraseId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        self.slots.push(Slot {
            id,
            phrase,
            bound,
            expires: now + lifetime,
            state: PhraseState::Live,
        });
        Ok(id)
    }

    /// The live phrase's three words, for the member's own sheet to draw, and nothing else.
    #[must_use]
    pub fn words(&self, id: PhraseId, now: Instant) -> Option<[&'static str; 3]> {
        self.slot(id)
            .filter(|slot| slot.state == PhraseState::Live && now < slot.expires)
            .map(|slot| slot.phrase.words())
    }

    /// What the phrase is doing, while the book still holds it.
    #[must_use]
    pub fn state(&self, id: PhraseId) -> Option<PhraseState> {
        self.slot(id).map(|slot| slot.state)
    }

    /// When the phrase stops admitting.
    #[must_use]
    pub fn expires(&self, id: PhraseId) -> Option<Instant> {
        self.slot(id).map(|slot| slot.expires)
    }

    fn slot(&self, id: PhraseId) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id == id)
    }

    fn slot_mut(&mut self, id: PhraseId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|slot| slot.id == id)
    }

    /// M's side of a run with phrase `id`, for the joiner whose request makes `ci`, on J's
    /// nonce. M's nonce is returned for J; `sid` is fixed from here. The phrase is running from
    /// here on: no second run can start with it, and only [`Phrases::confirm`] admits.
    ///
    /// # Errors
    /// When the book holds no such phrase, the phrase is spent or expired, the phrase was shown
    /// for another request's `CI` (the phrase stays live: no guess was made), or randomness
    /// fails.
    pub fn begin(
        &mut self,
        id: PhraseId,
        ci: &ChannelId,
        joiner_nonce: &Nonce,
        now: Instant,
    ) -> Result<(MemberRun, Nonce)> {
        let nonce = *random::<NONCE_LEN>("nonce")?;
        let scalar = random::<32>("scalar")?;
        self.begin_with(id, ci, joiner_nonce, nonce, scalar, now)
    }

    fn begin_with(
        &mut self,
        id: PhraseId,
        ci: &ChannelId,
        joiner_nonce: &Nonce,
        nonce: Nonce,
        scalar: Zeroizing<[u8; 32]>,
        now: Instant,
    ) -> Result<(MemberRun, Nonce)> {
        let slot = self
            .slot_mut(id)
            .ok_or_else(|| Error::new("this Mac shows no such join phrase"))?;
        if slot.state != PhraseState::Live {
            return Err(Error::new(
                "that join phrase was already used; allow again for a new one",
            ));
        }
        if now >= slot.expires {
            return Err(Error::new(
                "that join phrase expired; allow again for a new one",
            ));
        }
        if slot.bound.as_ref().is_some_and(|bound| bound != ci) {
            return Err(Error::new(
                "that join phrase was shown for another Mac's request",
            ));
        }
        let sid = lv_cat(&[joiner_nonce, &nonce]);
        let mut g = generator(&slot.phrase.canonical(), ci, &sid);
        let share = x25519::x25519(&scalar, &g);
        g.zeroize();
        slot.state = PhraseState::Running;
        Ok((
            MemberRun {
                id,
                sid,
                scalar,
                share,
            },
            nonce,
        ))
    }

    /// The running phrase's slot.
    fn running(&mut self, id: PhraseId) -> Result<&mut Slot> {
        self.slot_mut(id)
            .filter(|slot| slot.state == PhraseState::Running)
            .ok_or_else(|| Error::new("that join run is over"))
    }

    /// J's share `Y_J`, and M's `Y_M` in answer.
    ///
    /// # Errors
    /// When the run is over, or `Y_J` is a low-order value (an all-zero `K`), which aborts the
    /// run and burns the phrase.
    pub fn share(
        &mut self,
        run: MemberRun,
        joiner_share: &Share,
    ) -> Result<(MemberConfirming, Share)> {
        let slot = self.running(run.id)?;
        let Some(k) = agree(&run.scalar, joiner_share) else {
            slot.state = PhraseState::Burned;
            return Err(low_order());
        };
        let keys = Keys::derive(&run.sid, &k, joiner_share, &run.share);
        Ok((MemberConfirming { id: run.id, keys }, run.share))
    }

    /// J's tag `T_J`, checked before M signs or sends anything. It admits only when it matches
    /// while the phrase lives: the phrase is then admitted, and `T_M` returns with the session
    /// that seals J's bundle. Any other answer burns the phrase.
    ///
    /// # Errors
    /// When the run is over, `T_J` does not match (a wrong phrase, or a key swapped in `CI`),
    /// or the phrase expired.
    pub fn confirm(
        &mut self,
        run: MemberConfirming,
        joiner_tag: &Tag,
        now: Instant,
    ) -> Result<(MemberSession, Tag)> {
        let slot = self.running(run.id)?;
        let matched = hmac::verify(&run.keys.joiner, &run.keys.transcript, joiner_tag).is_ok();
        let live = now < slot.expires;
        if !(matched && live) {
            slot.state = PhraseState::Burned;
            return Err(if matched {
                Error::new("that join phrase expired before the run confirmed it")
            } else {
                wrong_phrase()
            });
        }
        slot.state = PhraseState::Admitted;
        let tag = Keys::tag(&run.keys.member, &run.keys.transcript);
        Ok((
            MemberSession {
                bundle: run.keys.bundle,
            },
            tag,
        ))
    }

    /// Burn a phrase whose run ended without delivering a bundle: a failed possession proof, a
    /// dropped connection, or a timeout.
    pub fn burn(&mut self, id: PhraseId) {
        if let Some(slot) = self
            .slot_mut(id)
            .filter(|slot| matches!(slot.state, PhraseState::Running | PhraseState::Admitted))
        {
            slot.state = PhraseState::Burned;
        }
    }

    /// Cancel a live phrase whose row was re-allowed, ignored, or closed before a run began, or
    /// whose sheet did not open, so no person saw it.
    pub fn cancel(&mut self, id: PhraseId) {
        if let Some(slot) = self
            .slot_mut(id)
            .filter(|slot| slot.state == PhraseState::Live)
        {
            slot.state = PhraseState::Burned;
        }
    }
}

#[cfg(test)]
mod tests;
