//! The broker's durable record skin: a versioned, CRC-framed message carrying its
//! subject and the producer idempotency key. Disjoint from the engine
//! `ENV_VERSION`, the cognition `COG_VERSION`, and the durable-exec `STEP_VERSION`
//! — the broker reuses the `astream_wire::Frame` codec (CRC + 16 MiB cap), not the
//! engine `Envelope` (which carries no `Subject` and no publish key).
//!
//! ONE `encode`/`from_payload` pair is used by BOTH append and recover, so a
//! restart reconstructs records and the dedup map byte-for-byte (round-trip test).

use astream_wire::{Frame, FrameError, Offset, MAX_PAYLOAD_LEN};

/// Broker record format version (2 adds the optional atomic `commit` annotation).
///
/// Every stored record's payload starts with this byte, and a log is read record by
/// record, so a log holds whatever versions wrote it. EVERY version keeps the same
/// first nine bytes — this byte, then the record's own offset as a little-endian
/// `u64` — because that prefix is how an older build tells a newer record from
/// damage.
///
/// A FORMAT CHANGE — bumping this, or changing what an existing version's stored
/// bytes mean (a hidden record's body, a durable key's derivation, a sidecar beside
/// the log) — ships, in the same change:
///
/// - a READ-THROUGH: [`BrokerRecord::from_payload`] decodes every version an astream
///   release has written up to the new one (fields the old one lacks take their
///   defaults), so every existing log still opens, while appends write the newest;
/// - a FIXTURE of the previous format: a small log written by the last release,
///   committed as bytes and never regenerated, and a test that this build opens it
///   and reads its records back as that release wrote them.
///
/// The other direction is already handled and needs nothing new: a build that meets
/// a record above its own version refuses the log by name — written by a newer
/// astream, `ErrorKind::Unsupported` — on `open` and `open_repair` alike, never as
/// corruption and never truncated. (v1 is not read: 2b84f85 dropped it with no
/// read-through, before this rule was written down.)
pub const BREC_VERSION: u8 = 2;
/// Fixed header: version(1) + seq(8) + producer_id(8) + producer_seq(8).
const HEADER: usize = 25;
/// The largest record payload the log accepts: the frame cap minus a margin, so a
/// stored record ALWAYS also fits the frame wherever it is re-framed — inside a
/// `Replicate` request (whose envelope, with the leader's log-identity trailer, is 25
/// bytes larger than the record's own), and sealed at rest on an encrypted log (a
/// 24-byte nonce and a 16-byte tag). A record at the cap could otherwise be committed
/// on a leader yet never be shippable to, or storable on, a follower.
pub const MAX_RECORD_PAYLOAD: usize = MAX_PAYLOAD_LEN - RECORD_MARGIN;
/// The larger of the two re-framing overheads above: the at-rest seal's 40 bytes.
const RECORD_MARGIN: usize = 40;
#[cfg(feature = "aead")]
const _: () = assert!(RECORD_MARGIN >= astream_aead::OVERHEAD);

/// One durable message on the broker log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerRecord {
    /// This record's own offset (dense from `Offset::ZERO`).
    pub seq: Offset,
    /// The producer's stable id (idempotency key, first half).
    pub producer_id: u64,
    /// The producer's monotone-per-message sequence (idempotency key, second half).
    pub producer_seq: u64,
    /// The validated subject string this message was published to.
    pub subject: String,
    /// The opaque message body.
    pub body: Vec<u8>,
    /// An optional consumer-group commit carried ATOMICALLY by this record. A normal
    /// publish is `None`; a transactional read-process-write (or a pure commit)
    /// stamps `Some(GroupCommit)`, so the produced output and the offset commit land
    /// in ONE durable append (one fsync) — exactly-once processing.
    pub commit: Option<GroupCommit>,
}

/// A consumer group advancing its committed offset to `upto` (the last offset it has
/// fully processed). Carried on a [`BrokerRecord`] so produce+commit is atomic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCommit {
    /// The consumer group id.
    pub group: String,
    /// The last offset the group has fully processed (resume is `upto + 1`).
    pub upto: u64,
}

/// The record format a stored payload names, when it is NEWER than this build reads,
/// judged by the prefix every version keeps (see [`BREC_VERSION`]): a version byte
/// above this build's, then — when `offset` is known — the record's own offset. Asked
/// only of a payload that passed the frame CRC, so such a record was written by a
/// newer astream, not damaged; the offset check is what keeps a payload this build
/// cannot read for another reason (an encrypted log opened without its key, whose
/// first byte is a random nonce byte) from passing as one. `None` otherwise.
pub(crate) fn newer_format(p: &[u8], offset: Option<u64>) -> Option<u8> {
    let version = *p.first()?;
    let seq = u64::from_le_bytes(p.get(1..9)?.try_into().ok()?);
    (version > BREC_VERSION && offset.is_none_or(|o| o == seq)).then_some(version)
}

impl BrokerRecord {
    /// Serialize to a CRC [`Frame`]'s bytes. A field exceeding `u32`, or a payload
    /// over [`MAX_RECORD_PAYLOAD`], is [`FrameError::TooLarge`] (never panics).
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        Frame::new(self.encode_payload()?).encode()
    }

    /// The record's serialized bytes WITHOUT the outer CRC frame — the unit the
    /// at-rest layer seals before framing (so the frame carries ciphertext). The
    /// [`MAX_RECORD_PAYLOAD`] cap is enforced here so it holds for both the plaintext
    /// and the sealed path.
    pub(crate) fn encode_payload(&self) -> Result<Vec<u8>, FrameError> {
        // Sized exactly (header, two length-prefixed fields, the commit tag and its
        // body), so a large body is copied once and never reallocated.
        let commit_len = self.commit.as_ref().map_or(0, |c| 4 + c.group.len() + 8);
        let mut p = Vec::with_capacity(
            HEADER + 4 + self.subject.len() + 4 + self.body.len() + 1 + commit_len,
        );
        p.push(BREC_VERSION);
        p.extend_from_slice(&self.seq.0.to_le_bytes());
        p.extend_from_slice(&self.producer_id.to_le_bytes());
        p.extend_from_slice(&self.producer_seq.to_le_bytes());
        let sl = u32::try_from(self.subject.len()).map_err(|_| FrameError::TooLarge)?;
        p.extend_from_slice(&sl.to_le_bytes());
        p.extend_from_slice(self.subject.as_bytes());
        let bl = u32::try_from(self.body.len()).map_err(|_| FrameError::TooLarge)?;
        p.extend_from_slice(&bl.to_le_bytes());
        p.extend_from_slice(&self.body);
        match &self.commit {
            None => p.push(0),
            Some(c) => {
                p.push(1);
                let gl = u32::try_from(c.group.len()).map_err(|_| FrameError::TooLarge)?;
                p.extend_from_slice(&gl.to_le_bytes());
                p.extend_from_slice(c.group.as_bytes());
                p.extend_from_slice(&c.upto.to_le_bytes());
            }
        }
        if p.len() > MAX_RECORD_PAYLOAD {
            return Err(FrameError::TooLarge);
        }
        Ok(p)
    }

    /// Parse from a decoded frame's payload. Every read is bounds-checked, so
    /// hostile bytes yield `None`, never a panic.
    pub fn from_payload(p: &[u8]) -> Option<BrokerRecord> {
        if p.len() < HEADER || p[0] != BREC_VERSION {
            return None;
        }
        let seq = Offset(u64::from_le_bytes(p[1..9].try_into().ok()?));
        let producer_id = u64::from_le_bytes(p[9..17].try_into().ok()?);
        let producer_seq = u64::from_le_bytes(p[17..25].try_into().ok()?);
        let mut pos = HEADER;
        let sl = u32::from_le_bytes(p.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let send = pos.checked_add(sl)?;
        let subject = String::from_utf8(p.get(pos..send)?.to_vec()).ok()?;
        pos = send;
        let bl = u32::from_le_bytes(p.get(pos..pos + 4)?.try_into().ok()?) as usize;
        pos += 4;
        let bend = pos.checked_add(bl)?;
        let body = p.get(pos..bend)?.to_vec();
        pos = bend;
        let commit = match *p.get(pos)? {
            0 => None,
            1 => {
                pos += 1;
                let gl = u32::from_le_bytes(p.get(pos..pos + 4)?.try_into().ok()?) as usize;
                pos += 4;
                let gend = pos.checked_add(gl)?;
                let group = String::from_utf8(p.get(pos..gend)?.to_vec()).ok()?;
                pos = gend;
                let upto = u64::from_le_bytes(p.get(pos..pos + 8)?.try_into().ok()?);
                Some(GroupCommit { group, upto })
            }
            _ => return None,
        };
        Some(BrokerRecord {
            seq,
            producer_id,
            producer_seq,
            subject,
            body,
            commit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_frame() {
        let r = BrokerRecord {
            seq: Offset(7),
            producer_id: 42,
            producer_seq: 3,
            subject: "/a/stream/x".to_string(),
            body: b"hello".to_vec(),
            commit: None,
        };
        let bytes = r.encode().unwrap();
        let decoded = Frame::decode(&bytes).unwrap().unwrap();
        assert_eq!(BrokerRecord::from_payload(&decoded.frame.payload), Some(r));
    }

    #[test]
    fn round_trips_with_an_atomic_commit() {
        let r = BrokerRecord {
            seq: Offset(3),
            producer_id: 1,
            producer_seq: 1,
            subject: "/a/out/y".to_string(),
            body: b"processed".to_vec(),
            commit: Some(GroupCommit {
                group: "g1".to_string(),
                upto: 9,
            }),
        };
        let bytes = r.encode().unwrap();
        let decoded = Frame::decode(&bytes).unwrap().unwrap();
        assert_eq!(BrokerRecord::from_payload(&decoded.frame.payload), Some(r));
    }

    /// A record at the record cap encodes; one byte over is `TooLarge` — and the cap
    /// leaves room for the larger `Replicate` envelope around the same record.
    #[test]
    fn record_cap_leaves_room_for_the_replicate_envelope() {
        let fixed = 1 + 8 + 8 + 8 + 4 + 4 + 1; // version, seq, ids, two lengths, commit tag
        let at_cap = BrokerRecord {
            seq: Offset(1),
            producer_id: 1,
            producer_seq: 1,
            subject: String::new(),
            body: vec![7u8; MAX_RECORD_PAYLOAD - fixed],
            commit: None,
        };
        let bytes = at_cap.encode().expect("at the record cap encodes");
        assert_eq!(bytes.len(), 12 + MAX_RECORD_PAYLOAD);
        // The largest envelope: the leader's log-identity trailer included.
        let leader = crate::LogIdentity {
            id: [1; 16],
            generation: 1,
        };
        let req = crate::proto::encode_replicate(1, 1, 1, "", &at_cap.body, None, Some(&leader));
        assert!(
            Frame::new(req).encode().is_ok(),
            "the same record inside a Replicate request still fits the frame cap"
        );
        let over = BrokerRecord {
            body: vec![7u8; MAX_RECORD_PAYLOAD - fixed + 1],
            ..at_cap
        };
        assert_eq!(over.encode(), Err(FrameError::TooLarge));
    }

    /// The payload buffer is sized exactly, with or without a commit, so encoding
    /// never grows (reallocates and copies) it.
    #[test]
    fn encode_payload_sizes_its_buffer_exactly() {
        for commit in [
            None,
            Some(GroupCommit {
                group: "g1".to_string(),
                upto: 9,
            }),
        ] {
            let r = BrokerRecord {
                seq: Offset(1),
                producer_id: 1,
                producer_seq: 1,
                subject: "/a/x".to_string(),
                body: vec![7u8; 1000],
                commit,
            };
            let p = r.encode_payload().unwrap();
            assert_eq!(p.capacity(), p.len());
        }
    }

    #[test]
    fn rejects_truncated_payload_without_panicking() {
        assert_eq!(BrokerRecord::from_payload(&[BREC_VERSION, 1, 2]), None);
        // Claims a 1000-byte subject with none present.
        let mut p = vec![BREC_VERSION];
        p.extend_from_slice(&[0u8; 24]); // seq + ids
        p.extend_from_slice(&1000u32.to_le_bytes());
        assert_eq!(BrokerRecord::from_payload(&p), None);
    }
}
