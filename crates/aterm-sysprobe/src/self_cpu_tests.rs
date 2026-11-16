// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn named_self_cpu_counters_keep_units_precision_and_wide_intermediates() {
    let thread = thread_counter(1_234_000, 2_345_000).unwrap();
    assert_eq!(thread.ns, 3_579_000);
    assert_eq!(thread.precision_ns, 2_000);
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

#[cfg(not(target_os = "macos"))]
#[test]
fn named_self_cpu_unsupported_platform_is_unavailable_instead_of_zero() {
    assert_eq!(read(Instant::now()), Err(ReadError::Unsupported));
}
