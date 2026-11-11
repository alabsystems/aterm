// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_ask_reads_back_as_itself_and_nothing_else_reads_as_one() {
    let table = [0xab; 32];
    let within = Duration::from_millis(1500);
    assert_eq!(
        read_ask(ask(&table, within).as_bytes()),
        Some((table, within))
    );
    assert_eq!(
        read_ask(ask(&table, Duration::from_secs(3600)).as_bytes()),
        Some((table, WITHIN_MAX)),
        "a longer ask waits the most a push may"
    );
    for other in [
        &b"atm-push 1 ab 5\n"[..],
        b"atm-push 2 abababababababababababababababababababababababababababababababab 5\n",
        b"atm-wake 1 members 3\n",
        b"stop\n",
    ] {
        assert_eq!(read_ask(other), None, "{}", String::from_utf8_lossy(other));
    }
    for pushed in [Some(Pushed::Reached), Some(Pushed::TimedOut), None] {
        assert_eq!(read_answer(answer_of(pushed)), Some(pushed));
    }
    assert_eq!(read_answer(b"atm-pushed 1 maybe\n"), None);
}
