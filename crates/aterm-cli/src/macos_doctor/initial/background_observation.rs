// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prompt-free CLI observation of the coordinator which already owns workstation setup.

use aterm_machines::init::WorkstationObservation;
use aterm_types::macos_setup::Selection;
use std::time::Duration;

/// Read the running coordinator's published state without starting setup, selecting its tab,
/// reading other apps' settings, or requesting a permission.
#[must_use]
pub fn check_background_workstation() -> WorkstationObservation {
    match aterm_ctl::front_door_ask("setup status\n", Duration::from_secs(2)) {
        aterm_ctl::FrontDoorAnswer::Answered(reply) => observation(&reply),
        aterm_ctl::FrontDoorAnswer::Stopped(_) | aterm_ctl::FrontDoorAnswer::Silent(_) => {
            WorkstationObservation::Waiting("background setup has not answered yet".into())
        }
        aterm_ctl::FrontDoorAnswer::Absent => unavailable(),
    }
}

fn unavailable() -> WorkstationObservation {
    WorkstationObservation::Unavailable(
        "background setup has not published a usable workstation observation".into(),
    )
}

fn subjects(words: &str) -> bool {
    !words.is_empty()
        && words.len() <= 1024
        && words.split(',').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn observation(reply: &str) -> WorkstationObservation {
    if reply.lines().count() != 1 || reply.split_ascii_whitespace().next() != Some("OK") {
        return unavailable();
    }
    let fields: Vec<_> = reply.split_ascii_whitespace().collect();
    let schema: Vec<_> = fields
        .iter()
        .filter_map(|word| word.strip_prefix("schema="))
        .collect();
    let setup: Vec<_> = fields
        .iter()
        .filter_map(|word| word.strip_prefix("setup="))
        .collect();
    let verified: Vec<_> = fields
        .iter()
        .filter_map(|word| word.strip_prefix("verified="))
        .collect();
    if schema != ["2"] || setup.len() != 1 || verified.len() != 1 {
        return unavailable();
    }
    let Some(verified) = verified[0]
        .parse::<u16>()
        .ok()
        .and_then(Selection::from_bits)
        .filter(|verified| *verified == verified.workstation())
    else {
        return unavailable();
    };
    let mut parts = setup[0].split(';');
    let head = parts.next().unwrap_or_default();
    let (done, pending) = match head {
        "done" => (true, None),
        "pending" => (false, None),
        word => match word
            .strip_prefix("pending:")
            .filter(|words| subjects(words))
        {
            Some(words) => (false, Some(words)),
            None => return unavailable(),
        },
    };
    let mut asking = None;
    let mut needs = None;
    let mut offered = false;
    for part in parts {
        let Some((key, words)) = part.split_once(':').filter(|(_, words)| subjects(words)) else {
            return unavailable();
        };
        match key {
            "asking" if done && asking.is_none() => asking = Some(words),
            "offered" if done && !offered => offered = true,
            "needs" if !done && pending.is_some() && needs.is_none() => needs = Some(words),
            _ => return unavailable(),
        }
    }
    if let Some(needs) = needs {
        if needs.split(',').any(|word| {
            !pending
                .unwrap_or_default()
                .split(',')
                .any(|pending| pending == word)
        }) {
            return unavailable();
        }
        WorkstationObservation::Permission(format!(
            "background setup needs a person's action: {needs}"
        ))
    } else if let Some(asking) = asking {
        WorkstationObservation::Waiting(format!("background setup is continuing: {asking}"))
    } else if done {
        WorkstationObservation::Complete(verified)
    } else {
        WorkstationObservation::Waiting(pending.map_or_else(
            || "background setup is checking".into(),
            |words| format!("background setup is waiting: {words}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_observation_keeps_completion_waits_and_permissions_distinct() {
        assert_eq!(
            observation("OK schema=2 sid=0 setup=done verified=0"),
            WorkstationObservation::Complete(Selection::none())
        );
        let verified = Selection::all().workstation();
        assert_eq!(
            observation(&format!(
                "OK schema=2 setup=done;offered:lan verified={}",
                verified.bits()
            )),
            WorkstationObservation::Complete(verified)
        );
        for reply in [
            "OK schema=2 setup=pending verified=0",
            "OK schema=2 setup=pending:repos verified=0",
            "OK schema=2 setup=done;asking:vault;offered:lan verified=0",
        ] {
            assert!(matches!(
                observation(reply),
                WorkstationObservation::Waiting(_)
            ));
        }
        assert!(matches!(
            observation("OK schema=2 setup=pending:lan;needs:lan verified=0"),
            WorkstationObservation::Permission(_)
        ));
    }

    #[test]
    fn missing_refused_or_ambiguous_evidence_is_never_completion_or_permission() {
        for reply in [
            "ERR auth",
            "OK schema=1",
            "OK schema=1 setup=done",
            "OK schema=1 setup=done verified=255",
            "OK schema=2 setup=done",
            "OK schema=2 setup=done verified=0 verified=255",
            "OK schema=2 setup=done verified=256",
            "OK schema=2 setup=done verified=65535",
            "OK schema=2 setup=done verified=65536",
            "OK schema=2 setup=done verified=-1",
            "OK schema=2 setup=done verified=unknown",
            "OK schema=2 setup=- verified=0",
            "OK schema=2 setup=done setup=pending verified=0",
            "OK schema=2 setup=done;needs:lan verified=0",
            "OK schema=2 setup=pending:repos;needs:lan verified=0",
            "OK schema=2 setup=done;asking: verified=0",
            "OK schema=2 setup=pending:repos,,vault verified=0",
            "OK schema=2 setup=done verified=0\nERR failed",
        ] {
            assert!(
                matches!(observation(reply), WorkstationObservation::Unavailable(_)),
                "{reply}"
            );
        }
    }
}
