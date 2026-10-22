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
    /// Which scenario boots a world; all four unless explicitly selected.
    pub scenario: Scenario,
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
    /// The ordered broker's HORIZON, in milliseconds (`horizon_ms=`): how
    /// long after the first ack no full flush covers its writer flushes in
    /// full. `Some` exactly when `durability` is [`Tier::Ordered`]
    /// ([`ORDERED_HORIZON_MS`] unless named).
    pub horizon_ms: Option<u64>,
    /// Whether each world's broker RETAINS its log by aterm's default rules
    /// (`retention=on`, what ships: `aterm_link::broker_rules::retain`, set at
    /// every open as `aterm link broker` sets them) or keeps none
    /// (`retention=off`: a broker from before astream's R4, the harness's
    /// unretained open). Over an unretaining broker the bridge's `auto` mirror
    /// stays off as well, so `retention=off` takes both away and [`Log`]
    /// separates the two.
    pub retention: bool,
    /// The bridge's `--log` ([`Log`]): `auto`, what ships, or `off`.
    pub log: Log,
    /// IDLE BYSTANDER SESSIONS spawned beside the messaging and ping-pong worlds'
    /// two (`bystanders=`, default 0, at most [`ATTACH_MAX`]): a node hosting
    /// more than the two that talk, so the bridge's and aterm's walks over every
    /// session (the outbox peek's, the drain's roster read) carry them.
    pub bystanders: usize,
    /// THE PACED SUB-PHASE of the ping-pong arm (`paced=`, default
    /// [`DEFAULT_PACED`], 0 for none): that many more round trips after the
    /// back-to-back ones, each a fixed gap after the last one's reply, so each
    /// finds the bridge's loop idle (`run/pingpong.rs`).
    pub paced: usize,
    /// How often the retaining broker's retention tick runs when no batch asks
    /// for one sooner, in milliseconds (`retention_interval_ms=`, which the
    /// bench hands `Broker::set_retention_interval` after the rules). `None`
    /// keeps astream's minute, what ships. A world lives well under a minute
    /// and the bench's bytes are far below every rule's bound, so without it
    /// no tick lands in a measured window. Only on a retaining broker.
    pub retention_interval_ms: Option<u64>,
}

/// The scenarios measured in one run. Selecting one does not start the others.
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

    /// Read exactly one scenario name.
    ///
    /// # Errors
    ///
    /// A name outside the five supported words.
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

/// The bridge's `--log` mode, as a run names it (`log=auto|off`): `auto`,
/// what ships (owner decision D4), mirrors the session
/// logs' bus class onto the sessions' `ev` faces while the broker retains
/// them; `off` mirrors nothing. The harness passes `--log off` on the bridge's
/// `serve` line for `off`, and nothing for `auto`, the bridge's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Log {
    #[default]
    Auto,
    Off,
}

impl Log {
    /// The word [`parse`] reads, and the report prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Log::Auto => "auto",
            Log::Off => "off",
        }
    }

    /// `auto` or `off`, and nothing else. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// Any other word, quoted.
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

/// aterm's ordered horizon (`aterm_link::durability::ORDERED_HORIZON`, 100 ms),
/// the one aterm opens an ordered broker with, and so the ordered arm's unless
/// it names another. Spelled here because this file is std-only (the lane
/// mounts it); `run.rs` holds the two equal. (Every entry names its own,
/// `horizon_ms=` in its words: those before aterm took 100 ms say 1000.)
pub const ORDERED_HORIZON_MS: u64 = 100;

/// The durability tier the bench opens its in-process broker at — aterm's
/// `aterm_link::durability::Durability`, named here in a std-only type
/// because the lane mounts this file and has no aterm-link dependency
/// (`run.rs` maps it, and opens the broker with aterm's own options for it:
/// the tier and, on a weak one, aterm's escalation table).
///
/// ORDERED IS THE DEFAULT ARM, because it is what aterm opens its guarded
/// broker at by default (`aterm_link::durability::default_for`, which
/// `run.rs` holds this equal to): every ack after an ordering barrier
/// (`F_BARRIERFSYNC`), and a full flush once the horizon
/// ([`Spec::horizon_ms`]) has passed since the first ack no full flush
/// covers — at most the horizon plus a batch and a flush after it — and
/// before the ack of every record aterm's table escalates; served strict by
/// astream off macOS and off the boot volume, which the entry's broker line
/// then says. A bare `verify perf fabric` therefore measures the
/// configuration aterm ships. STRICT is the baseline the default was gated
/// against, and every entry's yardstick unit: every ack after a full flush
/// (`F_FULLFSYNC` on macOS), what a person's message costs on a strict
/// broker, and the default arm of every entry written before the 2026-10-20
/// re-gate: compare a bare entry with one of those through
/// `durability=strict`, never bare to bare. RELAXED is the ceiling: the
/// batch's page-cache write and no flush at all, the bound a cheaper durable
/// tier could approach and never pass. All three go through the same guarded
/// open (the same capability checks); the bridge's own state-file writes are
/// not the broker's and stay as they are in every arm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tier {
    Strict,
    #[default]
    Ordered,
    Relaxed,
}

impl Tier {
    /// The word [`parse`] reads, and the report prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Tier::Strict => "strict",
            Tier::Ordered => "ordered",
            Tier::Relaxed => "relaxed",
        }
    }

    /// `strict`, `ordered` or `relaxed`, and nothing else. Pure
    /// (unit-tested).
    ///
    /// # Errors
    ///
    /// Any other word, quoted.
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

/// The defaults: what `verify perf fabric` runs with no option.
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

    /// The spec as the words [`parse`] reads, for the report: an ordered
    /// arm's with its horizon, so the entry says which arm it is and
    /// reproduces it.
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

/// Parse `out=<dir> [samples=<n>] [warmup=<n>] [burst=<n>] [pingpong=<n>]
/// [attach=<n>,<n>,…] [rounds=<n>] [bystanders=<n>] [paced=<n>] [retention=on|off] [log=auto|off]
/// [retention_interval_ms=<n>] [durability=strict|ordered|relaxed]
/// [horizon_ms=<n>] [scenario=all|messaging|pingpong|attach|handoff]`. Pure (unit-tested).
///
/// # Errors
///
/// A word that is not one of those, a count that is not a positive integer
/// (`warmup` and `horizon_ms` may be 0), a durability that is not a
/// [`Tier`], a horizon for a tier that is not ordered, a retention interval
/// on a broker that does not retain, a burst past [`BURST_MAX`], an attach
/// list that does not rise or passes [`ATTACH_MAX`], or no `out=`.
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
            // The report's words carry them, so an entry says which broker
            // and which mirror it measured, and reproduces them.
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
            // A tick is a retaining broker's alone.
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
            // The report's words carry the tier, so an entry says what its
            // acks promised and reproduces it.
            assert!(spec.words().ends_with(&format!(" durability={word}")));
            let again = parse(&format!("out=/o {}", spec.words())).expect("round trip");
            assert_eq!(again, spec);
        }
        // Ordered carries its horizon: aterm's unless named, and the words say
        // it, so the 1 s and the 100 ms arms read apart.
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
        // A horizon is the ordered tier's alone — the default arm's, unnamed.
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
        // Only the three words, spelled exactly: a near miss is refused rather
        // than measured at a tier nobody asked for.
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
    /// the ping-pong arm's paced rounds. Both reach the words and reproduce;
    /// a bystander count past what one instance carries is refused.
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
        // NEGATIVE CONTROLS.
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
