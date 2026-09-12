#![forbid(unsafe_code)]
// Load-bearing, not tidiness: the constant-time compares are pinned by tests that
// call the private `verify_diff` / `verify_attach_diff` helpers, and a rewrite of
// `verify` / `verify_attach` to compare in the entry point instead would leave the
// helper it stopped calling unused. Under `deny`, that rewrite is a build failure
// of `cargo test -p astream-cap` (the lib is compiled without `cfg(test)` for the
// doctests), so the fold cannot be dropped and leave this crate's claim green.
#![deny(dead_code)]
//! `astream-cap` — the unforgeable capability mint: the capability/ACL half of
//! "SSH" for untrusted-network use (the doctrine's "sound, unforgeable capability
//! mint") — WHO may attach where. The confidentiality/authentication half is the
//! sibling crate `astream-aead` (an XChaCha20-Poly1305 sealed wire), deliberately
//! a separate crate so each vetted crypto dependency stays isolated.
//!
//! A **capability is a signed grant**. A grant is an [`astream_wire::Filter`] —
//! the subject subtree its bearer may attach/observe/drive — optionally prefixed
//! with a *mode* and a *principal*:
//!
//! ```text
//! grant := [ ("rw" | "ro") [ "," "p=" <principal> ] ":" ] <filter>
//!   /f/F/pub/>            read-write, unbound: every capability minted before this existed
//!   ro:/f/F/fleet/>       read-only: subscribe/fetch, never publish or commit
//!   rw,p=n-a1b2c3d4:/f/F/in/*/*/n-a1b2c3d4/*
//!                         read-write, and only as producer_id_of("n-a1b2c3d4")
//! ```
//!
//! The prefix is unambiguous *because* [`Filter::new`] rejects any string without
//! a leading `/`: a prefixed grant can never parse as a filter, and a filter can
//! never carry a prefix. The broker holds a secret key, mints a capability by
//! tagging the **whole grant string** with HMAC-SHA256, and verifies a presented
//! capability by recomputing the tag and comparing it with a fold over all 32
//! bytes that has no early exit on the first differing one — so the mode and the
//! principal are exactly as unforgeable as the filter, and a bare filter still
//! mints and verifies over byte-for-byte the same message it always did.
//!
//! The **producer id** is derived by the broker from the principal the grant names
//! ([`producer_id_of`]), never chosen by a bearer. [`grants_publish`] refuses a
//! publish whose `producer_id` a bound grant does not derive, which is what closes
//! dedup-key poisoning: a co-permitted publisher pre-publishing a peer's next
//! `(producer_id, producer_seq)` so the peer's genuine record silently dedups away
//! to the attacker's offset.
//!
//! [`attach_proof`] / [`verify_attach`] are the proof of possession that keeps the
//! tag off the wire: the client answers a broker-chosen per-connection nonce with
//! `HMAC-SHA256(tag, nonce ‖ grant)`, so the tag is never readable out of a stream
//! and a captured attach cannot be replayed onto another connection.
//!
//! Crypto honesty: the MAC is **HMAC-SHA256 per RFC 2104** over the workspace's
//! already-vetted `sha2` primitive — a standard construction, NOT a hand-rolled
//! cipher or MAC. Encrypting the wire is intentionally NOT done here: that is
//! `astream-aead`'s job (over the vetted RustCrypto AEAD), wired into the broker
//! as `serve_tcp_sealed`; a guarded broker (`open_guarded`) composes the two.

use astream_wire::{Filter, Subject};
use sha2::{Digest, Sha256};

const BLOCK: usize = 64; // SHA-256 block size
const TAG: usize = 32; // SHA-256 output size

/// Domain separator for producer-id derivation, so the id of a principal is never
/// some other SHA-256 this workspace already computes over the same bytes.
const PID_DOMAIN: &[u8] = b"astream-pid\0";

/// The longest a principal's name half may be — short enough that a principal
/// cannot carry a sentence into an agent's context.
const PRINCIPAL_NAME_MAX: usize = 32;

/// The class prefixes a principal may carry: session, node, human, agent/service.
/// Reserved literal subject segments can never collide with a class-prefixed id.
const PRINCIPAL_CLASSES: [&str; 4] = ["s-", "n-", "h-", "a-"];

/// A bearer capability: the granted **grant string** plus its HMAC tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// The grant this capability seals — a bare `astream_wire` filter string (the
    /// read-write, unbound grant) or a mode/principal-prefixed one ([`Grant`]).
    /// The field keeps its name because a bare filter *is* a grant.
    pub filter: String,
    /// HMAC-SHA256(secret, grant) — the unforgeable seal, over mode, principal and
    /// filter together.
    pub tag: [u8; TAG],
}

/// What a grant authorizes: reads only, or reads and writes.
///
/// `ReadOnly` covers `Subscribe`/`Last`/`Fetch`; `ReadWrite` additionally covers
/// `Publish`, `Commit` and `Will`. A read grant is never *implied* to be a write
/// grant — the two are separate capabilities on one connection's ring, which is
/// how a member reads a broadcast subtree it must not be able to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read (subscribe/fetch/last) only — never publish, commit or leave a will.
    ReadOnly,
    /// Read and write.
    ReadWrite,
}

/// A parsed grant: the mode, the optional bound principal, and the filter half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Whether this grant may write, or only read.
    pub mode: Mode,
    /// The principal this grant binds writes to, if any. `None` is an *unbound*
    /// grant: it may publish under any producer id, so it is the god cap and the
    /// shape the fleet root keeps.
    pub principal: Option<String>,
    /// The `astream_wire` filter half — the subject subtree itself.
    pub filter: String,
}

impl Grant {
    /// Parse a grant string, or explain why it is not one.
    ///
    /// A bare filter (leading `/`) is the read-write, unbound grant: every
    /// capability minted before the prefix existed parses to exactly that, which
    /// is what keeps those capabilities valid. Otherwise the string is
    /// `<mode>[,p=<principal>]:<filter>`, split on the FIRST `:` — unambiguous
    /// because a principal may not contain one, and a filter that does is only
    /// ever reached after the prefix has been consumed.
    pub fn parse(grant: &str) -> Result<Self, String> {
        if grant.starts_with('/') {
            Filter::new(grant).map_err(|e| format!("invalid filter: {e}"))?;
            return Ok(Grant {
                mode: Mode::ReadWrite,
                principal: None,
                filter: grant.to_string(),
            });
        }
        let Some((prefix, filter)) = grant.split_once(':') else {
            return Err(format!(
                "invalid grant {grant:?}: neither a filter (leading '/') nor a \
                 \"<rw|ro>[,p=<principal>]:<filter>\" prefix"
            ));
        };
        let (mode_tok, principal) = match prefix.split_once(',') {
            None => (prefix, None),
            Some((mode_tok, rest)) => {
                let p = rest.strip_prefix("p=").ok_or_else(|| {
                    format!("invalid grant prefix {prefix:?}: expected \",p=<principal>\"")
                })?;
                if !valid_principal(p) {
                    return Err(format!(
                        "invalid principal {p:?}: expected one of {PRINCIPAL_CLASSES:?} \
                         then 1..={PRINCIPAL_NAME_MAX} of [a-z0-9-]"
                    ));
                }
                (mode_tok, Some(p.to_string()))
            }
        };
        let mode = match mode_tok {
            "rw" => Mode::ReadWrite,
            "ro" => Mode::ReadOnly,
            other => {
                return Err(format!(
                    "invalid grant mode {other:?}: expected \"rw\" or \"ro\""
                ))
            }
        };
        Filter::new(filter).map_err(|e| format!("invalid filter: {e}"))?;
        Ok(Grant {
            mode,
            principal,
            filter: filter.to_string(),
        })
    }
}

/// Whether `p` is a well-formed principal: a class prefix (`s-` session, `n-`
/// node, `h-` human, `a-` service) then `[a-z0-9-]{1,32}`.
///
/// HONEST BOUNDARY: the per-class *shape* of the name (`s-<20 hex>`, `n-<16 hex>`)
/// is deliberately not enforced here. What this rejects is anything that could
/// confuse the grant grammar or the subject grammar — a `:`, a `/`, a wildcard, a
/// control byte, an over-long or empty name — not anything about who issued the id.
fn valid_principal(p: &str) -> bool {
    let Some(name) = PRINCIPAL_CLASSES.iter().find_map(|c| p.strip_prefix(c)) else {
        return false;
    };
    !name.is_empty()
        && name.len() <= PRINCIPAL_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The mode `grant` authorizes, or `None` if it is not a grant at all.
#[must_use]
pub fn mode_of(grant: &str) -> Option<Mode> {
    Grant::parse(grant).ok().map(|g| g.mode)
}

/// The producer id the **broker** derives for `principal`:
/// `u64::from_le_bytes(SHA-256("astream-pid\0" ‖ principal)[..8])`.
///
/// Nothing a bearer types is ever a producer id — a bound grant's publish must
/// carry exactly this. 64 bits is the wire's producer-id width, so a targeted
/// collision is a 2^64 second-preimage search; the broker's binding table
/// (`pid → principal`) is the belt-and-braces on top of that, not this function.
#[must_use]
pub fn producer_id_of(principal: &str) -> u64 {
    let mut h = Sha256::new();
    h.update(PID_DOMAIN);
    h.update(principal.as_bytes());
    let digest = h.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(head)
}

/// HMAC-SHA256(key, msg) per RFC 2104, over the vetted `sha2` primitive.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; TAG] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..TAG].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    let mut out = [0u8; TAG];
    out.copy_from_slice(&outer.finalize());
    out
}

/// The folded difference between two tags: the OR of all 32 byte-wise XORs. Zero
/// exactly when they are equal.
///
/// The loop has no data-dependent exit — every one of the 32 bytes is read and
/// folded whatever the inputs are, so how long the compare runs, and what it
/// returns, say nothing about WHERE the first difference is.
///
/// The fold is not merely written here, it is *pinned on the path the security
/// check takes*. [`verify`] and [`verify_attach`] each decide by testing a
/// `*_diff` helper — `verify_diff`, `verify_attach_diff` — against zero, and
/// `verify_folds_every_byte_of_the_tag` / `verify_attach_folds_every_byte_of_the_proof`
/// assert, for every ordered pair of mismatch positions, that what those helpers
/// return carries the LATER byte's difference as well as the earlier one. So a
/// rewrite of either compare to `a == b`, or to a loop that breaks at the first
/// differing byte, fails those tests; and a rewrite that leaves the helper in place
/// and compares inside the entry point instead leaves the helper uncalled, which
/// the crate's `deny(dead_code)` turns into a build failure of the same command.
///
/// It is a structural property of the code, not a measurement — nothing here
/// observes a clock, and nothing here says what the compiler, the CPU or the cache
/// do with the loop.
fn ct_diff(a: &[u8; TAG], b: &[u8; TAG]) -> u8 {
    let mut diff = 0u8;
    for i in 0..TAG {
        diff |= a[i] ^ b[i];
    }
    diff
}

/// Mint a capability granting `grant`, sealed with `secret`. The grant is parsed —
/// its filter half validated by `Filter::new` — so an unparseable grant (a bad
/// mode, a malformed principal, an invalid filter) is rejected up front.
///
/// A bare filter mints exactly as it always did: same message, same tag.
pub fn mint(secret: &[u8], grant: &str) -> Result<Capability, String> {
    Grant::parse(grant)?;
    Ok(Capability {
        filter: grant.to_string(),
        tag: hmac_sha256(secret, grant.as_bytes()),
    })
}

/// The fold [`verify`] decides on: [`ct_diff`] between the tag `secret` would have
/// minted for this capability's grant and the tag the capability carries. Zero
/// exactly when the capability is genuine.
///
/// It exists so the no-early-exit shape can be asserted *through the function the
/// mint check calls* rather than beside it — see `ct_diff`.
fn verify_diff(secret: &[u8], cap: &Capability) -> u8 {
    ct_diff(&hmac_sha256(secret, cap.filter.as_bytes()), &cap.tag)
}

/// Verify a capability was minted with `secret` — its grant string (mode,
/// principal and filter alike) has not been tampered with. The tag compare folds
/// all 32 bytes, with no early exit on the first differing byte.
pub fn verify(secret: &[u8], cap: &Capability) -> bool {
    verify_diff(secret, cap) == 0
}

/// The proof of possession a client sends *instead of* its tag:
/// `HMAC-SHA256(tag, nonce ‖ grant)` over the broker's per-connection nonce.
///
/// The tag never leaves the client, and the proof is bound to both the nonce (so a
/// captured `Attach` frame replayed on a second connection is refused) and the
/// grant (so a proof for one grant cannot be presented for another).
#[must_use]
pub fn attach_proof(tag: &[u8; TAG], nonce: &[u8], grant: &str) -> [u8; TAG] {
    let mut msg = Vec::with_capacity(nonce.len() + grant.len());
    msg.extend_from_slice(nonce);
    msg.extend_from_slice(grant.as_bytes());
    hmac_sha256(tag, &msg)
}

/// The fold [`verify_attach`] decides on: [`ct_diff`] between the proof `secret`
/// would have expected for `grant` over `nonce` and the proof presented. `None`
/// when there is nothing to fold at all — a grant that does not parse (so no MAC
/// is trusted), or a proof that is not `TAG` bytes long, refused on the length
/// alone, which the wire has already revealed. A wrong length is an ordinary
/// mismatch, never a panic.
///
/// It exists so the no-early-exit shape can be asserted *through the function the
/// accept path calls* rather than beside it — see `ct_diff`.
fn verify_attach_diff(secret: &[u8], grant: &str, nonce: &[u8], proof: &[u8]) -> Option<u8> {
    if Grant::parse(grant).is_err() {
        return None;
    }
    let proof = <&[u8; TAG]>::try_from(proof).ok()?;
    let tag = hmac_sha256(secret, grant.as_bytes());
    Some(ct_diff(&attach_proof(&tag, nonce, grant), proof))
}

/// The broker side of the proof of possession: recompute the tag `secret` would
/// have minted for `grant`, recompute the expected proof over `nonce`, and compare
/// the two with `verify_attach_diff` — all 32 bytes folded, no early exit on the
/// first differing byte, a wrong length an ordinary mismatch.
///
/// The grant is parsed before any MAC is trusted, so a string that carries a
/// genuine tag but is not a grant authorizes nothing.
#[must_use]
pub fn verify_attach(secret: &[u8], grant: &str, nonce: &[u8], proof: &[u8]) -> bool {
    verify_attach_diff(secret, grant, nonce, proof) == Some(0)
}

/// The parsed grant of a capability that is genuine under `secret`, or `None`.
fn authentic_grant(secret: &[u8], cap: &Capability) -> Option<Grant> {
    if !verify(secret, cap) {
        return None;
    }
    Grant::parse(cap.filter.as_str()).ok()
}

/// The genuine, read-write grant of `cap` whose filter matches `subject`, if any.
/// The one place the write half of the §8.2 matrix is decided.
fn rw_matches(secret: &[u8], cap: &Capability, subject: &str) -> Option<Grant> {
    let grant = authentic_grant(secret, cap)?;
    if grant.mode != Mode::ReadWrite {
        return None;
    }
    match (Filter::new(grant.filter.as_str()), Subject::new(subject)) {
        (Ok(f), Ok(s)) if f.matches(&s) => Some(grant),
        _ => None,
    }
}

/// Whether `cap` (verified under `secret`) authorizes `subject` for a WRITE — it is
/// genuine, read-write, and its granted filter matches the subject. The ACL check
/// the broker runs before a publish, a read-process-write output, or a commit.
///
/// HONEST BOUNDARY: it does **not** check the producer binding, because it is not
/// given a producer id — [`grants_publish`] is the check that closes dedup-key
/// poisoning, and [`grants_commit`] is the same rule named for a group.
pub fn grants(secret: &[u8], cap: &Capability, subject: &str) -> bool {
    rw_matches(secret, cap, subject).is_some()
}

/// Whether `cap` authorizes publishing to `subject` **as** `producer_id`: genuine,
/// read-write, filter matches, and — when the grant names a principal — the id is
/// exactly the one the broker derives from that principal.
///
/// An unbound grant (no principal) may publish under any id: that is the god cap
/// the fleet root keeps, and the reason a mint face should require an explicit mode.
pub fn grants_publish(secret: &[u8], cap: &Capability, subject: &str, producer_id: u64) -> bool {
    match rw_matches(secret, cap, subject) {
        None => false,
        Some(grant) => match grant.principal {
            None => true,
            Some(p) => producer_id_of(&p) == producer_id,
        },
    }
}

/// Whether `cap` authorizes committing a consumer group whose NAME is `group`:
/// genuine, read-write, and the granted filter matches that name (group names are
/// subjects the capability must grant; they are never delivered).
///
/// The producer binding does not apply — a commit carries no producer id.
pub fn grants_commit(secret: &[u8], cap: &Capability, group: &str) -> bool {
    rw_matches(secret, cap, group).is_some()
}

/// Whether `cap` (verified under `secret`) authorizes SUBSCRIBING with `filter`
/// -- it is genuine AND its granted filter CONTAINS the requested filter (every
/// subject the request could match is inside the grant). The filter twin of
/// [`grants`]: the ACL check a broker runs before a capability-scoped subscribe.
///
/// Mode-agnostic on purpose: a read-only grant is a full *read* grant.
pub fn grants_filter(secret: &[u8], cap: &Capability, filter: &str) -> bool {
    let Some(grant) = authentic_grant(secret, cap) else {
        return false;
    };
    match (Filter::new(grant.filter.as_str()), Filter::new(filter)) {
        (Ok(granted), Ok(requested)) => granted.contains(&requested),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_filter_scopes_subscribe_to_the_granted_subtree() {
        let secret = b"broker-secret";
        let cap = mint(secret, "/a/stream/s1/>").unwrap();
        assert!(grants_filter(secret, &cap, "/a/stream/s1/out"));
        assert!(grants_filter(secret, &cap, "/a/stream/s1/>"));
        assert!(grants_filter(secret, &cap, "/a/stream/s1/*"));
        assert!(!grants_filter(secret, &cap, "/a/stream/s2/out"));
        assert!(!grants_filter(secret, &cap, "/a/>"));
        // a forged/widened cap (real tag, swapped filter) is rejected.
        let forged = Capability {
            filter: "/a/>".to_string(),
            tag: cap.tag,
        };
        assert!(!grants_filter(secret, &forged, "/a/x"));
    }

    #[test]
    fn rfc2104_test_vector() {
        // RFC 4231 Test Case 2: key="Jefe", data="what do ya want for nothing?".
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn valid_capability_verifies_and_grants_its_subtree() {
        let secret = b"broker-secret-key";
        let cap = mint(secret, "/a/stream/>").unwrap();
        assert!(verify(secret, &cap));
        assert!(grants(secret, &cap, "/a/stream/x"));
        assert!(grants(secret, &cap, "/a/stream/x/y"));
        // Outside the granted subtree: denied even though the cap is genuine.
        assert!(!grants(secret, &cap, "/a/inbox/x"));
    }

    #[test]
    fn forged_or_tampered_capabilities_are_rejected() {
        let secret = b"broker-secret-key";
        let cap = mint(secret, "/a/stream/>").unwrap();
        // Widen the filter but keep the old tag → tag mismatch → rejected.
        let tampered = Capability {
            filter: "/a/>".to_string(),
            tag: cap.tag,
        };
        assert!(!verify(secret, &tampered));
        assert!(!grants(secret, &tampered, "/a/inbox/x"));
        // A guessed tag is rejected.
        let forged = Capability {
            filter: "/a/>".to_string(),
            tag: [0u8; TAG],
        };
        assert!(!verify(secret, &forged));
        // The right filter under the WRONG secret is rejected.
        assert!(!verify(b"attacker-key", &cap));
    }

    #[test]
    fn a_bare_filter_is_the_read_write_unbound_grant() {
        let secret = b"broker-secret-key";
        let bare = "/a/stream/>";
        let g = Grant::parse(bare).unwrap();
        assert_eq!(g.mode, Mode::ReadWrite);
        assert_eq!(g.principal, None);
        assert_eq!(g.filter, bare);
        assert_eq!(mode_of(bare), Some(Mode::ReadWrite));

        // The tag is over the same message it always was, so a capability minted
        // before the prefix existed still verifies bit-for-bit.
        let cap = mint(secret, bare).unwrap();
        assert_eq!(cap.tag, hmac_sha256(secret, bare.as_bytes()));
        assert!(verify(secret, &cap));

        // Unbound: any producer id publishes, which is what makes it the god cap.
        assert!(grants_publish(secret, &cap, "/a/stream/x", 1));
        assert!(grants_publish(secret, &cap, "/a/stream/x", u64::MAX));
        assert!(!grants_publish(secret, &cap, "/a/inbox/x", 1));
        assert!(grants_commit(secret, &cap, "/a/stream/g1"));
    }

    #[test]
    fn a_read_only_grant_reads_and_never_writes() {
        let secret = b"broker-secret-key";
        let grant = "ro:/f/F/fleet/>";
        // The prefix is safe precisely because it cannot parse as a filter.
        assert!(Filter::new(grant).is_err());

        let cap = mint(secret, grant).unwrap();
        assert!(verify(secret, &cap));
        assert_eq!(mode_of(grant), Some(Mode::ReadOnly));
        assert_eq!(Grant::parse(grant).unwrap().filter, "/f/F/fleet/>");

        // Reads: the whole granted subtree.
        assert!(grants_filter(secret, &cap, "/f/F/fleet/h-andrew/halt"));
        assert!(grants_filter(secret, &cap, "/f/F/fleet/>"));
        assert!(!grants_filter(secret, &cap, "/f/F/pub/>"));

        // Writes: none, anywhere, including through the legacy entry point.
        assert!(!grants_publish(secret, &cap, "/f/F/fleet/h-andrew/halt", 7));
        assert!(!grants_commit(secret, &cap, "/f/F/fleet/g1"));
        assert!(!grants(secret, &cap, "/f/F/fleet/h-andrew/halt"));
    }

    #[test]
    fn a_bound_grant_publishes_only_as_the_derived_producer_id() {
        let secret = b"broker-secret-key";
        let node = "n-a1b2c3d4e5f60718";
        let grant = "rw,p=n-a1b2c3d4e5f60718:/f/F/in/*/*/n-a1b2c3d4e5f60718/*";
        let parsed = Grant::parse(grant).unwrap();
        assert_eq!(parsed.mode, Mode::ReadWrite);
        assert_eq!(parsed.principal.as_deref(), Some(node));

        let cap = mint(secret, grant).unwrap();
        let pid = producer_id_of(node);
        let subject = "/f/F/in/n-b/s-c/n-a1b2c3d4e5f60718/ask";
        assert!(grants_publish(secret, &cap, subject, pid));
        // A bearer that names any other producer id is refused: this is the
        // dedup-key poisoning hole, closed.
        assert!(!grants_publish(secret, &cap, subject, pid.wrapping_add(1)));
        assert!(!grants_publish(
            secret,
            &cap,
            subject,
            producer_id_of("h-andrew")
        ));
        assert!(!grants_publish(secret, &cap, subject, 0));
        // Still scoped by the filter, under the right id.
        assert!(!grants_publish(
            secret,
            &cap,
            "/f/F/in/n-b/s-c/h-andrew/ask",
            pid
        ));
        // The `*` kind segment: an 8-segment subject is outside the grant.
        assert!(!grants_publish(
            secret,
            &cap,
            "/f/F/in/n-b/s-c/n-a1b2c3d4e5f60718/h-andrew/answer",
            pid
        ));
    }

    #[test]
    fn producer_ids_are_domain_separated_sha256_and_distinct_per_principal() {
        // Fixed vectors: the derivation is part of the wire contract (a broker and
        // a bridge must agree on it), so it is pinned, not merely round-tripped.
        assert_eq!(
            producer_id_of("n-a1b2c3d4e5f60718"),
            2_992_018_643_546_584_750
        );
        assert_eq!(producer_id_of("h-andrew"), 9_245_449_477_876_580_480);
        assert_ne!(producer_id_of("h-andrew"), producer_id_of("h-andrea"));
        assert_ne!(producer_id_of("h-andrew"), producer_id_of("a-andrew"));

        // Independently recomputed from the stated construction, domain first.
        let mut h = Sha256::new();
        h.update(b"astream-pid\0");
        h.update(b"s-0123456789abcdef0123");
        let d = h.finalize();
        let mut head = [0u8; 8];
        head.copy_from_slice(&d[..8]);
        assert_eq!(
            producer_id_of("s-0123456789abcdef0123"),
            u64::from_le_bytes(head)
        );

        // The domain separator is load-bearing: an undomained hash differs.
        let plain = Sha256::digest(b"h-andrew");
        let mut plain_head = [0u8; 8];
        plain_head.copy_from_slice(&plain[..8]);
        assert_ne!(producer_id_of("h-andrew"), u64::from_le_bytes(plain_head));
    }

    #[test]
    fn a_widened_mode_or_a_swapped_principal_with_the_old_tag_is_rejected() {
        let secret = b"broker-secret-key";
        let ro = mint(secret, "ro:/f/F/fleet/>").unwrap();
        // Flip the mode to read-write, keep the tag: the tag covers the WHOLE
        // grant string, so the mode is as unforgeable as the filter.
        let widened = Capability {
            filter: "rw:/f/F/fleet/>".to_string(),
            tag: ro.tag,
        };
        assert!(!verify(secret, &widened));
        assert!(!grants_publish(secret, &widened, "/f/F/fleet/h-a/halt", 1));
        assert!(!grants_filter(secret, &widened, "/f/F/fleet/>"));

        let bound = mint(secret, "rw,p=h-andrew:/f/F/fleet/h-andrew/>").unwrap();
        // Swap the principal for one whose id the attacker controls.
        let swapped = Capability {
            filter: "rw,p=h-mallory:/f/F/fleet/h-andrew/>".to_string(),
            tag: bound.tag,
        };
        assert!(!verify(secret, &swapped));
        assert!(!grants_publish(
            secret,
            &swapped,
            "/f/F/fleet/h-andrew/halt",
            producer_id_of("h-mallory")
        ));
        // Drop the binding entirely to reach the unbound god-cap branch.
        let unbound = Capability {
            filter: "rw:/f/F/fleet/h-andrew/>".to_string(),
            tag: bound.tag,
        };
        assert!(!verify(secret, &unbound));
        assert!(!grants_publish(
            secret,
            &unbound,
            "/f/F/fleet/h-andrew/halt",
            9
        ));
        // Widen the filter half under a genuine prefix.
        let wide = Capability {
            filter: "rw,p=h-andrew:/f/F/>".to_string(),
            tag: bound.tag,
        };
        assert!(!verify(secret, &wide));
        // And the genuine grant under the wrong secret authorizes nothing.
        assert!(!verify(b"attacker-key", &bound));
        assert!(!grants_publish(
            b"attacker-key",
            &bound,
            "/f/F/fleet/h-andrew/halt",
            producer_id_of("h-andrew")
        ));
    }

    #[test]
    fn grant_parse_refuses_malformed_grants() {
        for bad in [
            "",                               // empty
            "not-a-grant",                    // no prefix, no leading slash
            "f/F/pub",                        // a filter without its leading slash
            "ro:",                            // empty filter half
            "ro:f/F/pub",                     // filter half without leading slash
            "ro:/f//pub",                     // empty segment
            "ro:/f/F/pub/>/x",                // `>` not last
            "ro:/f/F/pu*b",                   // partial wildcard
            "rwx:/f/F/pub/>",                 // unknown mode
            "RW:/f/F/pub/>",                  // modes are lowercase literals
            "rw,h-andrew:/f/F/pub/>",         // principal without `p=`
            "rw,p=andrew:/f/F/pub/>",         // no class prefix
            "rw,p=x-andrew:/f/F/pub/>",       // unknown class
            "rw,p=h-:/f/F/pub/>",             // empty name
            "rw,p=h-Andrew:/f/F/pub/>",       // uppercase outside [a-z0-9-]
            "rw,p=h-and_rew:/f/F/pub/>",      // underscore outside [a-z0-9-]
            "rw,p=h-and/rew:/f/F/pub/>",      // a `/` would break the subject grammar
            "rw,p=h-and:rew:/f/F/pub/>",      // a `:` in the principal
            "rw,p=h-andrew,p=h-m:/f/F/pub/>", // a second binding
        ] {
            assert!(
                Grant::parse(bad).is_err(),
                "{bad:?} must not parse as a grant"
            );
            assert!(mint(b"secret", bad).is_err(), "{bad:?} must not mint");
            assert_eq!(mode_of(bad), None, "{bad:?} has no mode");
        }
        // A 32-character name is the boundary; 33 is over it.
        let name32 = "h-".to_string() + &"a".repeat(32);
        assert!(Grant::parse(&format!("rw,p={name32}:/f/F/pub/>")).is_ok());
        let name33 = "h-".to_string() + &"a".repeat(33);
        assert!(Grant::parse(&format!("rw,p={name33}:/f/F/pub/>")).is_err());

        // A genuinely-tagged string that is not a grant authorizes nothing: the
        // parse is fail-closed, never a fall-through to "unrestricted".
        let secret = b"broker-secret-key";
        let junk = "rwx:/f/F/pub/>";
        let genuine_tag = Capability {
            filter: junk.to_string(),
            tag: hmac_sha256(secret, junk.as_bytes()),
        };
        assert!(verify(secret, &genuine_tag), "the tag itself is genuine");
        assert!(!grants(secret, &genuine_tag, "/f/F/pub/x"));
        assert!(!grants_publish(secret, &genuine_tag, "/f/F/pub/x", 1));
        assert!(!grants_commit(secret, &genuine_tag, "/f/F/pub/x"));
        assert!(!grants_filter(secret, &genuine_tag, "/f/F/pub/x"));
    }

    #[test]
    fn attach_proof_is_bound_to_the_nonce_the_tag_and_the_grant() {
        let secret = b"broker-secret-key";
        let grant = "rw,p=n-a1b2c3d4e5f60718:/f/F/pub/n-a1b2c3d4e5f60718/>";
        let cap = mint(secret, grant).unwrap();
        let nonce = [7u8; 32];
        let proof = attach_proof(&cap.tag, &nonce, grant);

        assert!(verify_attach(secret, grant, &nonce, &proof));
        // The proof is not the tag: what crosses the wire never reveals it.
        assert_ne!(proof, cap.tag);

        // A captured Attach replayed on a second connection (a fresh nonce) fails.
        let other_nonce = [8u8; 32];
        assert!(!verify_attach(secret, grant, &other_nonce, &proof));
        // ... and one flipped nonce byte is enough.
        let mut near = nonce;
        near[31] ^= 0x01;
        assert!(!verify_attach(secret, grant, &near, &proof));

        // A proof for one grant cannot be presented for another, even one the same
        // secret would happily mint.
        let sibling = "rw,p=n-a1b2c3d4e5f60718:/f/F/cur/n-a1b2c3d4e5f60718/>";
        assert!(!verify_attach(secret, sibling, &nonce, &proof));
        // Widening the grant at attach time fails too.
        assert!(!verify_attach(secret, "/f/F/>", &nonce, &proof));

        // A proof computed from a guessed tag, from another cap's tag, or under
        // the wrong secret is refused.
        let guessed = attach_proof(&[0u8; TAG], &nonce, grant);
        assert!(!verify_attach(secret, grant, &nonce, &guessed));
        let elsewhere = mint(secret, sibling).unwrap();
        assert!(!verify_attach(
            secret,
            grant,
            &nonce,
            &attach_proof(&elsewhere.tag, &nonce, grant)
        ));
        assert!(!verify_attach(b"attacker-key", grant, &nonce, &proof));

        // A truncated or over-long proof is a mismatch, never a panic.
        assert!(!verify_attach(secret, grant, &nonce, &proof[..31]));
        assert!(!verify_attach(secret, grant, &nonce, &[]));
        let mut long = proof.to_vec();
        long.push(0);
        assert!(!verify_attach(secret, grant, &nonce, &long));

        // A grant string that is not a grant is refused before the MAC is trusted.
        let junk = "rwx:/f/F/pub/>";
        let junk_tag = hmac_sha256(secret, junk.as_bytes());
        assert!(!verify_attach(
            secret,
            junk,
            &nonce,
            &attach_proof(&junk_tag, &nonce, junk)
        ));
    }

    /// The structural guard behind "no early exit on the first differing byte",
    /// on the primitive itself. For every ordered pair of mismatch positions, the
    /// fold must carry the LATER byte's difference as well as the earlier one: an
    /// implementation that returned as soon as it found a difference (or delegated
    /// to `a == b`, which is free to) would report only the difference at `i` and
    /// fail here.
    ///
    /// On its own this pins only `ct_diff`. The two tests below pin the same shape
    /// on the functions the security path actually calls.
    ///
    /// This asserts the shape of the compare, not a wall-clock timing property:
    /// nothing here reads a clock, and a source-level test could not make a
    /// deterministic timing measurement in CI anyway. What it does establish is
    /// that the result is a fold over all 32 bytes, which is what a refactor that
    /// reintroduces a data-dependent exit would break.
    #[test]
    fn ct_diff_folds_every_byte_whatever_the_mismatch() {
        let a = [0u8; TAG];
        for i in 0..TAG {
            for j in (i + 1)..TAG {
                let mut b = a;
                b[i] = 0x01; // an early difference ...
                b[j] = 0x02; // ... and a later one, in a bit the first does not set
                assert_eq!(
                    ct_diff(&a, &b),
                    0x03,
                    "byte {j} was not folded in: the compare exits at the mismatch at {i}"
                );
            }
        }
        // A difference in the last byte alone is still seen, and equal tags fold to
        // zero — the two ends the loop is easiest to get wrong at.
        let mut last = a;
        last[TAG - 1] = 0x80;
        assert_eq!(ct_diff(&a, &last), 0x80);
        assert_eq!(ct_diff(&a, &a), 0);
    }

    /// The same property at the public entry point: a proof that differs from the
    /// genuine one in any single byte — first, last, or anywhere between — is
    /// refused. Flipping the LAST byte is the case an early-exit compare would
    /// still get right, so this is the behaviour, and `ct_diff_folds_every_byte...`
    /// above is the reason it holds without reading the tail conditionally.
    #[test]
    fn verify_attach_refuses_a_proof_differing_in_any_single_byte() {
        let secret = b"broker-secret-key";
        let grant = "rw,p=n-a1b2c3d4e5f60718:/f/F/pub/n-a1b2c3d4e5f60718/>";
        let cap = mint(secret, grant).unwrap();
        let nonce = [7u8; 32];
        let proof = attach_proof(&cap.tag, &nonce, grant);
        assert!(verify_attach(secret, grant, &nonce, &proof));
        for i in 0..TAG {
            let mut near = proof;
            near[i] ^= 0x01;
            assert!(
                !verify_attach(secret, grant, &nonce, &near),
                "a proof differing only in byte {i} was accepted"
            );
        }
    }

    /// The same fold, pinned on the function the MINT check decides on. `verify` is
    /// `verify_diff(..) == 0` and nothing else, so what this asserts about
    /// `verify_diff` is what the compare behind `verify` does: for every ordered
    /// pair of tampered byte positions the returned fold carries the LATER byte's
    /// difference as well as the earlier one, which a compare that stopped at the
    /// first difference — `a == b` included — could not report.
    ///
    /// The other half of the guard is not in this function: a rewrite that leaves
    /// `verify_diff` in place and compares inside `verify` instead leaves it
    /// uncalled outside `cfg(test)`, and the crate's `deny(dead_code)` fails the
    /// build of this very command. Neither half reads a clock; this is the shape of
    /// the compare, not its timing.
    #[test]
    fn verify_folds_every_byte_of_the_tag() {
        let secret = b"broker-secret-key";
        let cap = mint(secret, "rw:/f/F/pub/>").unwrap();
        assert_eq!(verify_diff(secret, &cap), 0);
        assert!(verify(secret, &cap));
        for i in 0..TAG {
            for j in (i + 1)..TAG {
                let mut forged = cap.clone();
                forged.tag[i] ^= 0x01; // an early difference ...
                forged.tag[j] ^= 0x02; // ... and a later one, in a bit the first misses
                assert_eq!(
                    verify_diff(secret, &forged),
                    0x03,
                    "verify's compare dropped tag byte {j}: it exits at the mismatch at {i}"
                );
                assert!(!verify(secret, &forged));
            }
        }
    }

    /// The same fold, pinned on the function the ATTACH check decides on.
    /// `verify_attach` is `verify_attach_diff(..) == Some(0)` and nothing else. The
    /// `None` arms are the two cases with nothing to fold: a grant that does not
    /// parse (no MAC is trusted) and a proof of the wrong length (refused on the
    /// length alone, never a panic).
    #[test]
    fn verify_attach_folds_every_byte_of_the_proof() {
        let secret = b"broker-secret-key";
        let grant = "rw,p=n-a1b2c3d4e5f60718:/f/F/pub/n-a1b2c3d4e5f60718/>";
        let cap = mint(secret, grant).unwrap();
        let nonce = [7u8; 32];
        let proof = attach_proof(&cap.tag, &nonce, grant);
        assert_eq!(verify_attach_diff(secret, grant, &nonce, &proof), Some(0));
        assert!(verify_attach(secret, grant, &nonce, &proof));
        for i in 0..TAG {
            for j in (i + 1)..TAG {
                let mut near = proof;
                near[i] ^= 0x01;
                near[j] ^= 0x02;
                assert_eq!(
                    verify_attach_diff(secret, grant, &nonce, &near),
                    Some(0x03),
                    "the attach compare dropped proof byte {j}: it exits at the mismatch at {i}"
                );
                assert!(!verify_attach(secret, grant, &nonce, &near));
            }
        }
        // Nothing to fold: a wrong length, and a string that is not a grant.
        assert_eq!(
            verify_attach_diff(secret, grant, &nonce, &proof[..TAG - 1]),
            None
        );
        let mut long = proof.to_vec();
        long.push(0);
        assert_eq!(verify_attach_diff(secret, grant, &nonce, &long), None);
        assert_eq!(
            verify_attach_diff(secret, "rwx:/f/F/pub/>", &nonce, &proof),
            None
        );
    }
}
