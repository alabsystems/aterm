// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What one run measures, read from [`ENV`].

use std::path::PathBuf;

/// The variable that names the run.
pub const ENV: &str = "FABRIC_BENCH";

/// Maximum posts one sender session queues in a burst.
pub const PER_SENDER: usize = 100;

/// The largest burst a run takes.
pub const BURST_MAX: usize = 5_000;

/// The largest attach population on one headless instance.
pub const ATTACH_MAX: usize = 128;

/// One run's knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Output directory for `fabric.json` and `fabric.md`.
    pub out: PathBuf,
    /// Scenario selection for the run.
    pub scenario: Scenario,
    /// Measured post-to-deliver round trips.
    pub samples: usize,
    /// Unmeasured warmup round trips.
    pub warmup: usize,
    /// Posts in the burst.
    pub burst: usize,
    /// Measured request/reply round trips in the ping-pong arm.
    pub pingpong: usize,
    /// Ascending attach populations.
    pub attach: Vec<usize>,
    /// Bridge restarts measured per population.
    pub rounds: usize,
    /// In-process broker durability tier.
    pub durability: Tier,
    /// Ordered broker flush horizon in milliseconds.
    pub horizon_ms: Option<u64>,
    /// Whether each world's broker retains its log by default rules.
    pub retention: bool,
    /// Bridge `--log` mirror mode.
    pub log: Log,
    /// Idle bystander sessions spawned beside the talking sessions.
    pub bystanders: usize,
    /// Paced round trips in the ping-pong arm.
    pub paced: usize,
    /// Retaining broker tick interval in milliseconds.
    pub retention_interval_ms: Option<u64>,
}

/// The scenarios measured in one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scenario {
    #[default]
    All,
    Messaging,
    Pingpong,
    Attach,
    Handoff,
}

impl Scenario {
    /// The word the specification and report use.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Messaging => "messaging",
            Self::Pingpong => "pingpong",
            Self::Attach => "attach",
            Self::Handoff => "handoff",
        }
    }

    /// Read a scenario name.
    ///
    /// # Errors
    /// A name outside the supported words.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "all" => Ok(Self::All),
            "messaging" => Ok(Self::Messaging),
            "pingpong" => Ok(Self::Pingpong),
            "attach" => Ok(Self::Attach),
            "handoff" => Ok(Self::Handoff),
            _ => Err(format!(
                "{ENV}: `scenario=` is all, messaging, pingpong, attach or handoff, not `{word}`"
            )),
        }
    }
}

/// The bridge's `--log` mode (`auto` or `off`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Log {
    #[default]
    Auto,
    Off,
}

impl Log {
    /// The word [`parse`] reads and the report prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Log::Auto => "auto",
            Log::Off => "off",
        }
    }

    /// Parse `auto` or `off`.
    ///
    /// # Errors
    /// Any other word.
    pub fn parse(word: &str) -> Result<Log, String> {
        match word {
            "auto" => Ok(Log::Auto),
            "off" => Ok(Log::Off),
            _ => Err(format!("{ENV}: `log=` is `auto` or `off`, not `{word}`")),
        }
    }
}

/// `on` or `off`, for `retention=`. Pure (unit-tested through [`parse`]).
fn on_off(word: &str) -> Result<bool, String> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!(
            "{ENV}: `retention=` is `on` or `off`, not `{word}`"
        )),
    }
}

/// Default ordered broker flush horizon in milliseconds.
pub const ORDERED_HORIZON_MS: u64 = 100;

/// The durability tier for the bench's in-process broker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tier {
    Strict,
    #[default]
    Ordered,
    Relaxed,
}

impl Tier {
    /// The word [`parse`] reads and the report prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Tier::Strict => "strict",
            Tier::Ordered => "ordered",
            Tier::Relaxed => "relaxed",
        }
    }

    /// Parse `strict`, `ordered` or `relaxed`.
    ///
    /// # Errors
    /// Any other word.
    pub fn parse(word: &str) -> Result<Tier, String> {
        match word {
            "strict" => Ok(Tier::Strict),
            "ordered" => Ok(Tier::Ordered),
            "relaxed" => Ok(Tier::Relaxed),
            _ => Err(format!(
                "{ENV}: `durability=` is `strict`, `ordered` or `relaxed`, not `{word}`"
            )),
        }
    }
}

/// Default sample count for `verify perf fabric`.
pub const DEFAULT_SAMPLES: usize = 200;
pub const DEFAULT_WARMUP: usize = 3;
pub const DEFAULT_BURST: usize = 500;
pub const DEFAULT_PINGPONG: usize = 200;
pub const DEFAULT_ATTACH: [usize; 3] = [1, 8, 32];
pub const DEFAULT_ROUNDS: usize = 3;
pub const DEFAULT_PACED: usize = 50;

impl Spec {
    /// The sender sessions the burst is split across.
    #[must_use]
    pub fn burst_senders(&self) -> usize {
        self.burst.div_ceil(PER_SENDER)
    }

    /// Formats the spec as key=value words that [`parse`] reads.
    #[must_use]
    pub fn words(&self) -> String {
        let attach: Vec<String> = self.attach.iter().map(usize::to_string).collect();
        let mut words = format!(
            "samples={} warmup={} burst={} pingpong={} attach={} rounds={} bystanders={} paced={} \
             retention={} log={}",
            self.samples,
            self.warmup,
            self.burst,
            self.pingpong,
            attach.join(","),
            self.rounds,
            self.bystanders,
            self.paced,
            if self.retention { "on" } else { "off" },
            self.log.word()
        );
        if let Some(ms) = self.retention_interval_ms {
            words.push_str(&format!(" retention_interval_ms={ms}"));
        }
        if self.scenario != Scenario::All {
            words.push_str(&format!(" scenario={}", self.scenario.word()));
        }
        words.push_str(&format!(" durability={}", self.durability.word()));
        if let Some(ms) = self.horizon_ms {
            words.push_str(&format!(" horizon_ms={ms}"));
        }
        words
    }
}

/// Parse a `FABRIC_BENCH` specification string.
///
/// # Errors
/// Unknown words, invalid counts or options, or missing `out=`.
pub fn parse(raw: &str) -> Result<Spec, String> {
    let mut out = None;
    let mut spec = Spec {
        out: PathBuf::new(),
        scenario: Scenario::default(),
        samples: DEFAULT_SAMPLES,
        warmup: DEFAULT_WARMUP,
        burst: DEFAULT_BURST,
        pingpong: DEFAULT_PINGPONG,
        attach: DEFAULT_ATTACH.to_vec(),
        rounds: DEFAULT_ROUNDS,
        durability: Tier::default(),
        horizon_ms: None,
        retention: true,
        log: Log::default(),
        retention_interval_ms: None,
        bystanders: 0,
        paced: DEFAULT_PACED,
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
            "scenario" => spec.scenario = Scenario::parse(v)?,
            "samples" => spec.samples = count(key, v, false)?,
            "warmup" => spec.warmup = count(key, v, true)?,
            "burst" => spec.burst = count(key, v, false)?,
            "pingpong" => spec.pingpong = count(key, v, false)?,
            "rounds" => spec.rounds = count(key, v, false)?,
            "bystanders" => spec.bystanders = count(key, v, true)?,
            "paced" => spec.paced = count(key, v, true)?,
            "durability" => spec.durability = Tier::parse(v)?,
            "retention" => spec.retention = on_off(v)?,
            "log" => spec.log = Log::parse(v)?,
            "retention_interval_ms" => {
                spec.retention_interval_ms = Some(count(key, v, false)? as u64);
            }
            "horizon_ms" => {
                spec.horizon_ms = Some(v.parse::<u64>().map_err(|_| {
                    format!("{ENV}: `horizon_ms=` takes a non-negative integer, not `{v}`")
                })?);
            }
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
    match (spec.durability, spec.horizon_ms) {
        (Tier::Ordered, None) => spec.horizon_ms = Some(ORDERED_HORIZON_MS),
        (Tier::Ordered, Some(_)) | (_, None) => {}
        (tier, Some(_)) => {
            return Err(format!(
                "{ENV}: horizon_ms= is the ordered tier's, and this run is {}",
                tier.word()
            ));
        }
    }
    if spec.retention_interval_ms.is_some() && !spec.retention {
        return Err(format!(
            "{ENV}: retention_interval_ms= is a retaining broker's tick, and this run is \
             retention=off"
        ));
    }
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
    if spec.bystanders > ATTACH_MAX {
        return Err(format!(
            "{ENV}: bystanders= tops out at {ATTACH_MAX} sessions on one instance"
        ));
    }
    Ok(spec)
}

/// Parse [`Spec`] from [`ENV`].
///
/// # Errors
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
        assert_eq!(spec.scenario, Scenario::All);
        assert!(
            !spec.words().contains("scenario="),
            "default words stay compatible"
        );
        assert_eq!(
            parse("out=/tmp/x scenario=all").expect("explicit all"),
            spec
        );
        assert_eq!(spec.samples, DEFAULT_SAMPLES);
        assert_eq!(spec.burst, DEFAULT_BURST);
        assert_eq!(spec.pingpong, DEFAULT_PINGPONG);
        assert_eq!(spec.attach, DEFAULT_ATTACH.to_vec());
        assert_eq!(spec.burst_senders(), 5);
        assert_eq!(
            (spec.durability, spec.horizon_ms),
            (Tier::Ordered, Some(ORDERED_HORIZON_MS)),
            "aterm opens its guarded broker ordered at its horizon (run.rs holds it to aterm's)"
        );
        assert_eq!(
            (spec.retention, spec.log, spec.retention_interval_ms),
            (true, Log::Auto, None),
            "what ships: the default rules at every open, the auto mirror, astream's minute"
        );
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
        for scenario in [
            Scenario::Messaging,
            Scenario::Pingpong,
            Scenario::Attach,
            Scenario::Handoff,
        ] {
            let word = scenario.word();
            let spec = parse(&format!("out=/o scenario={word}")).expect(word);
            assert_eq!(spec.scenario, scenario);
            assert!(spec.words().contains(&format!(" scenario={word} ")));
            assert_eq!(
                parse(&format!("out=/o {}", spec.words())).expect("round trip"),
                spec
            );
        }
        for bad in [
            "samples=5",
            "out=/o samples=0",
            "out=/o burst=x",
            "out=/o pingpong=0",
            "out=/o pingpong=-1",
            "out=/o attach=8,1",
            "out=/o attach=1,1",
            "out=/o attach=1,999",
            "out=/o burst=999999",
            "out=/o speed=11",
            "out=/o samples",
            "out=",
            "out=/o scenario=",
            "out=/o scenario=Pingpong",
            "out=/o scenario=pingpong,attach",
            "out=/o scenario=unknown",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn retention_the_mirror_and_the_tick_are_named_and_reproduce() {
        use super::*;

        for (words, retention, log, every) in [
            ("retention=on log=auto", true, Log::Auto, None),
            ("retention=on log=off", true, Log::Off, None),
            ("retention=off", false, Log::Auto, None),
            ("retention=off log=off", false, Log::Off, None),
            ("retention_interval_ms=1000", true, Log::Auto, Some(1000)),
        ] {
            let spec = parse(&format!("out=/o {words}")).expect(words);
            assert_eq!(
                (spec.retention, spec.log, spec.retention_interval_ms),
                (retention, log, every),
                "{words}"
            );
            let said = spec.words();
            assert!(
                said.contains(&format!(
                    " retention={} log={}",
                    if retention { "on" } else { "off" },
                    log.word()
                )),
                "{said}"
            );
            assert_eq!(
                parse(&format!("out=/o {said}")).expect("round trip"),
                spec,
                "{said}"
            );
        }
        for bad in [
            "retention=yes",
            "retention=",
            "log=turns",
            "log=on",
            "retention_interval_ms=0",
            "retention_interval_ms=soon",
            "retention=off retention_interval_ms=1000",
        ] {
            let err = parse(&format!("out=/o {bad}")).expect_err(bad);
            assert!(
                err.contains("retention") || err.contains("log="),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn the_durability_word_names_the_tier_and_nothing_else() {
        use super::*;

        for (word, tier) in [("strict", Tier::Strict), ("relaxed", Tier::Relaxed)] {
            let spec = parse(&format!("out=/o durability={word}")).expect(word);
            assert_eq!(spec.durability, tier, "{word}");
            assert_eq!(spec.horizon_ms, None, "{word}: no horizon");
            assert_eq!(tier.word(), word);
            assert!(spec.words().ends_with(&format!(" durability={word}")));
            let again = parse(&format!("out=/o {}", spec.words())).expect("round trip");
            assert_eq!(again, spec);
        }
        let ordered = parse("out=/o durability=ordered").expect("ordered");
        assert_eq!(
            (ordered.durability, ordered.horizon_ms),
            (Tier::Ordered, Some(ORDERED_HORIZON_MS))
        );
        assert!(ordered.words().ends_with(&format!(
            " durability=ordered horizon_ms={ORDERED_HORIZON_MS}"
        )));
        let fast = parse("out=/o durability=ordered horizon_ms=1000").expect("1 s");
        assert_eq!(fast.horizon_ms, Some(1000));
        assert_eq!(
            parse(&format!("out=/o {}", fast.words())).expect("round trip"),
            fast
        );
        for bad in [
            "durability=strict horizon_ms=100",
            "durability=relaxed horizon_ms=100",
        ] {
            let err = parse(&format!("out=/o {bad}")).expect_err(bad);
            assert!(err.contains("horizon_ms="), "{bad}: {err}");
        }
        assert_eq!(
            parse("out=/o horizon_ms=1000")
                .expect("the default arm's")
                .horizon_ms,
            Some(1000)
        );
        assert!(parse("out=/o durability=ordered horizon_ms=soon").is_err());
        for bad in [
            "Strict",
            "RELAXED",
            "Ordered",
            "replicated",
            "",
            "relaxed,strict",
        ] {
            let err = parse(&format!("out=/o durability={bad}")).expect_err(bad);
            assert!(err.contains("durability="), "{bad}: {err}");
        }
    }

    /// B10's two knobs: idle bystander sessions beside the talking two, and
    /// the ping-pong arm's paced rounds.
    #[test]
    fn the_bystanders_and_the_paced_rounds_are_named_and_reproduce() {
        use super::*;

        let bare = parse("out=/o").expect("bare");
        assert_eq!((bare.bystanders, bare.paced), (0, DEFAULT_PACED));
        assert!(
            bare.words()
                .contains(&format!(" bystanders=0 paced={DEFAULT_PACED} ")),
            "{}",
            bare.words()
        );
        for (words, bystanders, paced) in [
            ("bystanders=64", 64, DEFAULT_PACED),
            ("bystanders=16 paced=0", 16, 0),
            ("paced=10", 0, 10),
        ] {
            let spec = parse(&format!("out=/o {words}")).expect(words);
            assert_eq!(
                (spec.bystanders, spec.paced),
                (bystanders, paced),
                "{words}"
            );
            assert_eq!(
                parse(&format!("out=/o {}", spec.words())).expect("round trip"),
                spec,
                "{words}"
            );
        }
        for bad in [
            "bystanders=-1",
            "bystanders=x",
            "paced=many",
            "bystanders=999",
        ] {
            assert!(parse(&format!("out=/o {bad}")).is_err(), "{bad}");
        }
    }
}
