// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::pty_master_held_of;

/// `/dev/pts/3` as `tty_nr` numbers it.
const PTS_3: u64 = (136 << 8) | 3;

/// A live process's devpts terminal reads held whoever holds its master, a holder no scan of
/// this uid's tables would find included, since the kernel takes a terminal from its session
/// once its master closes; a terminal of another driver, or a number no `tty_nr` is, reads
/// unknown. NEGATIVE CONTROL: the scan this replaced read `Some(false)` for `/dev/pts/3`
/// once no table of this uid held a `ptmx` printing index 3, which is what it read with the
/// master held by root's `sudo`, another user's terminal or a container's shim.
#[test]
fn a_live_processs_pty_reads_held_whoever_holds_its_master() {
    assert_eq!(pty_master_held_of(PTS_3), Some(true), "/dev/pts/3");
    assert_eq!(
        pty_master_held_of(34817),
        Some(true),
        "/dev/pts/1, as a stat line reads it"
    );
    assert_eq!(
        pty_master_held_of(0x0120_8834),
        Some(true),
        "/dev/pts/4660, its minor split across the encoding"
    );
    assert_eq!(pty_master_held_of((4 << 8) | 1), None, "/dev/tty1");
    assert_eq!(pty_master_held_of((4 << 8) | 64), None, "/dev/ttyS0");
    assert_eq!(pty_master_held_of(0), None, "no terminal");
    assert_eq!(
        pty_master_held_of((0x1000 << 32) | PTS_3),
        None,
        "a 64-bit dev_t whose major's low 12 bits are 136"
    );
}
