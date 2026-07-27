// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-suggest` — the inline-suggestion (**ghost text**) engine.
//!
//! aterm already knows, for free and with no extra plumbing, everything a
//! world-class autosuggestion needs. The shell integration
//! (`aterm-shell-integration`) emits OSC 133 A/B/C/D and OSC 633;E from zsh,
//! bash, fish and PowerShell, and `aterm-core` folds those into
//! `OutputBlock`-shaped records carrying the **clean command text**, the
//! **working directory**, the **exit code**, and four **timestamps**. This
//! crate is the pure function from that corpus plus "what the user has typed so
//! far" to ONE completion to paint after the cursor.
//!
//! ## Why this is not just fish's autosuggest
//!
//! `fish` and `zsh-autosuggestions` rank history by recency alone, inside one
//! shell. This engine sits in the *terminal*, one level below the shell, and so
//! can use three signals a shell-side implementation structurally cannot:
//!
//! * **Exit status.** A command that has only ever failed is never suggested.
//!   The shell's history file does not record whether the command worked; the
//!   OSC 133;D marker does.
//! * **Working directory.** `cargo test` in the repo you actually ran it in
//!   outranks the same string from somewhere else, because OSC 7 / OSC 633
//!   pinned the cwd to the block.
//! * **One corpus across every shell and pane.** The record is made from the
//!   PTY stream, so zsh in one tab and bash in another feed the same history —
//!   and a remote shell whose integration is installed feeds it too.
//!
//! ## Purity (the house discipline)
//!
//! No clock, no I/O, no allocation on the query path beyond the returned
//! completion. Every entry point takes the caller's `now_ms`, so the whole
//! ranker is unit-testable without sleeping, and the same corpus plus the same
//! `now_ms` always yields the same suggestion — the property the tests pin.
//! Timestamps are **milliseconds since the Unix epoch**, exactly the unit
//! `OutputBlock` already records, so no clock domains are mixed.
//!
//! ## Safety is in the engine, not the caller
//!
//! A terminal is not a text field, and a wrong ghost is worse than none: it is
//! read, evaluated, and discarded, and that costs more attention than typing
//! the characters would have. Every gate that makes a suggestion safe lives in
//! [`Engine::suggest`] and is driven by the [`Context`] the host supplies, so a
//! host cannot forget one. See [`Context`] for what each field must mean.

use std::collections::VecDeque;

mod ghost;
mod host;
pub use ghost::Ghost;
pub use host::{BlockRecord, line_buffer};

/// How aggressively to suggest. Parsed from the `inline_suggest` config string.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SuggestMode {
    /// Never suggest. The default, so a default-constructed engine is inert
    /// until config opts in — the same fail-safe posture as `PredictMode::Off`
    /// in `aterm-predict`.
    #[default]
    Off,
    /// Suggest from the recorded command corpus.
    History,
}

impl SuggestMode {
    /// Parse the config string (case-insensitive); unknown ⇒ `Off` (fail safe).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "history" | "on" | "true" => Self::History,
            _ => Self::Off,
        }
    }
}

/// Where a suggestion came from. Carried on [`Suggestion`] so the host can
/// style sources differently and so telemetry can measure per-source accept
/// rates — the only honest way to tune the ranker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// A previously-run command from the recorded corpus.
    History,
}

/// One recorded command. Deduplicated by exact command text: re-running a
/// command bumps its counters rather than appending, so a corpus of `N` entries
/// is `N` *distinct* commands and the ranker never has to collapse duplicates
/// at query time.
#[derive(Clone, Debug)]
struct Entry {
    command: Box<str>,
    /// The cwd of the most recent run **that reported one**. One slot, not a
    /// set: the ranker only asks "was this last run where I am now", and keeping
    /// a set would grow unboundedly for commands run everywhere (`ls`,
    /// `git status`).
    ///
    /// A later run with no cwd (a block whose OSC 7 never arrived) does NOT
    /// clear it — absence of a report is not evidence the directory changed, and
    /// forgetting would silently drop the strongest ranking signal the terminal
    /// has for a command it has otherwise just seen succeed.
    cwd: Option<Box<str>>,
    /// Runs that exited 0.
    successes: u32,
    /// Runs that exited non-zero.
    failures: u32,
    /// Epoch-ms of the most recent run, successful or not.
    last_used_ms: u64,
}

impl Entry {
    /// Has this command ever worked? A command that has only ever failed is
    /// never suggested (see [`Engine::suggest`]) — suggesting a known-broken
    /// command line is worse than suggesting nothing, because the user pays the
    /// cost of running it to rediscover that it fails.
    fn ever_succeeded(&self) -> bool {
        self.successes > 0
    }
}

/// Everything the engine needs to decide whether — and what — to suggest.
///
/// Each field is a **safety gate**, not a hint. The host reads them from the
/// terminal it already has; the engine refuses rather than guessing when any of
/// them says the context is wrong.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// What the user has typed on this line so far.
    ///
    /// The host derives this by reading the grid from the in-progress block's
    /// `command_start_row`/`command_start_col` (OSC 133;B) to the cursor —
    /// **not** by modeling readline. Reading the screen is what makes this
    /// correct through history recall, `Ctrl-W`, arrow keys, `Ctrl-R` and every
    /// other line-editor operation the terminal never sees as such: whatever
    /// those did, the result is on the glass.
    pub buffer: &'a str,
    /// Current working directory (OSC 7 / OSC 633;P), used only for ranking.
    pub cwd: Option<&'a str>,
    /// The shell is between OSC 133;B (input started) and OSC 133;C (executing).
    ///
    /// This is the load-bearing gate: it is the ONLY positive evidence that the
    /// bytes on this line are a shell command line and not a `read` prompt, a
    /// REPL, an `ssh` password, or a full-screen program's own UI. Without a
    /// 133;B in hand the host must pass `false` — a heuristic "looks like a
    /// prompt" detector *will* be wrong, and being wrong here means painting a
    /// shell command into someone's Python REPL or `psql` session.
    pub at_prompt: bool,
    /// The alternate screen is active (vim, less, htop, fzf, tmux).
    ///
    /// There the application owns every cell and every keystroke is a command,
    /// so a ghost suffix is corruption. Mirrors `aterm-predict`'s alt-screen
    /// gate; kept as a separate field from `at_prompt` because they can
    /// disagree (a full-screen program launched from a prompt whose 133;C the
    /// shell never sent).
    pub alt_screen: bool,
    /// The line is echoing what the user types.
    ///
    /// `false` at a password prompt (`read -s`, `sudo`, `ssh`, pinentry). A
    /// suggestion on a non-echoing line would paint text the user cannot see
    /// themselves typing — and a completion drawn from history next to a
    /// password field is a security incident, not a feature. The host supplies
    /// the same signal `aterm-predict` derives for its epoch gate.
    pub echoing: bool,
}

/// One completion, ready to paint immediately after the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The text to paint AFTER the buffer — the completion only, never the
    /// buffer itself. Concatenating `ctx.buffer` with this yields the full
    /// command, which is exactly the byte string an accept must write to the
    /// PTY.
    pub completion: Box<str>,
    /// Which corpus produced it.
    pub source: Source,
}

/// Tuning for [`Engine`]. Defaults are the shipping values; the fields are
/// public so the config surface and the tests can move them independently.
#[derive(Clone, Copy, Debug)]
pub struct SuggestConfig {
    /// Display mode. `Off` ⇒ the engine never suggests.
    pub mode: SuggestMode,
    /// Maximum distinct commands retained. The corpus is a bounded ring: the
    /// least-recently-used entry is evicted, so memory is `O(cap)` forever.
    pub capacity: usize,
    /// Minimum typed characters before anything is suggested.
    ///
    /// Not zero: on an empty line every command in history is a prefix match,
    /// so the "suggestion" is really just the most recent command, which is
    /// noise the user did not ask for and must visually discard on every single
    /// prompt. Two characters is enough to make the match meaningful.
    pub min_prefix: usize,
    /// Minimum score a candidate must reach to be offered at all.
    ///
    /// Without a floor the ranker paints its best candidate however bad it is —
    /// and a suggestion that is often wrong is *worse* than none, because the
    /// user must read and evaluate it every time, which costs more attention
    /// than typing the characters would have saved. Precision here is a cliff,
    /// not a slope: a ghost you can accept reflexively is worth a lot, and one
    /// you must verify is worth less than nothing.
    ///
    /// The default is `0.0`, which reads as: **the evidence for a command must
    /// outweigh the evidence against it.** Recency and frequency are positive
    /// and the failure penalty is negative, so a clean command clears it at any
    /// age — a prefix match on something you once ran successfully is already a
    /// high-precision signal, and dropping week-old commands would gut the
    /// feature — while a flaky one has to earn its place with recency or a cwd
    /// match. A three-week-old entry with five failures scores about −1.3 and is
    /// silently dropped; the same entry run here today is not.
    ///
    /// Raise it to demand more before anything is painted.
    pub min_score: f32,
    /// Half-life of the recency score, in milliseconds. A command run this long
    /// ago scores half what it would have scored fresh. 24 h by default: long
    /// enough that yesterday's work is still ranked, short enough that today's
    /// wins.
    pub recency_half_life_ms: u64,
}

impl Default for SuggestConfig {
    fn default() -> Self {
        Self {
            mode: SuggestMode::Off,
            capacity: 2000,
            min_prefix: 2,
            min_score: 0.0,
            recency_half_life_ms: 24 * 60 * 60 * 1000,
        }
    }
}

/// Weight of the frequency term relative to recency. Deliberately below the
/// recency weight: what you are doing *now* predicts the next command better
/// than what you have historically done most, and a frequency-dominant ranker
/// gets stuck suggesting `ls` forever.
const W_FREQUENCY: f32 = 0.35;
/// Weight of the cwd match. Large, because directory is the single strongest
/// context signal a terminal has: `cargo test` means this repo, `make` means
/// this project. This is the term a shell-side autosuggest structurally cannot
/// compute.
const W_CWD: f32 = 0.60;
/// Weight of the recency term (normalized to `[0, 1]` by the half-life decay).
const W_RECENCY: f32 = 1.0;
/// Penalty applied per failure, capped by [`FAILURE_PENALTY_CAP`]. A command
/// that usually works but failed once is still worth suggesting; one that fails
/// most of the time is not.
const W_FAILURE: f32 = 0.5;
/// Ceiling on the accumulated failure penalty so a single pathological entry
/// cannot dominate the score space.
const FAILURE_PENALTY_CAP: f32 = 1.5;

/// The suggestion engine for one session.
///
/// Cheap to construct and inert in [`SuggestMode::Off`]. Holds only the bounded
/// corpus, so an idle engine costs nothing per frame and `O(capacity)` at rest.
#[derive(Debug)]
pub struct Engine {
    cfg: SuggestConfig,
    /// Bumped by every mutation of the corpus or config.
    ///
    /// [`Ghost`]'s incremental path deliberately never re-consults the engine
    /// while a suggestion keeps matching, so without a version stamp a standing
    /// ghost outlives the corpus it came from: `clear()` — the user-facing
    /// "forget my history" control — left the offending completion on glass, and
    /// a command completing mid-line could not change the ranking until the
    /// user diverged. The ghost compares this against the value it last scanned
    /// at and falls back to a full rescan when they differ.
    generation: u64,
    /// Most-recently-used LAST, so eviction pops the front and the common case
    /// (re-running the previous command) touches the back.
    entries: VecDeque<Entry>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(SuggestConfig::default())
    }
}

impl Engine {
    /// An engine with `cfg`.
    #[must_use]
    pub fn new(cfg: SuggestConfig) -> Self {
        Self {
            cfg,
            generation: 0,
            entries: VecDeque::new(),
        }
    }

    /// The corpus/config version — see the field docs. A change means any
    /// standing suggestion must be recomputed rather than merely narrowed.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Apply a (possibly changed) config. Switching to [`SuggestMode::Off`]
    /// keeps the corpus: the user may toggle the feature back on, and a corpus
    /// rebuilt from scratch would suggest nothing useful for hours. Nothing is
    /// painted while `Off`, which is the property that matters.
    pub fn set_config(&mut self, cfg: SuggestConfig) {
        self.cfg = cfg;
        self.generation = self.generation.wrapping_add(1);
        self.trim();
    }

    /// The current config.
    #[must_use]
    pub fn config(&self) -> SuggestConfig {
        self.cfg
    }

    /// Number of distinct commands retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget everything. Wired to the user-facing "clear typing history"
    /// control: a corpus of everything you have ever typed must be erasable in
    /// one action, or the feature is a keylogger with extra steps.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Record one completed command block.
    ///
    /// `exit_code` is `None` for a block whose 133;D never arrived (the shell
    /// died, the pane closed); such a run counts for recency but as neither a
    /// success nor a failure, because we genuinely do not know.
    ///
    /// Commands that [`looks_secret`] flags are **dropped, not stored**. The
    /// corpus is in-memory only today, but it is filtered at the point of
    /// RECORD rather than at the point of suggest, so that stays true if it ever
    /// gains a backing file: a token pasted onto a command line must never reach
    /// the corpus in the first place.
    pub fn record(&mut self, command: &str, cwd: Option<&str>, exit_code: Option<i32>, at_ms: u64) {
        self.generation = self.generation.wrapping_add(1);
        let command = command.trim();
        // A blank line is a prompt the user dismissed with Enter, not a command.
        if command.is_empty() || command.contains('\n') || looks_secret(command) {
            return;
        }
        let success = exit_code == Some(0);
        let failure = exit_code.is_some_and(|c| c != 0);
        if let Some(pos) = self.entries.iter().position(|e| &*e.command == command) {
            // Move-to-back keeps the deque in LRU order so `trim` can evict the
            // front unconditionally.
            let mut e = self
                .entries
                .remove(pos)
                .expect("position() returned an in-range index");
            e.successes = e.successes.saturating_add(u32::from(success));
            e.failures = e.failures.saturating_add(u32::from(failure));
            e.last_used_ms = at_ms;
            if cwd.is_some() {
                e.cwd = cwd.map(Box::from);
            }
            self.entries.push_back(e);
        } else {
            self.entries.push_back(Entry {
                command: Box::from(command),
                cwd: cwd.map(Box::from),
                successes: u32::from(success),
                failures: u32::from(failure),
                last_used_ms: at_ms,
            });
        }
        self.generation = self.generation.wrapping_add(1);
        self.trim();
    }

    /// Evict least-recently-used entries down to the configured capacity.
    fn trim(&mut self) {
        while self.entries.len() > self.cfg.capacity {
            self.entries.pop_front();
        }
    }

    /// Whether this context and buffer are eligible for a suggestion at all —
    /// everything decidable WITHOUT consulting the corpus.
    ///
    /// Split out because [`Ghost`] must apply exactly these refusals on its
    /// incremental path, which skips [`suggest`](Self::suggest) entirely. When
    /// they lived inline, narrowing kept a ghost that a full rescan would have
    /// dropped — typing a space held the old completion on glass, and a
    /// completion whittled down to a lone blank stayed "visible". One predicate,
    /// two callers, no drift.
    #[must_use]
    pub fn accepts_context(&self, ctx: &Context<'_>) -> bool {
        if self.cfg.mode == SuggestMode::Off {
            return false;
        }
        // Every one of these is a refusal to guess, not a heuristic. See the
        // field docs on `Context` for why each is load-bearing.
        if !ctx.at_prompt || ctx.alt_screen || !ctx.echoing {
            return false;
        }
        let buffer = ctx.buffer;
        // A buffer with a newline is a multi-line construct (a heredoc, a
        // continued pipeline); the corpus is single-line commands, so any match
        // would be against the wrong thing.
        if buffer.contains('\n') || buffer.chars().count() < self.cfg.min_prefix {
            return false;
        }
        // Trailing-whitespace buffers match everything with that prefix, which
        // makes the ghost flicker between unrelated commands as the user types
        // a space. Wait for the next real character.
        !buffer.ends_with(char::is_whitespace)
    }

    /// The completion to paint after the cursor, or `None`.
    ///
    /// Refuses — in this order, cheapest gate first — when the feature is off,
    /// the context is not a live echoing shell prompt, or the buffer is too
    /// short to have earned a match. Then returns the single highest-scoring
    /// strict prefix extension.
    #[must_use]
    pub fn suggest(&self, ctx: &Context<'_>, now_ms: u64) -> Option<Suggestion> {
        if !self.accepts_context(ctx) {
            return None;
        }
        let buffer = ctx.buffer;

        let mut best: Option<(f32, &Entry)> = None;
        for e in &self.entries {
            // A strict extension: the entry must start with the buffer and have
            // something left over. Equal strings are not suggestions.
            let Some(rest) = e.command.strip_prefix(buffer) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            // Never offer a command line that has never once worked.
            if !e.ever_succeeded() {
                continue;
            }
            // Never complete INTO a destructive command. The blast radius of one
            // reflexive accept is unbounded and irreversible: typing
            // `git push --force origin ma` meaning `my-feature` and being handed
            // `main` — which ranks higher, because it is the branch you push
            // most — is a one-keystroke history rewrite. Having run it before is
            // not evidence that running it NOW, with this argument, is intended.
            if is_destructive(&e.command) {
                continue;
            }
            let score = self.score(e, ctx.cwd, now_ms);
            // A candidate nobody would want is not a candidate. See
            // `SuggestConfig::min_score`.
            if score < self.cfg.min_score {
                continue;
            }
            // Deterministic tie-break: strictly-greater keeps the FIRST of an
            // equal-scoring run, and the deque is in LRU order, so ties resolve
            // to the least-recently-used… which is backwards. Compare on
            // `>=` so the later (more recent) entry wins, making the result a
            // pure function of the corpus and independent of iteration luck.
            if best.is_none_or(|(b, _)| score >= b) {
                best = Some((score, e));
            }
        }

        let (_, e) = best?;
        let completion = e
            .command
            .strip_prefix(buffer)
            .expect("the winner matched this prefix during scoring");
        // A ghost is painted one cell per character with `wide = false`, so a
        // double-width glyph would corrupt the cells to its right, and a control
        // character has no cell geometry at all. Truncate rather than refuse:
        // the ASCII head of a completion is still useful, and this mirrors the
        // echo lane's single-width rule (`aterm-predict` lib.rs:341).
        let completion = match completion
            .char_indices()
            .find(|(_, c)| c.is_control() || aterm_grapheme::char_width(*c) != 1)
        {
            Some((i, _)) => &completion[..i],
            None => completion,
        };
        // An all-blank remainder is not a suggestion. It can arise from the
        // truncation above (`cd 世界` truncates to a lone space): it paints no
        // visible glyph, yet it would still latch `sugg_shown` and force a
        // repaint per keystroke — cost with no pixels.
        if completion.trim().is_empty() {
            return None;
        }
        Some(Suggestion {
            completion: Box::from(completion),
            source: Source::History,
        })
    }

    /// Rank one candidate. Higher is better; the terms are documented on the
    /// `W_*` constants.
    fn score(&self, e: &Entry, cwd: Option<&str>, now_ms: u64) -> f32 {
        // Recency: exponential decay to `[0, 1]`, halving every half-life. A
        // future timestamp (clock skew between a remote shell and this host)
        // saturates at 1.0 rather than going superscalar.
        let age_ms = now_ms.saturating_sub(e.last_used_ms) as f32;
        let half_life = self.cfg.recency_half_life_ms.max(1) as f32;
        // `0.5^y` written as `2^-y`: exactly equal, and `exp2` is a direct
        // intrinsic where `powf` goes through a log/exp pair. This runs once per
        // corpus entry on every full rescan (up to `capacity`), so it is the
        // hottest arithmetic in the crate.
        let recency = (-(age_ms / half_life)).exp2();

        // Frequency: log-shaped so the 50th run of a command is not 50x the
        // first. Normalized by a nominal 32 runs.
        let runs = e.successes.saturating_add(e.failures).max(1) as f32;
        let frequency = (runs.ln() / 32f32.ln()).min(1.0);

        // Directory: an exact match of the cwd this command last ran in.
        let cwd_match = match (cwd, e.cwd.as_deref()) {
            (Some(here), Some(there)) if here == there => 1.0,
            _ => 0.0,
        };

        let failure_penalty = (e.failures as f32 * W_FAILURE).min(FAILURE_PENALTY_CAP);

        W_RECENCY * recency + W_FREQUENCY * frequency + W_CWD * cwd_match - failure_penalty
    }
}

/// Is this command line destructive enough that a one-keystroke accept is the
/// wrong interaction for it?
///
/// **Non-configurable by design.** This is not a preference: the whole value of
/// an inline suggestion is that accepting it is reflexive, and reflex is exactly
/// the wrong mode for an irreversible command. A user who wants `rm -rf
/// build/` back can press Up. The list is keyed on the first token, plus the
/// handful of subcommand forms where the verb alone is harmless and the flag is
/// not (`git push --force`, `kubectl delete`).
///
/// Deliberately over-broad on the first token: `rm` is denied even for
/// `rm notes.txt`, because the ranker cannot know which `rm` a prefix will
/// resolve to and the failure is not recoverable.
#[must_use]
pub fn is_destructive(command: &str) -> bool {
    // ANY segment of a compound line can be the destructive one. Checking only
    // the head let `make clean && rm -rf target` through — and that string is
    // exactly the kind of thing that ends up in a history corpus and then one
    // keystroke away. Splitting on the bare separator characters also splits
    // inside quotes (`echo "a|b"`), which over-matches; over-matching costs a
    // suggestion, under-matching costs a directory.
    command
        .split(['\n', ';', '&', '|'])
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .any(segment_is_destructive)
}

/// Is this ONE simple command (no separators) destructive? See
/// [`is_destructive`], which owns the compound-line split.
fn segment_is_destructive(segment: &str) -> bool {
    /// Commands whose ordinary use is destructive or irreversible.
    const DESTRUCTIVE_HEADS: &[&str] = &[
        "rm", "rmdir", "dd", "mkfs", "shred", "fdisk", "parted", "srm", "wipefs",
    ];
    let mut tokens = segment.split_whitespace().peekable();
    // `TMPDIR=/x rm -rf /srv` — a leading run of `NAME=value` assignments is
    // environment, not the command.
    while tokens
        .peek()
        .is_some_and(|t| t.split_once('=').is_some_and(|(n, _)| is_env_name(n)))
    {
        tokens.next();
    }
    let Some(head) = tokens.next() else {
        return false;
    };
    // Compare on the basename so `/bin/rm` is caught too.
    let base = head.rsplit('/').next().unwrap_or(head);
    // `mkfs` by PREFIX: nobody types bare `mkfs`, they type `mkfs.ext4` /
    // `mkfs.apfs`. Exact-matching it meant `sudo mkfs.ext4 /dev/sd…` was one
    // reflexive keystroke away from formatting a real device.
    if DESTRUCTIVE_HEADS.contains(&base) || base.starts_with("mkfs.") {
        return true;
    }
    // A privilege wrapper is judged by what it wraps — after its OWN flags.
    // `sudo -u bob rm -rf /srv` used to read `-u` as the command and pass.
    if matches!(base, "sudo" | "doas" | "run0") {
        skip_flags(&mut tokens, &["-u", "-g", "--user", "--group"]);
        let rest: Vec<&str> = tokens.collect();
        return !rest.is_empty() && segment_is_destructive(&rest.join(" "));
    }
    // Global flags precede the subcommand: `git -C /repo push --force …`.
    if matches!(base, "git" | "kubectl" | "oc" | "docker" | "podman") {
        skip_flags(&mut tokens, &["-C", "-c", "-n", "--namespace", "--context"]);
    }
    let rest: Vec<&str> = tokens.collect();
    match base {
        "git" => match rest.first() {
            // `git push --force` / `--force-with-lease` rewrites remote history.
            Some(&"push") => rest.iter().any(|t| t.starts_with("--force") || *t == "-f"),
            // `git reset --hard` and `git clean -f` discard uncommitted work
            // with no undo — the same "irreversible, and you meant the other
            // branch" shape as a force push.
            Some(&"reset") => rest.contains(&"--hard"),
            Some(&"clean") => rest
                .iter()
                .any(|t| t.starts_with("-f") || t.starts_with("--force")),
            _ => false,
        },
        "kubectl" | "oc" => rest.first() == Some(&"delete"),
        "terraform" | "tofu" => matches!(rest.first(), Some(&"apply") | Some(&"destroy")),
        // `prune` is matched ANYWHERE in the subcommand chain: the real
        // spellings are `docker system prune` / `image prune` / `volume prune`.
        "docker" | "podman" => {
            matches!(rest.first(), Some(&"rm") | Some(&"rmi")) || rest.contains(&"prune")
        }
        _ => false,
    }
}

/// Is `name` a shell-legal environment-variable name (so `name=value` is an
/// assignment prefix rather than the command itself)?
fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Consume a leading run of `-flags`, also consuming the VALUE of any flag in
/// `valued` (`-u bob`, `-C /repo`). Leaves the iterator on the first token that
/// is not part of the flag run — the wrapped command, or the subcommand.
fn skip_flags<'a>(
    tokens: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    valued: &[&str],
) {
    while let Some(tok) = tokens.peek() {
        if !tok.starts_with('-') {
            break;
        }
        let tok = *tok;
        tokens.next();
        // `--flag=value` carries its own value; `-u bob` takes the next token.
        if valued.contains(&tok) {
            tokens.next();
        }
    }
}

/// Does this command line look like it carries a secret?
///
/// Conservative and deliberately crude: the cost of a false positive is one
/// command that never gets suggested, and the cost of a false negative is a
/// credential sitting in a suggestion corpus. When those are the
/// stakes the threshold belongs where it is.
///
/// Matches the shapes credentials actually arrive in on a command line: an
/// explicit secret-named flag or assignment, and the standard token prefixes
/// that are self-identifying.
#[must_use]
pub fn looks_secret(command: &str) -> bool {
    const SECRET_WORDS: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "credential",
        "private_key",
        "private-key",
        "auth",
        "bearer",
    ];
    const SECRET_PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
        "glpat-",
    ];
    // Self-identifying token prefixes are checked case-SENSITIVELY: `AKIA` and
    // `sk-` are literal vendor formats, and lowercasing first would make `akia`
    // in an ordinary word a match.
    if SECRET_PREFIXES.iter().any(|p| command.contains(p)) {
        return true;
    }
    let lower = command.to_ascii_lowercase();
    // A secret WORD only counts next to an assignment or a flag: plain
    // `git push` should not be suppressed because a path contains "auth", but
    // `--token=…`, `PASSWORD=…` and `--password …` all should be.
    SECRET_WORDS.iter().any(|w| {
        lower
            .match_indices(w)
            .any(|(i, _)| assignment_or_flag_context(&lower, i, w.len()))
    })
}

/// Is the match at `[i, i+len)` in flag/assignment position — i.e. does a value
/// actually follow it?
fn assignment_or_flag_context(lower: &str, i: usize, len: usize) -> bool {
    // Widen to the whitespace-delimited TOKEN holding the match. Both arms below
    // judge the token, not the raw byte after the word: the secret word is
    // routinely an INFIX of the name (`AWS_SECRET_ACCESS_KEY`), so a rule that
    // only looks at the next character misses the commonest leak there is.
    let tok_start = lower[..i].rfind(char::is_whitespace).map_or(0, |p| p + 1);
    let tok_end = lower[i + len..]
        .find(char::is_whitespace)
        .map_or(lower.len(), |p| i + len + p);
    let token = &lower[tok_start..tok_end];
    let rel = i - tok_start;
    // Split the token at its FIRST `=`: everything before is a name, after is a
    // value. `--token=…`, `PGPASSWORD=…`, `SECRET_KEY=…`, `TF_VAR_db_password=…`.
    let (name, value) = match token.find('=') {
        Some(eq) => (&token[..eq], Some(&token[eq + 1..])),
        None => (token, None),
    };
    // A match in the VALUE half is the secret's own text, not a name; ignore it.
    if rel + len > name.len() {
        return false;
    }
    if value.is_some() {
        // Arm 1 — assignment. Decisive on its own, wherever in the name the word
        // sits: an `=` after a secret-named variable is how credentials arrive.
        return true;
    }
    // Arm 2 — flag or positional whose value is the NEXT token
    // (`--password hunter2`, `aws configure set aws_secret_access_key wJalr…`).
    // Here the word must be a whole sub-word of an identifier-shaped name,
    // delimited by `_`/`-` or the ends of the token. That is what keeps
    // `cd src/auth` (a path: `/` is not a name character), `vim tokens.rs`
    // (`token` is not a sub-word of `tokens`) and `git commit -m 'fix auth'`
    // (`auth'` is prose, not a name) out of the suppression net.
    let sep = |c: char| c == '_' || c == '-';
    let identifier = name.chars().all(|c| c.is_ascii_alphanumeric() || sep(c));
    let left_ok = rel == 0 || name[..rel].chars().next_back().is_some_and(sep);
    let right_ok = rel + len == name.len() || name[rel + len..].chars().next().is_some_and(sep);
    identifier && left_ok && right_ok && !lower[tok_end..].trim().is_empty()
}

#[cfg(test)]
mod tests;
