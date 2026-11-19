// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn named_self_cpu_counters_keep_units_precision_and_wide_intermediates() {
    let (thread, split) = thread_counter(1_234_000, 2_345_000).unwrap();
    assert_eq!(thread.ns, 3_579_000);
    assert_eq!(thread.precision_ns, 2_000);
    assert_eq!(
        split,
        CpuSplit {
            user_ns: 1_234_000,
            system_ns: 2_345_000
        },
        "the parts keep their own modes, not the sum twice or swapped"
    );
    // A 24 MHz kernel counter, including on a Rosetta process whose Mach timebase reads 1:1.
    let process = process_counter(2, 1, 125, 3).unwrap();
    assert_eq!((process.ns, process.precision_ns), (125, 85));
    // Sum before conversion must be wide: both components may fit when their u64 sum does not.
    assert_eq!(
        process_counter(u64::MAX, u64::MAX, 1, 2).unwrap().ns,
        u64::MAX
    );
    assert_eq!(process_counter(1, 2, 0, 1), Err(ReadError::Timebase));
    assert_eq!(process_counter(1, 2, 1, 0), Err(ReadError::Timebase));
    assert_eq!(thread_counter(u64::MAX, 1), Err(ReadError::CounterOverflow));
    assert_eq!(
        process_counter(u64::MAX, 1, 125, 3),
        Err(ReadError::CounterOverflow)
    );
}

#[test]
fn named_self_cpu_names_require_a_whole_valid_bounded_observation() {
    let named = |name: &[u8]| {
        let mut bytes = [0; 64];
        bytes[..name.len()].copy_from_slice(name);
        bytes
    };
    assert_eq!(
        thread_name(&named(b"aterm-vitals")),
        Some("aterm-vitals".into())
    );
    assert_eq!(
        thread_name(&named("aterm-µ".as_bytes())),
        Some("aterm-µ".into())
    );
    assert_eq!(thread_name(&named(&[b'a'; 62])).unwrap().len(), 62);
    assert_eq!(thread_name(&named(&[b'a'; 63])), None);
    for bytes in [
        named(b""),
        named(b"aterm-\xff"),
        named(b"aterm-\n"),
        named(b"aterm-\x1b"),
        [b'a'; 64],
    ] {
        assert_eq!(thread_name(&bytes), None);
    }
}

/// The epoch's lead is 73 microseconds, not nanoseconds: on Windows std reads two instants less
/// than the performance counter's resolution apart as one (its `checked_duration_since` answers
/// zero, not `None`, for a lead under it), so a lead of 73 ns read as no lead there.
#[test]
fn named_self_cpu_read_brackets_reject_a_future_epoch_without_fabricating_zero() {
    let epoch = Instant::now();
    let later = epoch + std::time::Duration::from_micros(73);
    assert_eq!(stamp(epoch, epoch), Ok(0));
    assert_eq!(stamp(epoch, later), Ok(73_000));
    assert_eq!(stamp(later, epoch), Err(ReadError::Clock));
}

#[test]
fn named_self_cpu_partial_reads_preserve_unknown_counters_and_the_cap() {
    let ports: Vec<_> = (1..=u32::try_from(MAX_THREADS + 2).unwrap()).collect();
    let mut asked = Vec::new();
    let (threads, coverage) = observe_threads(&ports, |port| {
        asked.push(port);
        if port == 2 {
            return None; // unique identity unread: no invented ID or row
        }
        Some(ThreadReading {
            id: u64::from(port) + 1000,
            name: (port != 4).then(|| "aterm-vitals".into()),
            cpu: (port != 3).then_some(CpuCounter {
                ns: u64::from(port),
                precision_ns: 2000,
            }),
            split: (port != 3).then_some(CpuSplit {
                user_ns: u64::from(port),
                system_ns: 0,
            }),
            queue: None,
        })
    });
    assert_eq!(asked, ports[..MAX_THREADS]);
    assert_eq!(
        coverage,
        Coverage {
            listed: MAX_THREADS + 2,
            inspected: MAX_THREADS,
            unread: 2,
            capped: true
        }
    );
    assert_eq!(threads.len(), MAX_THREADS - 1);
    assert!(!threads.iter().any(|t| t.id == 1002));
    let unread = threads.iter().find(|t| t.id == 1003).unwrap();
    assert_eq!(unread.cpu, None);
    let unnamed = threads.iter().find(|t| t.id == 1004).unwrap();
    assert_eq!(unnamed.name, None);
    assert!(
        unnamed.cpu.is_some(),
        "a missing name does not erase observed CPU"
    );
}

/// A fake of this process's memory: `pages` maps a 4 KiB page's base to its bytes, and a copy
/// that crosses a page boundary or touches a missing page is refused, as the kernel refuses a
/// copy that reaches an unmapped page.
struct Memory {
    pages: std::collections::BTreeMap<u64, Vec<u8>>,
}

impl Memory {
    fn with(writes: &[(u64, &[u8])]) -> Self {
        let mut pages = std::collections::BTreeMap::new();
        for &(at, bytes) in writes {
            for (i, b) in bytes.iter().enumerate() {
                let a = at + i as u64;
                let page = pages
                    .entry(a - a % LABEL_SPAN)
                    .or_insert_with(|| vec![0xee; LABEL_SPAN as usize]);
                page[(a % LABEL_SPAN) as usize] = *b;
            }
        }
        Self { pages }
    }

    fn copy(&self, at: u64, buf: &mut [u8]) -> bool {
        let base = at - at % LABEL_SPAN;
        let off = (at % LABEL_SPAN) as usize;
        match self.pages.get(&base) {
            Some(page) if off + buf.len() <= page.len() => {
                buf.copy_from_slice(&page[off..off + buf.len()]);
                true
            }
            _ => false,
        }
    }

    fn queue(&self, slot: u64, label_at: u64) -> Queue {
        let mut word = |at: u64| {
            let mut b = [0u8; 8];
            self.copy(at, &mut b).then(|| u64::from_ne_bytes(b))
        };
        let mut bytes = |at: u64, buf: &mut [u8]| self.copy(at, buf);
        queue_at(slot, label_at, &mut word, &mut bytes)
    }
}

/// The slot's word is the queue, the word at the label offset is the label's address, and the
/// label runs to its NUL, copied in pieces that never cross a page: a label that ends 8 bytes
/// before an unmapped page reads whole, and so does one that runs across a page boundary.
/// The negative control is one 64-byte copy of the first label, which reaches the unmapped page
/// and is refused: a reader that copied the label in one piece would answer `Unread` there.
#[test]
fn named_self_cpu_queue_label_copies_stop_at_page_boundaries() {
    const SLOT: u64 = 0x1_0000;
    const QUEUE: u64 = 0x2_0000;
    const LABEL_AT: u64 = 72;
    let near_end = 0x3_0000 + LABEL_SPAN - 24;
    let label = b"com.apple.Metal\0";
    let memory = Memory::with(&[
        (SLOT, &QUEUE.to_ne_bytes()),
        (QUEUE + LABEL_AT, &near_end.to_ne_bytes()),
        (near_end, label),
    ]);
    assert_eq!(
        memory.queue(SLOT, LABEL_AT),
        Queue::Label("com.apple.Metal".into())
    );
    assert!(
        !memory.copy(near_end, &mut [0u8; LABEL_MAX]),
        "negative control: one 64-byte copy reaches the unmapped page"
    );

    let across = 0x5_0000 + LABEL_SPAN - 4;
    let memory = Memory::with(&[
        (SLOT, &QUEUE.to_ne_bytes()),
        (QUEUE + LABEL_AT, &across.to_ne_bytes()),
        (across, b"aterm.queue\0"),
    ]);
    assert_eq!(
        memory.queue(SLOT, LABEL_AT),
        Queue::Label("aterm.queue".into())
    );

    assert_eq!(memory.queue(0, LABEL_AT), Queue::None, "no slot");
    let empty_slot = Memory::with(&[(SLOT, &0u64.to_ne_bytes())]);
    assert_eq!(
        empty_slot.queue(SLOT, LABEL_AT),
        Queue::None,
        "an empty slot"
    );
    let no_label = Memory::with(&[
        (SLOT, &QUEUE.to_ne_bytes()),
        (QUEUE + LABEL_AT, &0u64.to_ne_bytes()),
    ]);
    assert_eq!(no_label.queue(SLOT, LABEL_AT), Queue::Unlabelled);
    assert_eq!(Memory::with(&[]).queue(SLOT, LABEL_AT), Queue::Unread);
    let long = [b'q'; LABEL_MAX];
    let unterminated = Memory::with(&[
        (SLOT, &QUEUE.to_ne_bytes()),
        (QUEUE + LABEL_AT, &0x7_0000u64.to_ne_bytes()),
        (0x7_0000, &long),
    ]);
    assert_eq!(
        unterminated.queue(SLOT, LABEL_AT),
        Queue::Unread,
        "no NUL within LABEL_MAX bytes"
    );
    let control = Memory::with(&[
        (SLOT, &QUEUE.to_ne_bytes()),
        (QUEUE + LABEL_AT, &0x7_0000u64.to_ne_bytes()),
        (0x7_0000, b"a\x1bb\0"),
    ]);
    assert_eq!(
        control.queue(SLOT, LABEL_AT),
        Queue::Unread,
        "a control character"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn named_self_cpu_unsupported_platform_is_unavailable_instead_of_zero() {
    assert_eq!(read(Instant::now()), Err(ReadError::Unsupported));
}
