// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::store::{CLAIM_LEN, CONFLICT_SUBJECT, SUBJECT, group};
use astream_broker::BrokerRecord;
use std::collections::BTreeSet;

fn records(count: usize) -> Vec<BrokerRecord> {
    (0..count)
        .map(|index| {
            let mut body = (index as u64 + 1).to_le_bytes().to_vec();
            body.extend_from_slice(&[index as u8; 20]);
            BrokerRecord {
                seq: Offset(index as u64),
                producer_id: 1,
                producer_seq: index as u64 + 1,
                subject: if index % 2 == 0 {
                    SUBJECT
                } else {
                    CONFLICT_SUBJECT
                }
                .into(),
                body,
                commit: None,
            }
        })
        .collect()
}

/// The pre-optimization observer: admit every already identified link in order,
/// then preserve an identification refusal before checking the final boundary.
fn serial(links: &[Linked], refused: Option<&Error>, boundary: Boundary) -> Result<usize> {
    let mut claim = Claim::new(boundary);
    for link in links {
        claim.admit(link.through)?;
    }
    if let Some(refused) = refused {
        return Err(Error::new(&refused.0));
    }
    claim.finish()?;
    usize::try_from(boundary.acknowledged().count)
        .map_err(|_| Error::new("table witness count exceeds this reader"))
}

fn flipped(witness: Witness) -> Witness {
    let mut bytes = witness.bytes();
    bytes[8] ^= 1;
    Witness::decode(&bytes).unwrap()
}

/// Build a boundary through the actual independent permit codec and custody
/// recheck. Tests never manufacture an unchecked Boundary or mutate its fields.
fn boundary(dir: &Path, acknowledged: Witness, end: Option<Witness>) -> Boundary {
    let mut claim = [0; CLAIM_LEN];
    claim[4] = b'1';
    claim[WITNESS_AT..].copy_from_slice(&acknowledged.bytes());
    aterm_journal::write_private_durable(dir, "claim", &claim).unwrap();
    if let Some(end) = end {
        group::prepare(dir, "claim", &claim, acknowledged, end).unwrap();
    } else {
        let path = dir.join(group::name("claim"));
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    group::read(dir, "claim", &claim).unwrap()
}

fn answer(result: Result<usize>) -> std::result::Result<usize, String> {
    result.map_err(|error| error.to_string())
}

#[test]
fn endpoint_checks_match_serial_custody_validation_with_exact_group_tails() {
    const RECORDS: usize = 6;
    let clean = records(RECORDS);
    let mut full = Vec::new();
    identify_onto(&mut full, clean.clone()).unwrap();
    let chain: Vec<_> = std::iter::once(Witness::default())
        .chain(full.iter().map(|link| link.through))
        .collect();
    let mut journals = vec![clean.clone()];
    for at in 0..RECORDS {
        for damage in 0..3 {
            let mut changed = clean.clone();
            match damage {
                0 => changed[at].subject = "foreign".into(),
                1 => changed[at].body.truncate(7),
                _ => changed[at].seq = Offset(at as u64 + 1),
            }
            journals.push(changed);
        }
    }
    // Every committed prefix, including an empty checkpoint, exercises complete,
    // incomplete and excessive tails under both scalar and exact group bounds.
    journals.extend((0..RECORDS).map(|count| clean[..count].to_vec()));
    let root = aterm_tempfile::tempdir().unwrap();
    let mut refusals = BTreeSet::new();
    let mut accepted = BTreeSet::new();
    for (count, acknowledged) in chain.iter().copied().enumerate() {
        let claims = if count == 0 {
            vec![acknowledged]
        } else {
            vec![acknowledged, flipped(acknowledged)]
        };
        for acknowledged in claims {
            let mut ends = vec![None];
            for end in chain.iter().copied().skip(count + 1) {
                ends.extend([Some(end), Some(flipped(end))]);
            }
            for end in ends {
                let boundary = boundary(root.path(), acknowledged, end);
                for records in &journals {
                    let mut links = Vec::new();
                    let refused = identify_onto(&mut links, records.clone()).err();
                    let expected = answer(serial(&links, refused.as_ref(), boundary));
                    let actual = answer(witnessed_count(&links, refused, boundary));
                    assert_eq!(actual, expected, "prefix {count}, tail {}", links.len());
                    match actual {
                        Ok(count) => {
                            accepted.insert(count);
                        }
                        Err(error) => {
                            refusals.insert(error);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(accepted, (0..=RECORDS).collect::<BTreeSet<_>>());
    for refusal in [
        "acknowledged table history differs from custody witness",
        "table checkpoint differs from its exact group permission",
        "table history lost acknowledged custody records",
        "table log exceeds its independent checkpoint group allowance",
        "table log exceeds its single unwitnessed append allowance",
        "foreign table journal record",
        "table custody witness is not contiguous",
    ] {
        assert!(refusals.contains(refusal), "unexercised refusal: {refusal}");
    }
}
