// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What one run measures, read from [`ENV`].
//!
//! ONE VARIABLE, WORDS INSIDE IT. `FABRIC_BENCH="out=<dir> samples=200 …"` is
//! one name the lane sets (`crates/aterm-verify/src/perf/fabric.rs`) and one
//! grammar this file parses, refusing a word it does not know rather than
//! measuring something other than what was asked.
//!
//! The explicit bench requires this specification; an unset, non-Unicode or
//! malformed value refuses the run instead of reporting an empty measurement.

use std::path::PathBuf;

/// The variable that names the run.
pub const ENV: &str = "FABRIC_BENCH";

/// How many posts ONE sender session queues in a burst, at most.
///
/// `aterm-gui`'s `OUTBOX_CAP` is 128 waiting posts per session, refused at the
/// door with `ERR outbox full`. The burst's in-flight window (`run.rs`
/// `BURST_WINDOW`, under the recipient's 64-row allowance) already keeps fewer
/// posts than that queued in all, so the cap cannot bind today; the split
/// keeps it that way on its own. A burst of N comes from `ceil(N / PER_SENDER)`
/// sessions, so no sender's share — 100, plus the warm-up post each makes
/// first — could reach the cap even with the window gone, and the bridge
/// drains several sessions' outboxes, as a burst from many agents makes it.
pub const PER_SENDER: usize = 100;

/// The largest burst a run takes: 50 sender sessions on one headless instance.
pub const BURST_MAX: usize = 5_000;

/// The largest attach population: sessions spawned on one headless instance.
pub const ATTACH_MAX: usize = 128;

/// One run's knobs. Every count is at least one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Where `fabric.json` and `fabric.md` are written.
    pub out: PathBuf,
    /// Measured post -> deliver round trips.
    pub samples: usize,
    /// Round trips run first and not measured: the first post on a fresh lane
    /// resolves its route and creates the bridge's per-session state files.
    pub warmup: usize,
    /// Posts in the burst.
    pub burst: usize,
    /// Measured request/reply round trips in the ping-pong arm
    /// (`run/pingpong.rs`), after `warmup` unmeasured ones.
    pub pingpong: usize,
    /// The attach populations, ascending: each is measured on one instance as
    /// it grows, so the list must rise.
    pub attach: Vec<usize>,
    /// Bridge restarts measured per population.
    pub rounds: usize,
    /// What the in-process broker's ack promises ([`Tier`]).
    pub durability: Tier,
}

/// The durability tier the bench opens its in-process broker at — astream's
/// `Durability`, named here in a std-only type because the lane mounts this
/// file and has no astream dependency (`run.rs` maps it).
///
/// STRICT IS THE MEASUREMENT; RELAXED IS THE CEILING. aterm opens its broker
/// Strict (`Broker::open_guarded`: every ack after an fsync, `F_FULLFSYNC` on
/// macOS), so the default is what a person's message costs. Relaxed acks after
/// the batch's page-cache write instead, through the same guarded open
/// (`Broker::open_guarded_with`, the same capability checks): it is what the
/// stack costs with the broker's fsync taken out, the bound an ordered tier
/// (`F_BARRIERFSYNC`, ordering without the drive-cache flush) could approach
/// and never pass. The bridge's own state-file writes are not the broker's
/// and stay as they are in both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tier {
    #[default]
    Strict,
    Relaxed,
}

impl Tier {
    /// The word [`parse`] reads, and the report prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Tier::Strict => "strict",
            Tier::Relaxed => "relaxed",
        }
    }

    /// `strict` or `relaxed`, and nothing else. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// Any other word, quoted.
    pub fn parse(word: &str) -> Result<Tier, String> {
        match word {
            "strict" => Ok(Tier::Strict),
            "relaxed" => Ok(Tier::Relaxed),
            _ => Err(format!(
                "{ENV}: `durability=` is `strict` or `relaxed`, not `{word}`"
            )),
        }
    }
}

/// The defaults: what `verify perf fabric` runs with no option.
pub const DEFAULT_SAMPLES: usize = 200;
pub const DEFAULT_WARMUP: usize = 3;
pub const DEFAULT_BURST: usize = 500;
pub const DEFAULT_PINGPONG: usize = 200;
pub const DEFAULT_ATTACH: [usize; 3] = [1, 8, 32];
pub const DEFAULT_ROUNDS: usize = 3;

impl Spec {
    /// The sender sessions the burst is split across.
    #[must_use]
    pub fn burst_senders(&self) -> usize {
        self.burst.div_ceil(PER_SENDER)
    }

    /// The spec as the words [`parse`] reads, for the report.
    #[must_use]
    pub fn words(&self) -> String {
        let attach: Vec<String> = self.attach.iter().map(usize::to_string).collect();
        format!(
            "samples={} warmup={} burst={} pingpong={} attach={} rounds={} durability={}",
            self.samples,
            self.warmup,
            self.burst,
            self.pingpong,
            attach.join(","),
            self.rounds,
            self.durability.word()
        )
    }
}

/// Parse `out=<dir> [samples=<n>] [warmup=<n>] [burst=<n>] [pingpong=<n>]
/// [attach=<n>,<n>,…] [rounds=<n>] [durability=strict|relaxed]`. Pure
/// (unit-tested).
///
/// # Errors
///
/// A word that is not one of those, a count that is not a positive integer
/// (`warmup` may be 0), a durability that is not a [`Tier`], a burst past
/// [`BURST_MAX`], an attach list that does not rise or passes [`ATTACH_MAX`],
/// or no `out=`.
pub fn parse(raw: &str) -> Result<Spec, String> {
    let mut out = None;
    let mut spec = Spec {
        out: PathBuf::new(),
        samples: DEFAULT_SAMPLES,
        warmup: DEFAULT_WARMUP,
        burst: DEFAULT_BURST,
        pingpong: DEFAULT_PINGPONG,
        attach: DEFAULT_ATTACH.to_vec(),
        rounds: DEFAULT_ROUNDS,
        durability: Tier::Strict,
    };
    let count = |key: &str, v: &str, zero_ok: bool| -> Result<usize, String> {
        match v.parse::<usize>() {
            Ok(n) if n > 0 || zero_ok => Ok(n),
            _ => Err(format!(
                "{ENV}: `{key}=` takes a {} integer, not `{v}`",
                if zero_ok { "non-negative" } else { "positive" }
            )),
        }
    };
    for word in raw.split_whitespace() {
        let (key, v) = word
            .split_once('=')
            .ok_or_else(|| format!("{ENV}: `{word}` is not a key=value word"))?;
        match key {
            "out" if !v.is_empty() => out = Some(PathBuf::from(v)),
            "samples" => spec.samples = count(key, v, false)?,
            "warmup" => spec.warmup = count(key, v, true)?,
            "burst" => spec.burst = count(key, v, false)?,
            "pingpong" => spec.pingpong = count(key, v, false)?,
            "rounds" => spec.rounds = count(key, v, false)?,
            "durability" => spec.durability = Tier::parse(v)?,
            "attach" => {
                spec.attach = v
                    .split(',')
                    .map(|n| count(key, n, false))
                    .collect::<Result<_, _>>()?;
            }
            _ => return Err(format!("{ENV}: unknown word `{word}`")),
        }
    }
    spec.out = out.ok_or_else(|| format!("{ENV}: `out=<dir>` names where the report goes"))?;
    if spec.burst > BURST_MAX {
        return Err(format!(
            "{ENV}: burst={} is past the {BURST_MAX} one instance's sender sessions carry",
            spec.burst
        ));
    }
    if !spec.attach.windows(2).all(|w| w[0] < w[1]) {
        return Err(format!(
            "{ENV}: attach= must rise (each population grows from the one before)"
        ));
    }
    if spec.attach.last().is_some_and(|&n| n > ATTACH_MAX) {
        return Err(format!("{ENV}: attach= tops out at {ATTACH_MAX} sessions"));
    }
    Ok(spec)
}

/// The required run [`ENV`] names.
///
/// # Errors
///
/// Missing, non-Unicode or malformed input.
pub fn from_env() -> Result<Spec, String> {
    let raw = std::env::var(ENV)
        .map_err(|e| format!("{ENV} is required (`verify perf fabric` sets it): {e}"))?;
    parse(&raw)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_defaults_fill_what_the_words_leave_out() {
        use super::*;

        let spec = parse("out=/tmp/x").expect("parses");
        assert_eq!(spec.out, PathBuf::from("/tmp/x"));
        assert_eq!(spec.samples, DEFAULT_SAMPLES);
        assert_eq!(spec.burst, DEFAULT_BURST);
        assert_eq!(spec.pingpong, DEFAULT_PINGPONG);
        assert_eq!(spec.attach, DEFAULT_ATTACH.to_vec());
        assert_eq!(spec.burst_senders(), 5);
        assert_eq!(
            spec.durability,
            Tier::Strict,
            "aterm opens its broker Strict"
        );
        // The words it reports are the words that reproduce it.
        let again = parse(&format!("out=/tmp/x {}", spec.words())).expect("round trip");
        assert_eq!(again, spec);
    }

    #[test]
    fn every_knob_is_read_and_a_bad_word_is_refused() {
        use super::*;

        let spec = parse("out=/o samples=7 warmup=0 burst=101 pingpong=9 attach=2,4 rounds=1")
            .expect("ok");
        assert_eq!(
            (
                spec.samples,
                spec.warmup,
                spec.burst,
                spec.pingpong,
                spec.rounds
            ),
            (7, 0, 101, 9, 1)
        );
        assert_eq!(spec.attach, vec![2, 4]);
        assert_eq!(spec.burst_senders(), 2, "101 posts need a second sender");
        for bad in [
            "samples=5",           // no out=
            "out=/o samples=0",    // not positive
            "out=/o burst=x",      // not a number
            "out=/o pingpong=0",   // not positive
            "out=/o pingpong=-1",  // not a number
            "out=/o attach=8,1",   // does not rise
            "out=/o attach=1,1",   // repeats
            "out=/o attach=1,999", // past ATTACH_MAX
            "out=/o burst=999999", // past BURST_MAX
            "out=/o speed=11",     // unknown
            "out=/o samples",      // not key=value
            "out=",                // empty out
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_durability_word_names_the_tier_and_nothing_else() {
        use super::*;

        for (word, tier) in [("strict", Tier::Strict), ("relaxed", Tier::Relaxed)] {
            let spec = parse(&format!("out=/o durability={word}")).expect(word);
            assert_eq!(spec.durability, tier, "{word}");
            assert_eq!(tier.word(), word);
            // The report's words carry the tier, so an entry says what its
            // acks promised and reproduces it.
            assert!(spec.words().ends_with(&format!(" durability={word}")));
            let again = parse(&format!("out=/o {}", spec.words())).expect("round trip");
            assert_eq!(again, spec);
        }
        // Only the two words, spelled exactly: a near miss is refused rather
        // than measured at a tier nobody asked for.
        for bad in [
            "Strict",
            "RELAXED",
            "ordered",
            "replicated",
            "",
            "relaxed,strict",
        ] {
            let err = parse(&format!("out=/o durability={bad}")).expect_err(bad);
            assert!(err.contains("durability="), "{bad}: {err}");
        }
    }
}
