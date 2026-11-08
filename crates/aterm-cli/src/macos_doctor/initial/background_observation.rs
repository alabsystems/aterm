// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prompt-free CLI observation of the coordinator which already owns workstation setup.

use aterm_machines::init::WorkstationObservation;
use aterm_types::macos_setup::{Selection, Setting, Standing, Subject};
use std::collections::BTreeMap;
use std::time::Duration;

/// Read the running coordinator's published state without starting setup, selecting its tab,
/// reading other apps' settings, or requesting a permission. Only the requested private
/// settings decide this result; unrelated cached rows remain diagnostics.
#[must_use]
pub fn check_background_workstation(requested: Selection) -> WorkstationObservation {
    match aterm_ctl::front_door_ask("setup status\n", Duration::from_secs(2)) {
        aterm_ctl::FrontDoorAnswer::Answered(reply) => observation(&reply, requested.workstation()),
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

fn observation(reply: &str, requested: Selection) -> WorkstationObservation {
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
    let mut unknown = head.strip_prefix("unknown:");
    let (done, pending) = match head {
        "done" => (true, None),
        "pending" => (false, None),
        word => match word
            .strip_prefix("pending:")
            .filter(|words| subjects(words))
        {
            Some(words) => (false, Some(words)),
            None if unknown.is_some_and(subjects) => (false, None),
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
            "unknown" if !done && unknown.is_none() => unknown = Some(words),
            _ => return unavailable(),
        }
    }
    if let Some(needs) = needs
        && needs.split(',').any(|word| {
            !pending
                .unwrap_or_default()
                .split(',')
                .any(|pending| pending == word)
        })
    {
        return unavailable();
    }
    if unknown.is_some_and(|unknown| {
        unknown.split(',').any(|word| {
            pending.is_some_and(|pending| pending.split(',').any(|pending| pending == word))
        })
    }) {
        return unavailable();
    }
    let rows: Vec<_> = fields
        .iter()
        .filter_map(|field| field.strip_prefix("rows="))
        .collect();
    match rows.as_slice() {
        [rows] => match cached_rows(rows) {
            Some(rows) => scoped_observation(requested, verified, Some(&rows), false),
            None => unavailable(),
        },
        // Older coordinators already proved their complete verified mask. Descriptive
        // row metadata adds no prerequisite to that evidence; partial legacy status
        // cannot identify which requested row still waits or needs a person.
        [] => scoped_observation(requested, verified, None, done),
        _ => unavailable(),
    }
}

fn cached_rows(words: &str) -> Option<BTreeMap<&str, Option<Standing>>> {
    let mut rows = BTreeMap::new();
    if words == "-" {
        return Some(rows);
    }
    if words.len() > 4096 {
        return None;
    }
    for row in words.split(',') {
        let (subject, standing) = row.split_once(':')?;
        if !subjects(subject) {
            return None;
        }
        let standing = if standing == "unread" {
            None
        } else {
            Some(Standing::from_status_word(standing)?)
        };
        if rows.insert(subject, standing).is_some() {
            return None;
        }
    }
    Some(rows)
}

fn scoped_observation(
    requested: Selection,
    verified: Selection,
    rows: Option<&BTreeMap<&str, Option<Standing>>>,
    legacy_done: bool,
) -> WorkstationObservation {
    let mut left = Vec::new();
    let mut needs = false;
    for setting in Setting::ALL
        .into_iter()
        .filter(|setting| requested.contains(*setting))
    {
        let word = Subject::Setting(setting).status_word();
        let standing = rows.and_then(|rows| rows.get(word)).copied().flatten();
        if verified.contains(setting)
            && (matches!(standing, Some(Standing::InPlace | Standing::NotNeeded))
                || (rows.is_none() && legacy_done))
        {
            continue;
        }
        needs |= standing == Some(Standing::NeedsYou);
        left.push(format!(
            "{word}={}",
            standing.map_or("unread", Standing::status_word)
        ));
    }
    if left.is_empty() {
        WorkstationObservation::Complete(verified)
    } else if needs {
        WorkstationObservation::Permission(format!(
            "background setup needs a person's action: {}",
            left.join(", ")
        ))
    } else {
        WorkstationObservation::Waiting(format!(
            "background setup has not verified requested preferences: {}",
            left.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_observation_keeps_completion_waits_and_permissions_distinct() {
        let mut caps = Selection::none();
        caps.set(Setting::CapsLock, true);
        let observation = |reply: &str| super::observation(reply, caps);
        assert_eq!(
            super::observation("OK schema=2 sid=0 setup=done verified=0", Selection::none()),
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
            "OK schema=2 setup=unknown:capslock verified=0 observed_generation=7 rows=capslock:unknown caps_live=control",
            "OK schema=2 setup=pending;unknown:capslock verified=0",
            "OK schema=2 setup=pending:repos;unknown:capslock verified=0",
        ] {
            assert!(matches!(
                observation(reply),
                WorkstationObservation::Waiting(_)
            ));
        }
        assert!(matches!(
            observation(
                "OK schema=2 setup=pending:capslock;needs:capslock verified=0 rows=capslock:needs-you"
            ),
            WorkstationObservation::Permission(_)
        ));
        assert_eq!(
            observation("OK schema=2 setup=done verified=1"),
            WorkstationObservation::Complete(caps),
            "legacy completion retains its verified requested scope"
        );
        assert_eq!(
            observation(
                "OK schema=2 setup=unknown:dock verified=1 observed_generation=7 rows=capslock:in-place,dock:unknown caps_live=mixed:control=1,other=0,held=1"
            ),
            WorkstationObservation::Complete(caps),
            "unrelated unread rows do not taint a verified requested setting"
        );
        for (word, standing, setting) in [
            ("dock", "differs", Setting::Dock),
            ("trackpad", "at-login", Setting::TrackpadScrolling),
        ] {
            let reply = format!(
                "OK schema=2 setup=unknown:widgets verified=1 rows=capslock:in-place,{word}:{standing},widgets:unknown"
            );
            assert_eq!(observation(&reply), WorkstationObservation::Complete(caps));
            let mut selected = Selection::none();
            selected.set(setting, true);
            let WorkstationObservation::Waiting(why) = super::observation(&reply, selected) else {
                panic!("settled intent does not verify an explicit requested default");
            };
            assert!(
                why.contains(word) && !why.contains("capslock") && !why.contains("widgets"),
                "{why}"
            );
        }
        for reply in [
            "OK schema=2 setup=done verified=0",
            "OK schema=2 setup=pending:lan;needs:lan verified=0",
            "OK schema=2 setup=pending:spotlight;unknown:capslock verified=0 rows=capslock:unknown,dock:failed",
            "OK schema=2 setup=unknown:capslock verified=1 rows=capslock:unknown,dock:in-place",
            "OK schema=2 setup=pending verified=0 rows=capslock:in-place,dock:unknown",
            "OK schema=2 setup=done verified=1 rows=dock:in-place",
            "OK schema=2 setup=pending:capslock verified=0 rows=capslock:differs,dock:waiting",
            "OK schema=2 setup=pending verified=0 rows=capslock:at-login,dock:unknown",
        ] {
            let WorkstationObservation::Waiting(why) = observation(reply) else {
                panic!("the requested setting is unverified: {reply}");
            };
            assert!(
                why.contains("capslock")
                    && !why.contains("dock")
                    && !why.contains("spotlight")
                    && !why.contains("lan"),
                "{why}"
            );
        }
        let mut both = caps;
        both.set(Setting::Dock, true);
        let WorkstationObservation::Waiting(why) =
            super::observation("OK schema=2 setup=done verified=1", both)
        else {
            panic!("legacy partial coverage cannot complete both requested rows");
        };
        assert!(why.contains("dock") && !why.contains("capslock"), "{why}");
    }

    #[test]
    fn missing_refused_or_ambiguous_evidence_is_never_completion_or_permission() {
        let mut caps = Selection::none();
        caps.set(Setting::CapsLock, true);
        let observation = |reply: &str| super::observation(reply, caps);
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
            "OK schema=2 setup=unknown: verified=0",
            "OK schema=2 setup=done;unknown:capslock verified=0",
            "OK schema=2 setup=unknown:capslock;unknown:desktop verified=0",
            "OK schema=2 setup=pending;unknown: verified=0",
            "OK schema=2 setup=pending:repos;unknown:capslock;unknown:desktop verified=0",
            "OK schema=2 setup=pending:repos;unknown:capslock;needs:capslock verified=0",
            "OK schema=2 setup=pending:capslock;unknown:capslock verified=0",
            "OK schema=2 setup=done verified=1 rows=capslock:in-place,capslock:unknown",
            "OK schema=2 setup=done verified=1 rows=capslock:in-place rows=capslock:unknown",
            "OK schema=2 setup=done verified=1 rows=capslock:invented",
            "OK schema=2 setup=done verified=1 rows=capslock:",
        ] {
            assert!(
                matches!(observation(reply), WorkstationObservation::Unavailable(_)),
                "{reply}"
            );
        }
    }
}
