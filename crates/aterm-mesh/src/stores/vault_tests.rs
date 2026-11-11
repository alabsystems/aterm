// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::table::store::{AppendKind, Appended, Via};

/// A store append feeds the egress its record with its provenance: a local append (a write,
/// or what an old peer's repair round brought) is published, one the mesh brought is not.
#[test]
fn an_append_feeds_its_record_marked_by_how_it_came() {
    for (via, mesh_borne) in [(Via::Local, false), (Via::Mesh, true)] {
        let appended = Appended {
            table: [7; 32],
            recovery: true,
            kind: AppendKind::Conflict,
            offset: 4,
            length: 5,
            via,
            wire: b"wire",
        };
        let Feed::Table {
            key,
            length,
            record: Some((held, borne)),
        } = table_feed(&appended)
        else {
            panic!("a table feed with its record");
        };
        assert_eq!(
            key,
            TableKey {
                table: [7; 32],
                recovery: true
            }
        );
        assert_eq!((length, held.offset, held.kind), (5, 4, Kind::Conflict));
        assert_eq!(held.wire, b"wire");
        assert_eq!(borne, mesh_borne);
    }
}
