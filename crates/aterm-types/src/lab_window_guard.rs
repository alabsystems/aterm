// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private lab-window descriptor argument grammar. Parsed numbers and paths
//! supply no lock, resource admission or timing authority.

use std::ffi::OsString;

/// Two-word private argument: this flag followed by one inherited descriptor.
pub const FLAG: &str = "--lab-window-guard";
/// Three-word private argument: flag, inherited memory descriptor, original claim leaf.
pub const MEMORY_FLAG: &str = "--lab-window-memory-guard";
/// Fixed leaf beneath the actual account capacity directory, never an argv path.
pub const LOCK_NAME: &str = "lab-window.lock";

/// An original memory description's lookup reference. These parsed values confer
/// no reservation: the actual descriptor and original account record must agree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRef {
    /// Original inherited descriptor, distinct from the lane descriptor.
    pub descriptor: i32,
    /// Bounded single `.claim` filename beneath the independently selected ledger.
    pub claim: String,
}

/// Exact private descriptor roles, never a resource budget or admission proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardBundle {
    /// Original held account window lane.
    pub lane: i32,
    /// Original Functional memory claim; Timing windows retain only their lane here.
    pub memory: Option<MemoryRef>,
}

fn descriptor(word: Option<&OsString>) -> Result<i32, String> {
    word.and_then(|word| word.to_str())
        .filter(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|word| word.parse::<i32>().ok())
        .filter(|fd| *fd > 2)
        .ok_or_else(|| "lab window guards require non-stdio decimal descriptors".into())
}

fn claim_leaf(word: Option<&OsString>) -> Result<String, String> {
    let leaf = word
        .and_then(|word| word.to_str())
        .filter(|leaf| leaf.len() <= 96)
        .filter(|leaf| {
            leaf.strip_suffix(".claim").is_some_and(|stem| {
                !stem.is_empty()
                    && stem
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        })
        .ok_or("lab memory guard requires one bounded ASCII .claim leaf")?;
    Ok(leaf.into())
}

/// Remove one exact lane/optional-memory offer before a command boundary.
/// Words after `--`, `-e` or `--command` belong to the launched program and are
/// never inspected or removed. Absence leaves all arguments unchanged.
/// # Errors
/// Duplicate or joined flags, missing/invalid operands, standard descriptors,
/// unsafe claim leaves, memory without a lane, or reused descriptors. Refusal
/// leaves all original words unchanged; original record eligibility is not parsed here.
pub fn take(args: &mut Vec<OsString>) -> Result<Option<GuardBundle>, String> {
    let end = args
        .iter()
        .position(|word| word == "--" || word == "-e" || word == "--command")
        .unwrap_or(args.len());
    let mut lane_at = None;
    let mut memory_at = None;
    for (index, word) in args[..end].iter().enumerate() {
        for (flag, found) in [(FLAG, &mut lane_at), (MEMORY_FLAG, &mut memory_at)] {
            if word
                .to_str()
                .and_then(|word| word.strip_prefix(flag))
                .is_some_and(|suffix| suffix.starts_with('='))
            {
                return Err(format!("{flag} requires separate operand words"));
            }
            if word == flag && found.replace(index).is_some() {
                return Err(format!("{flag} was supplied more than once"));
            }
        }
    }
    let Some(lane_at) = lane_at else {
        return if memory_at.is_some() {
            Err("lab memory guard requires its original lane guard".into())
        } else {
            Ok(None)
        };
    };
    let before = &args[..end];
    let lane = descriptor(before.get(lane_at + 1))?;
    let memory = memory_at
        .map(|at| -> Result<MemoryRef, String> {
            let descriptor = descriptor(before.get(at + 1))?;
            if descriptor == lane {
                return Err("lab lane and memory guards require distinct descriptors".into());
            }
            Ok(MemoryRef {
                descriptor,
                claim: claim_leaf(before.get(at + 2))?,
            })
        })
        .transpose()?;
    let mut spans = vec![(lane_at, 2)];
    if let Some(at) = memory_at {
        spans.push((at, 3));
    }
    spans.sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, len) in spans {
        args.drain(at..at + len);
    }
    Ok(Some(GuardBundle { lane, memory }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn one_guard_is_removed_without_reinterpreting_a_program_payload() {
        let mut args = words(&["--window", FLAG, "17", "--control-sock", "/private/socket"]);
        assert_eq!(
            take(&mut args),
            Ok(Some(GuardBundle {
                lane: 17,
                memory: None
            }))
        );
        assert_eq!(
            args,
            words(&["--window", "--control-sock", "/private/socket"])
        );
        for boundary in ["--", "-e", "--command"] {
            let mut args = words(&["--window", boundary, "program", FLAG, "0"]);
            let original = args.clone();
            assert_eq!(take(&mut args), Ok(None));
            assert_eq!(args, original);
            let mut args = words(&[FLAG, "9", boundary, "program", FLAG, "0"]);
            assert_eq!(
                take(&mut args),
                Ok(Some(GuardBundle {
                    lane: 9,
                    memory: None
                }))
            );
            assert_eq!(args, words(&[boundary, "program", FLAG, "0"]));
        }
    }

    #[test]
    fn ambiguous_and_invalid_offers_fail_without_consuming_words() {
        for mut args in [
            words(&[FLAG]),
            words(&[FLAG, "0"]),
            words(&[FLAG, "1"]),
            words(&[FLAG, "2"]),
            words(&[FLAG, "-3"]),
            words(&[FLAG, "+3"]),
            words(&[FLAG, "3x"]),
            words(&[FLAG, "2147483648"]),
            words(&["--lab-window-guard=3"]),
            words(&[FLAG, "9", FLAG, "10"]),
            words(&[FLAG, "--", "9"]),
        ] {
            let original = args.clone();
            assert!(take(&mut args).is_err(), "{args:?}");
            assert_eq!(args, original);
        }
    }

    #[test]
    fn functional_offer_has_exact_distinct_roles_and_an_original_leaf() {
        let mut args = words(&[
            "--window",
            MEMORY_FLAG,
            "18",
            "1234567890123-42-0.claim",
            FLAG,
            "17",
        ]);
        assert_eq!(
            take(&mut args),
            Ok(Some(GuardBundle {
                lane: 17,
                memory: Some(MemoryRef {
                    descriptor: 18,
                    claim: "1234567890123-42-0.claim".into()
                })
            }))
        );
        assert_eq!(args, words(&["--window"]));
        for mut args in [
            words(&[MEMORY_FLAG, "18", "123-42-0.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "17", "123-42-0.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "18"]),
            words(&[FLAG, "17", MEMORY_FLAG, "0", "123-42-0.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "-18", "123-42-0.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "18", "../x.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "18", "/x.claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "18", ".claim"]),
            words(&[FLAG, "17", MEMORY_FLAG, "18", "x.wait"]),
            words(&[
                FLAG,
                "17",
                MEMORY_FLAG,
                "18",
                "x.claim",
                MEMORY_FLAG,
                "19",
                "x.claim",
            ]),
        ] {
            let original = args.clone();
            assert!(take(&mut args).is_err(), "{args:?}");
            assert_eq!(args, original);
        }
        let mut payload = words(&[
            FLAG,
            "17",
            MEMORY_FLAG,
            "18",
            "123-42-0.claim",
            "--",
            "program",
            MEMORY_FLAG,
            "0",
            "../bad",
        ]);
        assert!(take(&mut payload).unwrap().unwrap().memory.is_some());
        assert_eq!(
            payload,
            words(&["--", "program", MEMORY_FLAG, "0", "../bad"])
        );
        let mut oversized = words(&[FLAG, "17", MEMORY_FLAG, "18"]);
        oversized.push(format!("{}.claim", "a".repeat(96)).into());
        let original = oversized.clone();
        assert!(take(&mut oversized).is_err());
        assert_eq!(oversized, original);
    }
}
