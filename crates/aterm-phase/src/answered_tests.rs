// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The model's replies read as answers: a plain one, one over several lines, and one that only
/// mentions an error in passing past its first line's opening.
#[test]
fn a_models_reply_reads_as_an_answer() {
    for reply in [
        "Stage done.",
        "Done: the parser reads both forms now.\n\nTests pass.",
        "The build was red because of a stale lock; I removed it and the error is gone.",
    ] {
        assert!(!reads_as_error(reply), "{reply}");
    }
}

/// An error a program draws where its replies go reads as one: Muse Code's `model failed`, agy's
/// executor error, a bare `Error:`, an API error's head, an overload or a rate limit named
/// anywhere in it, the network's failure, and an empty reply.
#[test]
fn an_error_drawn_as_a_reply_reads_as_one() {
    for reply in [
        "model failed: API error 529: overloaded",
        "agent executor error: generating and executing: Error 503, Message: unavailable",
        "Error: stream closed before the response finished",
        "◆ error: upstream returned 502",
        "API Error: 500 Internal server error",
        "Working on it.\nAPI Error: 529 Overloaded. This is a server-side issue, usually temporary.",
        "Request failed with status 503",
        "The request could not finish: connect ECONNREFUSED 127.0.0.1:4100",
        "",
        "  \n ",
    ] {
        assert!(reads_as_error(reply), "{reply:?}");
    }
}
