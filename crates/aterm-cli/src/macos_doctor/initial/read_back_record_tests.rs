// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::macos_setup::{CHANGED_SINCE, Setting};

fn reading(setting: Setting, standing: Standing, note: Option<&str>) -> Observation {
    Observation {
        standing,
        now: Some("as read".into()),
        note: note.map(str::to_owned),
        ..Observation::unknown(Subject::Setting(setting))
    }
    .stamped(1_700_000_000)
}

/// A launch keeps what it read back settled of the private rows, and the next launch reads
/// it back exactly, its time included, under the same revision while the journal holds no
/// change written since each row's read started. Anything else is no reading: a row that
/// reads otherwise, an undated one, a row read in the background (the vault), a newer plan,
/// a change some pass journaled since (before it made it), a rotated or replaced journal, an
/// other format, or a torn record. A failure line journaled since changes nothing. Read of
/// the real file.
#[test]
fn the_last_read_back_holds_only_under_its_revision_while_nothing_changed() {
    use std::io::Write as _;
    let dock = reading(Setting::Dock, Standing::InPlace, None);
    let caps = reading(Setting::CapsLock, Standing::Differs, Some(CHANGED_SINCE));
    let trackpad = reading(Setting::TrackpadScrolling, Standing::AtLogin, None);
    let spotlight = reading(Setting::Spotlight, Standing::InPlace, None);
    let drifted = reading(Setting::Desktop, Standing::Differs, None);
    let undated = Observation {
        standing: Standing::InPlace,
        ..Observation::unknown(Subject::Setting(Setting::Widgets))
    };
    let vault = reading(Setting::Secrets, Standing::InPlace, None);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = scratch.path().join("journal.jsonl");
    let append = |line: &str| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    };
    append("{\"time\":1,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    let fence = Fence(JournalMark::of(&journal).unwrap());
    let fenced = |rows: &[Observation]| -> Vec<(Observation, Fence)> {
        rows.iter().cloned().map(|seen| (seen, fence)).collect()
    };
    let written = record(
        7,
        &fenced(&[
            dock.clone(),
            caps.clone(),
            trackpad.clone(),
            spotlight.clone(),
            drifted,
            undated,
            vault,
        ]),
        &journal,
    );
    let kept = fenced(&[dock, caps, trackpad, spotlight]);
    assert_eq!(parse(&written, 7, &journal), kept);
    assert!(
        parse(&written, 8, &journal).is_empty(),
        "another plan's revision"
    );
    append("{\"time\":2,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert_eq!(
        parse(&written, 7, &journal),
        kept,
        "a failure changed nothing"
    );
    for format in [READ_BACK_FORMAT - 1, READ_BACK_FORMAT + 1] {
        let other = json!({
            "format": format,
            "revision": 7,
            "rows": written["rows"].clone(),
        });
        assert!(parse(&other, 7, &journal).is_empty(), "format {format}");
    }
    assert!(
        parse(&json!({"rows": 1}), 7, &journal).is_empty(),
        "a torn record"
    );
    append("{\"time\":3,\"id\":\"init.dock@alice\",\"from\":\"b\",\"to\":\"a\"}\n");
    assert!(
        parse(&written, 7, &journal).is_empty(),
        "a change journaled since"
    );

    // A rotation moves the journal aside and starts a new file: no promise.
    let rotated = scratch.path().join("journal.1.jsonl");
    let now = Fence(JournalMark::of(&journal).unwrap());
    let kept: Vec<_> = kept.into_iter().map(|(seen, _)| (seen, now)).collect();
    let fresh = record(7, &kept, &journal);
    assert_eq!(parse(&fresh, 7, &journal), kept);
    std::fs::rename(&journal, &rotated).unwrap();
    append("{\"time\":4,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert!(parse(&fresh, 7, &journal).is_empty(), "a rotated journal");

    // With no journal then, a record holds while nothing but failures was journaled since.
    std::fs::remove_file(&journal).unwrap();
    let kept: Vec<_> = kept
        .into_iter()
        .map(|(seen, _)| (seen, Fence(None)))
        .collect();
    let none = record(7, &kept, &journal);
    assert_eq!(parse(&none, 7, &journal), kept);
    append("{\"time\":5,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    assert_eq!(parse(&none, 7, &journal), kept);
    append("{\"time\":6,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    assert!(parse(&none, 7, &journal).is_empty());
}

/// Review finding 3: each row is fenced by the journal as its read started, never as the
/// record is written, so a change any aterm process journals between a row's read and the
/// write (a terminal `aterm init` setting the Dock while this window held its reading) keeps
/// that reading out of the record; a row read after the change, fenced after it, is kept.
/// Before, the record took the journal as it was written, after the change, and the next
/// launch carried the earlier reading as set up.
/// NEGATIVE CONTROL: with no change between read and write, the row is kept and read back.
#[test]
fn a_change_between_a_rows_read_and_the_write_keeps_that_reading_out() {
    use std::io::Write as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = scratch.path().join("journal.jsonl");
    let append = |line: &str| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    };
    append("{\"time\":1,\"id\":\"repos\",\"failed\":\"offline\"}\n");
    let dock = reading(Setting::Dock, Standing::InPlace, None);
    let caps = reading(Setting::CapsLock, Standing::InPlace, None);
    let dock_read = Fence(JournalMark::of(&journal).unwrap());
    let unchanged = record(7, &[(dock.clone(), dock_read)], &journal);
    assert_eq!(
        parse(&unchanged, 7, &journal),
        [(dock.clone(), dock_read)],
        "CONTROL: nothing changed between read and write"
    );
    append("{\"time\":2,\"id\":\"init.dock@alice\",\"from\":\"a\",\"to\":\"b\"}\n");
    let caps_read = Fence(JournalMark::of(&journal).unwrap());
    let written = record(7, &[(dock, dock_read), (caps.clone(), caps_read)], &journal);
    assert_eq!(
        parse(&written, 7, &journal),
        [(caps, caps_read)],
        "the Dock read before the change is not kept; Caps Lock read after it is"
    );
}
