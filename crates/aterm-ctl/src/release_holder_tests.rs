// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn identity() -> Identity {
    Identity {
        pid: 79550,
        boot: "7bd6dc21fbac480eabe228c34320639d".into(),
        start: 3_306_349_674_200,
    }
}

#[test]
fn the_argument_round_trips_and_refuses_anything_else() {
    let id = identity();
    assert_eq!(Identity::of_arg(&id.arg()), Some(id.clone()));
    assert_eq!(Identity::parse(&id.word()), Some(id));
    for bad in [
        "",
        "1.ab",
        "1.ab.2.3",
        "x.ab.2",
        "1.AB.2",
        "1..2",
        "-1.ab.2",
        "1.ab.+2",
        "99999999999999999999.ab.2",
    ] {
        assert_eq!(Identity::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Identity::of_arg("facts"), None);
}

/// A row says something only of the identity it echoes, whole.
/// NEGATIVE CONTROLS: another pid or start, a bad alive word and trailing fields read as nothing.
#[test]
fn a_row_is_read_only_for_its_exact_identity() {
    let id = identity();
    for alive in [Some(true), Some(false), None] {
        assert_eq!(id.read(&id.row(alive)), Some(alive));
    }
    let mut other = id.clone();
    other.start += 1;
    assert_eq!(id.read(&other.row(Some(false))), None);
    other = id.clone();
    other.pid += 1;
    assert_eq!(id.read(&other.row(Some(false))), None);
    assert_eq!(id.read(&format!("{} extra=1", id.row(Some(false)))), None);
    assert_eq!(
        id.read(&id.row(Some(false)).replace("alive=0", "alive=dead")),
        None
    );
    assert_eq!(id.read("release_line alive=0 data=%7B%7D"), None);
}
