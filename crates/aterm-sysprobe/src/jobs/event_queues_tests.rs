// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::jobs::{JobsReader, Knote, KnoteFilter};

fn queue(fd: i32, count: usize) -> FdReading {
    FdReading {
        fd,
        kind: FdKind::EventQueue {
            sleeping: None,
            knotes: Some(vec![
                Knote {
                    ident: 4242,
                    filter: KnoteFilter::Proc
                };
                count
            ]),
        },
    }
}

#[test]
fn zero_refused_short_and_misaligned_libproc_answers_cannot_prove_empty_scope() {
    let width = 8;
    let slots = MAX_FDS + 1;
    assert_eq!(complete_bytes(0, 0, width, slots, MAX_FDS), Some(0));
    for (bytes, error) in [(0, 1), (-1, 0), (1, 0), (7, 0), (9, 0), (8, 1)] {
        assert_eq!(complete_bytes(bytes, error, width, slots, MAX_FDS), None);
    }
    assert_eq!(
        complete_bytes((MAX_FDS * width) as i32, 0, width, slots, MAX_FDS),
        Some(MAX_FDS)
    );
    for count in [slots, slots + 1] {
        assert_eq!(
            complete_bytes((count * width) as i32, 0, width, slots, MAX_FDS),
            None
        );
    }
    assert_eq!(complete_bytes(0, 0, 0, slots, MAX_FDS), None);
    assert_eq!(complete_notes(0, 0), Some(0));
    assert_eq!(
        complete_notes(1, 0),
        Some(1),
        "EXTINFO counts notes rather than bytes"
    );
    for (count, error) in [
        (0, 1),
        (-1, 0),
        (1, 1),
        (MAX_KNOTES as i32, 0),
        (MAX_KNOTES as i32 + 1, 0),
    ] {
        assert_eq!(complete_notes(count, error), None);
    }
    let bytes = crate::jobs::darwin::KQUEUE_SIZE;
    assert!(exact_record(bytes as i32, 0, bytes));
    for (got, error) in [
        (0, 0),
        (0, 1),
        (-1, 0),
        (bytes as i32 - 1, 0),
        (bytes as i32 + 1, 0),
        (bytes as i32, 1),
    ] {
        assert!(!exact_record(got, error, bytes));
    }
    assert_eq!(
        known_queue(3, None, || panic!(
            "failed queue header admits no note read"
        )),
        None
    );
    assert_eq!(known_queue(3, Some(false), || None), None);
    assert_eq!(
        known_queue(3, Some(false), || Some(Vec::new())),
        Some(FdReading {
            fd: 3,
            kind: FdKind::EventQueue {
                sleeping: Some(false),
                knotes: Some(Vec::new())
            },
        }),
        "a genuine empty queue requires its positive complete header"
    );
}

#[test]
fn only_a_complete_queue_scope_admits_absence_and_unrelated_descriptors_spend_no_queue_budget() {
    let mut listed = vec![(3, false); MAX_FD_DETAILS + 1];
    listed.push((200, true));
    let mut read = Vec::new();
    assert_eq!(
        read_complete(&listed, false, |fd| {
            read.push(fd);
            Some(queue(fd, 1))
        }),
        Some(vec![queue(200, 1)])
    );
    assert_eq!(
        read,
        [200],
        "ordinary descriptor details cannot hide a late queue"
    );
    assert_eq!(
        read_complete(&[(3, false)], false, |_| panic!("no queue")),
        Some(Vec::new())
    );
    assert_eq!(
        read_complete(&vec![(3, false); MAX_FDS], false, |_| panic!("no queue")),
        Some(Vec::new())
    );
    let at_budget: Vec<_> = (0..MAX_FD_DETAILS).map(|fd| (fd as i32, true)).collect();
    assert_eq!(
        read_complete(&at_budget, false, |fd| Some(queue(fd, 0)))
            .unwrap()
            .len(),
        MAX_FD_DETAILS
    );
    for (listed, cut) in [
        (vec![(3, true)], true),
        (vec![(3, false); MAX_FDS + 1], false),
        (vec![(3, true); MAX_FD_DETAILS + 1], false),
    ] {
        assert_eq!(
            read_complete(&listed, cut, |_| panic!(
                "cut scope must refuse before reads"
            )),
            None
        );
    }
    assert_eq!(read_complete(&[(3, true)], false, |_| None), None);
    assert_eq!(
        read_complete(&[(3, true)], false, |_| Some(FdReading {
            fd: 3,
            kind: FdKind::Other
        })),
        None
    );
    assert_eq!(
        read_complete(&[(3, true)], false, |_| Some(FdReading {
            fd: 3,
            kind: FdKind::EventQueue {
                sleeping: None,
                knotes: None
            },
        })),
        None
    );
    assert_eq!(
        read_complete(&[(3, true)], false, |_| Some(queue(4, 1))),
        None
    );
    assert_eq!(
        read_complete(&[(3, true)], false, |fd| Some(queue(fd, MAX_KNOTES))),
        None
    );
    assert_eq!(
        read_complete(&[(3, true)], false, |fd| Some(queue(fd, MAX_KNOTES - 1))),
        Some(vec![queue(3, MAX_KNOTES - 1)])
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn an_unsupported_queue_reader_returns_unknown_instead_of_an_empty_set() {
    assert_eq!(JobsReader::new().event_queues(std::process::id()), None);
}

#[cfg(target_os = "macos")]
#[test]
fn a_real_late_kqueue_is_read_without_spending_the_ordinary_detail_budget() {
    use crate::jobs::ReadReq;
    const ALONE: &str = "ATERM_SYSPROBE_QUEUES_ALONE";
    const NAME: &str = "jobs::event_queues::tests::a_real_late_kqueue_is_read_without_spending_the_ordinary_detail_budget";
    const DONE: &str = "complete late kqueue observed";
    if std::env::var_os(ALONE).is_none() {
        let output = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(ALONE, "1"),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("the isolated descriptor fixture exits under the one-minute hang detector");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains(DONE),
            "isolated queue fixture: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let pid = std::process::id();
    let mut reader = JobsReader::new();
    assert!(reader.lanes().kqueue_detail.is_on());
    let _unrelated: Vec<_> = (0..=MAX_FD_DETAILS)
        .map(|_| std::fs::File::open("/dev/null").expect("one bounded unrelated descriptor"))
        .collect();
    let watch =
        aterm_uds::exitwatch::ExitWatch::watch(pid).expect("the actual late self-exit queue");
    let fd = watch.as_raw_fd();
    let sampled = reader
        .read(ReadReq { pid, fds: true })
        .expect("this isolated process");
    assert!(
        sampled.fds_truncated,
        "negative control: ordinary sampling remains budgeted"
    );
    assert_eq!(
        sampled
            .fds
            .unwrap()
            .iter()
            .find(|reading| reading.fd == fd)
            .map(|reading| &reading.kind),
        Some(&FdKind::Other),
        "the late queue was outside the shared detail budget"
    );
    let complete = reader
        .event_queues(pid)
        .expect("queue-only scope is complete");
    assert!(
        complete.iter().any(|reading| reading.fd == fd
            && matches!(&reading.kind,
        FdKind::EventQueue { knotes: Some(notes), .. } if notes.iter().any(|note|
            note.ident == u64::from(pid) && note.filter == KnoteFilter::Proc))),
        "the real registered Proc note must survive the unrelated detail load"
    );
    assert_eq!(
        reader.event_queues(0),
        None,
        "a missing process is not an empty queue scope"
    );
    println!("{DONE}");
}
