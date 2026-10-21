// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `card [seed=<u64>|seed=off]`: THE HARNESS CARD'S CONSTELLATION, PINNED,
//! in a debug or test build (`docs/DESIGN-card-engine-2026-10-19.md` §7), so
//! the card two builds draw over the same screen can be compared pixel for
//! pixel — an A/B of the card engine in a private headless instance
//! (`targo --unverified lab instance --pre 'card seed=42' --image …`) — when
//! each card's stars would otherwise be seeded by its session and its
//! process, which no two runs share. Bare, the verb reads the pin.
//!
//! A pin re-arms every card on show ([`crate::App::rearm_claude_cards`]), so
//! each draws its next frame from the pinned seed; `seed=off` gives the
//! cards their own seeds back on the same terms. The pin is the main
//! thread's ([`crate::claude_card::pin_card_seed`]): a test's thread pins its
//! own cards only.

/// Whether this build pins ([`cmd`]): a debug or test build only.
const PIN_ENABLED: bool = cfg!(any(test, debug_assertions));

/// The usage line every junk form answers with.
const USAGE: &str = "usage: card [seed=<u64>|seed=off]";

/// What the verb asks: the pin read, or set (`None`: off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    Read,
    Pin(Option<u64>),
}

/// The verb's arguments, refused where this build does not pin. Pure.
pub(crate) fn parse(rest: &str, enabled: bool) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let asked = match words.as_slice() {
        [] => Asked::Read,
        [word] => match word.strip_prefix("seed=") {
            Some("off") => Asked::Pin(None),
            Some(seed) => Asked::Pin(Some(
                seed.parse::<u64>()
                    .map_err(|_| format!("{USAGE} (`{seed}` is no u64)"))?,
            )),
            None => return Err(USAGE.to_string()),
        },
        _ => return Err(USAGE.to_string()),
    };
    if !enabled {
        return Err("card needs a debug build".to_string());
    }
    Ok(asked)
}

/// The answer's line: the pin, and how many cards it re-armed.
pub(crate) fn line(pin: Option<u64>, rearmed: usize) -> String {
    match pin {
        Some(seed) => format!("seed={seed} rearmed={rearmed}"),
        None => format!("seed=off rearmed={rearmed}"),
    }
}

/// The verb's control-thread half: the read or the pin on the main thread
/// (`Wake::CardSeed`).
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let asked = match parse(rest, PIN_ENABLED) {
        Ok(asked) => asked,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::CardSeed {
        asked,
        reply,
    }) {
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forms: bare reads, `seed=<u64>` pins, `seed=off` unpins; junk
    /// and a release build are refused.
    #[test]
    fn the_card_verb_parses_its_forms() {
        assert_eq!(parse("", true), Ok(Asked::Read));
        assert_eq!(parse("seed=42", true), Ok(Asked::Pin(Some(42))));
        assert_eq!(parse(" seed=off ", true), Ok(Asked::Pin(None)));
        assert!(parse("seed=-1", true).is_err());
        assert!(parse("seed", true).is_err());
        assert!(parse("seed=1 seed=2", true).is_err());
        assert!(parse("stars=3", true).is_err());
        assert!(parse("seed=42", false).is_err(), "a release build refuses");
        assert_eq!(line(Some(7), 2), "seed=7 rearmed=2");
        assert_eq!(line(None, 0), "seed=off rearmed=0");
    }
}
