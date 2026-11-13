// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Two readers that both found the launcher dead each appended a `dead_seen` line; the first
/// stands. Negative control: let a later line replace the first, and the later (wider)
/// horizon reads.
#[test]
fn a_record_with_two_dead_seen_lines_keeps_the_first() {
    let rec = Record::parse(
        "run v=1 boot=b id=71-1792163800331520 uniqueid=5000 start=1 sid=-\n\
         dead_seen uniqueid=9000 at=1792163900000\n\
         dead_seen uniqueid=9500 at=1792164000000\n",
    );
    assert_eq!(rec.dead_seen, Some(9000));
    assert_eq!(rec.dead_seen_ms, Some(1_792_163_900_000));
}

/// A record's numbers are digits alone: a sign `str::parse` would take names no pid, group
/// or window edge. NEGATIVE CONTROL: parse the fields with `str::parse`, and the plus-signed
/// ones read as numbers.
#[test]
fn a_signed_number_in_a_record_is_no_number() {
    assert_eq!(RunId::parse("+41983-100"), None);
    assert_eq!(RunId::parse("41983-+100"), None);
    let rec = Record::parse(
        "run v=1 boot=b id=71-100 uniqueid=+5000 start=+1 sid=-\n\
         group pgid=+4000 leader_birth=+9 leader_uniqueid=+3000 stage=t\n\
         group pgid=4001 leader_birth=+9 leader_uniqueid=+3000 stage=t\n\
         closed pgid=+4001\n\
         dead_seen uniqueid=+9000 at=1\n",
    );
    assert_eq!((rec.uniqueid, rec.start_ms), (0, 0));
    assert_eq!(rec.groups.len(), 1, "a signed pgid names no group");
    let g = &rec.groups[0];
    assert_eq!(
        (g.pgid, g.leader_birth, g.leader_uniqueid, g.closed),
        (4001, Birth(0), 0, false)
    );
    assert_eq!(rec.dead_seen, None);
}
