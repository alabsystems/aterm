// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::io::{
    JournalDocumentKey, JournalEdit, decode_journal, encode_journal, encode_journal_v1,
    recover_journal_for, recover_journal_image_for,
};

const KEY: JournalDocumentKey = JournalDocumentKey(0x5eed);

fn snapshot(seq: u64, text: &str) -> JournalRecord {
    JournalRecord {
        document_raw: KEY.0,
        base_seq: Seq(seq),
        seq: Seq(seq),
        payload: JournalPayload::Snapshot(text.to_string()),
    }
}

fn delta(base: u64, seq: u64, at: usize, insert: &str) -> JournalRecord {
    JournalRecord {
        document_raw: KEY.0,
        base_seq: Seq(base),
        seq: Seq(seq),
        payload: JournalPayload::Delta(vec![JournalEdit {
            range: at..at,
            insert: insert.to_string(),
        }]),
    }
}

/// An image of `base`'s snapshot and one delta per typed string, appended one at a time as a
/// host does, with the tail after each record.
fn typed(base: &str, typed: &[&str]) -> (Vec<u8>, Vec<JournalTail>) {
    let (mut image, first) = encode_image(&[snapshot(1, base)]).unwrap();
    let mut tails = vec![first];
    let mut len = base.len();
    for (n, text) in typed.iter().enumerate() {
        let seq = n as u64 + 1;
        let record = encode_record(&delta(seq, seq + 1, len, text)).unwrap();
        let (frames, after) = frame_record(tails.last().unwrap(), &record).unwrap();
        image.extend_from_slice(&frames);
        tails.push(after);
        len += text.len();
    }
    (image, tails)
}

#[test]
fn an_appenders_tail_is_the_one_a_read_finds_and_names_the_images_bytes() {
    let (image, tails) = typed("disk", &[" one", " two", " three"]);
    let last = *tails.last().unwrap();
    assert_eq!(journal_tail(&image).unwrap(), last);
    assert_eq!(last.byte_len(), image.len() as u64);
    assert_eq!(last.records(), 4);
    assert_eq!(
        last.appended_since_snapshot(),
        last.byte_len() - tails[0].byte_len(),
        "the deltas after the snapshot"
    );
    assert_eq!(last.fingerprint(), ContentFingerprint::of(&image));
    let (at, head) = last.last_frame();
    let at = usize::try_from(at).unwrap();
    assert_eq!(&image[at..at + FRAME_HEAD], head);
    let recovered = recover_journal_for(KEY, &image).unwrap();
    assert_eq!(recovered.text, "disk one two three");
    assert_eq!(recovered.record_count, 4);
    assert_eq!(recovered.base_content, ContentFingerprint::of(b"disk"));
}

#[test]
fn a_torn_tail_recovers_to_the_last_whole_record() {
    let (image, tails) = typed("disk", &[" one", " two"]);
    let whole = usize::try_from(tails[2].byte_len()).unwrap();
    let before = usize::try_from(tails[1].byte_len()).unwrap();
    for cut in before..whole {
        let torn = &image[..cut];
        let recovered = recover_journal_for(KEY, torn).unwrap_or_else(|error| {
            panic!("cut at {cut} of {whole}: {error:?}");
        });
        assert_eq!(recovered.text, "disk one", "cut at {cut}");
        assert_eq!(recovered.record_count, 2, "cut at {cut}");
        assert_eq!(journal_tail(torn).unwrap(), tails[1], "cut at {cut}");
    }
}

#[test]
fn a_record_split_across_frames_is_whole_only_with_its_last_frame() {
    let big = "x".repeat(CHUNK_MAX + 7);
    let (base, first) = encode_image(&[snapshot(1, "disk")]).unwrap();
    let record = encode_record(&snapshot(2, &big)).unwrap();
    let (frames, after) = frame_record(&first, &record).unwrap();
    assert_eq!(after.steps, first.steps + 2, "two frames");
    let image = [base.as_slice(), &frames].concat();
    assert_eq!(recover_journal_for(KEY, &image).unwrap().text, big);
    assert_eq!(journal_tail(&image).unwrap(), after);
    assert_eq!(after.appended_since_snapshot(), 0, "a snapshot is no delta");

    let first_frame_end = base.len() + BODY_OFFSET + BODY_HEAD + CHUNK_MAX;
    for cut in [first_frame_end, first_frame_end + 1, image.len() - 1] {
        let recovered = recover_journal_for(KEY, &image[..cut]).unwrap();
        assert_eq!(recovered.text, "disk", "cut at {cut}: a part alone is torn");
        assert_eq!(journal_tail(&image[..cut]).unwrap(), first, "cut at {cut}");
    }
}

#[test]
fn a_flipped_byte_with_a_whole_record_after_it_is_refused() {
    let (image, tails) = typed("disk", &[" one", " two"]);
    let from = usize::try_from(tails[0].byte_len()).unwrap();
    let to = usize::try_from(tails[1].byte_len()).unwrap();
    for at in from..to {
        let mut damaged = image.clone();
        damaged[at] ^= 0x40;
        let refused = recover_journal_for(KEY, &damaged);
        assert!(
            matches!(
                refused,
                Err(JournalError::Fault(_) | JournalError::PrefixMismatch)
            ),
            "byte {at} of the middle record: {refused:?}"
        );
        assert!(decode_journal(&damaged).is_err(), "byte {at}");
    }
}

#[test]
fn a_frame_written_after_other_bytes_is_refused() {
    let (ours, ours_tails) = typed("disk", &[" one"]);
    let (theirs, theirs_tails) = typed("DISK", &[" one"]);
    let ours_at = usize::try_from(ours_tails[0].byte_len()).unwrap();
    let theirs_at = usize::try_from(theirs_tails[0].byte_len()).unwrap();
    let spliced = [&ours[..ours_at], &theirs[theirs_at..]].concat();
    assert_eq!(
        recover_journal_for(KEY, &spliced),
        Err(JournalError::PrefixMismatch)
    );
}

#[test]
fn an_image_names_its_document_once_and_first() {
    let (image, _) = typed("disk", &[" one"]);
    assert_eq!(
        recover_journal_for(JournalDocumentKey(KEY.0 + 1), &image),
        Err(JournalError::WrongDocument)
    );
    assert_eq!(
        decode_image(&MAGIC).map(|image| image.records),
        Err(JournalError::NoHeader)
    );
    let mut tail = JournalTail::magic();
    let mut headless = MAGIC.to_vec();
    let record = encode_record(&snapshot(1, "disk")).unwrap();
    frame(&mut tail, TAG_LAST, &record, &mut headless).unwrap();
    assert_eq!(
        decode_image(&headless).map(|image| image.records),
        Err(JournalError::NoHeader)
    );
}

#[test]
fn a_delta_spans_the_commits_an_append_in_flight_coalesced() {
    let (mut image, first) = encode_image(&[snapshot(3, "ab")]).unwrap();
    let record = encode_record(&delta(3, 9, 2, "cdef")).unwrap();
    let (frames, _) = frame_record(&first, &record).unwrap();
    image.extend_from_slice(&frames);
    let recovered = recover_journal_for(KEY, &image).unwrap();
    assert_eq!(
        (recovered.text.as_str(), recovered.durable_seq),
        ("abcdef", Seq(9))
    );
    assert_eq!(
        encode_record(&delta(9, 9, 0, "z")),
        Err(JournalError::InvalidSequence),
        "a delta moves forward"
    );
}

#[test]
fn a_format_1_journal_reads_as_the_same_records_and_text() {
    let records = vec![
        snapshot(1, "disk"),
        delta(1, 2, 4, " one"),
        delta(2, 3, 8, " λ"),
    ];
    let v1 = encode_journal_v1(&records).unwrap();
    assert_eq!(decode_journal(&v1).unwrap(), records);
    let migrated = encode_journal(&decode_journal(&v1).unwrap()).unwrap();
    assert!(migrated.starts_with(&MAGIC));
    assert_eq!(
        recover_journal_for(KEY, &migrated).unwrap(),
        recover_journal_for(KEY, &v1).unwrap()
    );
}

#[test]
fn a_republished_format_1_image_names_its_bytes_and_compacts_at_its_first_append() {
    let records = vec![snapshot(1, "disk"), snapshot(4, "disk draft")];
    let (v1, tail) = crate::io::encode_journal_image_v1(&records).unwrap();
    assert!(crate::io::journal_is_format_1(&v1));
    assert!(!crate::io::journal_is_format_1(
        &encode_journal(&records).unwrap()
    ));
    assert!(!crate::io::journal_is_format_1(b"ATDJ"));
    assert_eq!(
        crate::io::recover_journal_v1_for(KEY, &v1).unwrap(),
        recover_journal_for(KEY, &v1).unwrap(),
        "an aterm before format 2 reads it as this one does"
    );
    assert_eq!(tail.fingerprint(), ContentFingerprint::of(&v1));
    assert_eq!(tail.byte_len(), v1.len() as u64);
    assert!(!tail.appends_in_place());

    let record = encode_record(&delta(4, 5, 10, "!")).unwrap();
    assert_eq!(
        frame_record(&tail, &record),
        Err(JournalError::NoHeader),
        "no frame to follow"
    );
    let (compacted, after) = crate::io::compact_journal(KEY, &v1, &record).unwrap();
    assert!(compacted.starts_with(&MAGIC));
    assert!(after.appends_in_place());
    assert_eq!(after, journal_tail(&compacted).unwrap());
    let recovered = recover_journal_for(KEY, &compacted).unwrap();
    assert_eq!(
        (recovered.text.as_str(), recovered.durable_seq),
        ("disk draft!", Seq(5))
    );
    assert_eq!(recovered.base_content, ContentFingerprint::of(b"disk"));
}

/// Text whose bytes hold a whole empty astream frame (`¥` is C2 A5, so `¥q`, U+0001 and nine
/// NULs give magic A5 71, version 1, flags 0, length 0 and CRC 0).
const NESTED_FRAME: &str = "\u{a5}q\u{1}\0\0\0\0\0\0\0\0\0";

/// A crash cuts an append past a whole frame nested in its record's text. astream reads that
/// cut as a fault, not a torn tail (`astream_wire::steps`, "A body that holds a frame of its
/// own"), so the image is no crash's to drop; yet every record before the torn one is whole.
/// A read keeps them and names the fault beside them, and the boundary it reports ends before
/// the cut bytes, so a host keeps the image aside.
#[test]
fn a_cut_past_a_frame_nested_in_a_records_text_keeps_the_whole_records_before_it() {
    let text = format!("{NESTED_FRAME} and after");
    let (image, tails) = typed("disk", &[" one", &text]);
    let before = usize::try_from(tails[1].byte_len()).unwrap();
    // Past `¥`'s lead byte C2: the record's own frame starts A5 71 01 too.
    let nested = before
        + 1
        + image[before..]
            .windows(4)
            .position(|bytes| bytes == [0xc2, 0xa5, b'q', 1])
            .expect("the record's frame holds the nested one");
    let cut = &image[..nested + HEADER_SIZE];
    assert!(cut.len() < image.len(), "the cut is inside the record");
    assert!(
        matches!(
            scan_steps_from(&cut[MAGIC.len()..], 0).end,
            StepEnd::Fault { .. }
        ),
        "astream reads this cut as a fault"
    );
    let read = decode_image(cut).unwrap();
    assert!(read.fault.is_some(), "the fault is named");
    assert_eq!(read.records.len(), 2);
    assert_eq!(read.tail, tails[1], "the whole records end before the cut");
    let (recovered, tail, fault) = recover_journal_image_for(KEY, cut).unwrap();
    assert_eq!(recovered.text, "disk one");
    assert_eq!(tail, Some(tails[1]));
    assert!(fault.is_some());
    assert!(
        matches!(recover_journal_for(KEY, cut), Err(JournalError::Fault(_))),
        "a read that keeps no boundary still refuses it"
    );
    assert!(matches!(journal_tail(cut), Err(JournalError::Fault(_))));
    assert!(matches!(decode_journal(cut), Err(JournalError::Fault(_))));
}
