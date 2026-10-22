// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-Claude lab: the REAL Claude Code TUI (or Codex, or Muse Code),
//! launched by path inside a PRIVATE headless aterm and talking to a local
//! STUB of the Anthropic API (or of the Responses API, or of Meta's). No
//! tokens, no login, no egress: everything a lab starts runs in one sandbox
//! profile. It exists because stand-in TUIs and hand-built frames hid defects
//! the real TUI shows, and a real session spends tokens, needs a login and
//! cannot be pushed into a wall (529, a usage limit, a full context) on demand.
//! Read each new Claude Code build here (`tool`, `settings`, `phase`) before
//! the harness presses anything on it. Its measurements and the rulings made
//! from them are recorded in `docs/measured/real-claude-lab-2026-10-15.md`;
//! the account-stats facts of 2.1.301 (what a subscriber's Claude Code reads
//! its plan and limits from, and what reaches a stub) in [`stats`]. The real
//! Codex runs in it too ([`codex`]), and the real Gemini CLI and Antigravity
//! CLI (`agy`) against the stub's Gemini API ([`gemini`]; their screens:
//! [`gemini_captures`]). A `docs/…` path cited here and in the modules names
//! a record in the development repository; the public source does not carry
//! `docs/`.
//!
//! macOS only (`sandbox-exec`), the managed Claude Code (`aterm pkg install
//! claude`), and an aterm built with the dev seams:
//!
//! ```sh
//! targo --unverified build -p aterm --features dev-seams   # target/debug/aterm; or LAB_ATERM=/path/to/aterm
//! targo --unverified lab selftest                          # about 25 s; exit 1 when a check fails
//! targo --unverified lab nudge-trial                       # about 2.5 min: the repaint nudge against the real Claude Code
//! targo --unverified lab help                              # the verbs
//! ```
//!
//! (`targo --unverified lab …` is the `.cargo/config.toml` alias for `run -q
//! -p aterm-lab --`.) Run it from an unsandboxed shell: sandbox profiles do not
//! nest, and `up` refuses otherwise.
//!
//! # Where things are
//!
//! State is untracked, in `$LAB_STATE` (default
//! `/private/tmp/at.lab-<uid>/<checkout>-<hash>/`, one per checkout, outside
//! every home: the jail fences the homes' contents whole, and the harness's
//! tape store opens each directory from `/` down to a run's state root, so a
//! `LAB_STATE` inside a home is refused; the default is made this user's
//! private directory, a link or another user's directory in its place
//! refused; macOS clears `/private/tmp` of what nobody used for days, so a
//! `LAB_STATE` keeps what must last): `runs/<tag>/` a lab that is up, `logs/<tag>-<time>/` what
//! `down` keeps (stub log, the tripwire's log with the account masked, the
//! egress witness, the harness journal, the jail profile, the aterm log, the
//! relay's log, the settings witness's log, and what the [`stats`] readers read: Claude Code's debug log,
//! the status line's records, the scratch `.claude.json` as `claude.json` and
//! the transcripts under `claude-projects/`, and the sleep clock's offset and
//! records under `clock/`; local paths and all, so look
//! before sharing one), `captures/`
//! fixtures and PNGs, `results/` scenario and trial runs. `logs/` and
//! `results/` only grow. `paths` prints the state directory and the aterm
//! under test.
//!
//! Inside `runs/<tag>/` the jail keeps two kinds of file apart. `scratch/` is
//! the ONE place jailed code may write (`home/`, `cfg/`, `state/`, `dump/`,
//! `modes/`, `mode`, `reply.txt`, `tool.json`, `headers.json`, `account.json`, `stub.log`,
//! `stub.port`, `security.log`, `relay.log`, `statusline.jsonl`, `claude-debug.log`,
//! `config-change.log` (the settings witness's: [`settings_witness`]), and for
//! Codex `codex-home/`, `open.log`,
//! `open-control.log`, `control-home/`, for Gemini CLI and agy `gemini-home/`, `agy-home/`
//! and their `TMPDIR`, `tmp/`, the sleep clock's records `clock/`); its directories stay where `up` made
//! them ([`protected::SCRATCH_DIRS`]), and the lab reads and writes in them
//! through no link ([`sys::scratch_read`], [`sys::put`]). Everything else
//! (`env.sh`, `jail.sb`, `jailed.pids`, the witness logs `sockets.log`,
//! `egress.log`, `members.txt` and `runtime.txt`, the launchers `claude.sh`, `codex.sh`,
//! `codex-run.sh`, `gemini.sh`, `agy.sh`, `muse.sh`, `apikey.sh` and `config-change.sh` (the settings
//! witness's `ConfigChange` hook), `bin/aterm-lab` — the copy of this program
//! the stub, the witness, the tripwire and the open recorder run —
//! `shim/security`, `shim/open`, `shim/aterm` (the aterm under test, so the
//! launched agent's `aterm ctl …` runs it; a session the instance spawns lists
//! the shim last on the PATH its login shell rebuilds), `relay/node`, the
//! sleep clock's `clock/` with its library, offset and Claude Code clone) is
//! written from OUTSIDE the jail and cannot be written, replaced or linked
//! over from inside it.
//!
//! The aterm under test is `$LAB_ATERM` (its build's physical path: not
//! relative, no link, `.` or `..` on it, which a jailed process could
//! re-point), else the `aterm` beside this program, else `target/debug/aterm`
//! or `target/release/aterm` of this checkout, resolved ONCE by `up` and
//! recorded in `env.sh`: every later call for that lab runs that file, which
//! its jail pins. It must honour `ATERM_STATE_HOME` (a dev seam: a debug build
//! has it, an optimized one needs `--features dev-seams`); `up` refuses an
//! instance that did not take it, which would otherwise reach for the owner's
//! live state. Claude Code is the store binary `aterm pkg which claude` names
//! (the last path of its arrow chain), or `$LAB_CLAUDE`, launched BY PATH, not
//! through the managed shim; its `--version` runs inside the jail too. Codex is
//! the same for `aterm pkg which codex`, or `$LAB_CODEX`; a lab without one
//! runs Claude Code only, and its version is read from its package's
//! `codex-package.json`. Gemini CLI is `$LAB_GEMINI`, else Homebrew's
//! `/opt/homebrew/bin/gemini` (by its physical path, its version from its
//! package's `package.json`); agy is `$LAB_AGY`, else what `aterm pkg which
//! agy` names (its version from atpkg's record beside its build); Muse Code is
//! `$LAB_MUSE`, else the managed store's `muse` (`aterm pkg which muse`), and
//! its version is the build record atpkg keeps beside it ([`muse`]). No agent
//! runs outside the jail, `--version` included.
//!
//! # Use
//!
//! ```sh
//! L='targo --unverified lab'
//! $L up mytag                 # stub + black-hole proxy + jailed private headless aterm 144x50
//! $L claude mytag             # the launch line typed into the first tab; the harness answers the folder-trust dialog
//! $L turn mytag hi            # a prompt; the stub answers "Hi! I am the local lab stub, not a model. (reply N)"
//! $L text mytag trim          # the tab's text        $L image mytag shot.png   # a PNG of the frame
//! $L wait mytag 'regex' 30    # until a row matches (`await match`; exit 1 when none did)
//! $L mode mytag overloaded    # the stub's mode (the `stub` module lists them)
//! $L tool mytag '{"name":"Bash","input":{"command":"touch x"}}'   # the model's next tool use; then `turn mytag toolstart`
//! $L settings mytag '{"permissions":{"ask":["Bash(touch:*)"]}}'   # deep-merged into the scratch settings: a running Claude re-reads it
//! $L phase mytag              # what the hosted loop's reader (`aterm drive phase`) says of the tab
//! $L capture mytag name "what"   # captures/claude-<version>-name.txt (+png), the repo's fixture shape
//! $L catch mytag 'regex' name "what" 30   # the same, the moment a row matches
//! $L gemini mytag             # the real Gemini CLI against the stub's Gemini API (agy mytag: the real agy)
//! $L gemini-captures          # every Gemini CLI screen a harness needs, each lab its own (agy-captures: agy's)
//! $L down mytag               # everything ended by pid, the scratch removed, logs/<tag>-<time>/ kept
//! ```
//!
//! Several tags run side by side (own ports, socket dir, scratch, stub mode).
//! The lab forgets every `ATERM*`, `CLAUDE*`, `ANTHROPIC*`, `ATPKG*`, `CODEX*`,
//! `COPILOT*`, `CURSOR*` variable it inherited, so it works from inside an aterm
//! tab or a Claude session; never use a bare `aterm ctl` on a lab, use `ctl TAG
//! <verb>` ([`door`] says why). It forgets the caller's proxies as well
//! (`http_proxy`, `ALL_PROXY`, … in any case, and `CURL_HOME`), and its
//! requests to the stub take no proxy: what it starts in the jail is handed
//! the black hole as its proxy by name. It runs system programs by absolute
//! path or from `/usr/bin:/bin:/usr/sbin:/sbin` only, hands `xcrun_nocache=1`
//! to everything it starts (the xcrun shims would otherwise run what a
//! per-user cache any process of the user may write names), and works from
//! `/`, never from the caller's directory, which may be the lab's project dir.
//!
//! `turn` types the prompt, presses Enter once the prompt stands in the
//! composer (Codex: its last `›` row; Claude Code: the first row between its
//! last two rules) and exits 0 once the composer has let it go. It exits 1
//! when the prompt never stood there (no Enter is pressed), when the screen
//! could not be read after Enter, or when the prompt still stands after a
//! second Enter, which it presses, once, only while the prompt stands
//! (`codex-cli 0.163.0` once kept `› hi` with no request made after an Enter
//! typed 0.3 s after the prompt).
//!
//! `up` takes its switches from the environment: `LAB_SURVEY=1` (the forced
//! survey flag at launch; see Traps), `LAB_TRUST=1` (the folder trust seeded:
//! no dialog), `LAB_RESTORE=1` (the headless-restore seam, [`restore`]),
//! `LAB_CLOCK=1` (the lab's sleep clock in its instance and its Claude Code,
//! for `sleep`: [`clock`]),
//! `LAB_FIRST_RUN=1` (leave the one-time "Auto mode is now the default" notice,
//! and the "Make auto mode your default permission mode?" nudge a settings
//! `defaultMode` draws),
//! `LAB_HARNESS_KEYS='continue = false'` (extra `[harness]` lines; or `harness
//! TAG continue=false` live), `LAB_ATERM_TOML` (the top of the scratch
//! `aterm.toml`, so a root key applies), `LAB_COLS`/`LAB_LINES`, `LAB_SCALE`,
//! `LAB_PROXY=dead` (the proxy at the dead port 9), `LAB_CLAUDE`, `LAB_CODEX`,
//! `LAB_CODEX_MODEL` (Codex's `model`), `LAB_GEMINI`, `LAB_AGY`, `LAB_MUSE`, `LAB_ATERM`, `LAB_STATE` (checked
//! first: never inside a home, never with a quote or a backslash, never
//! relative), `LAB_INSTANCE_ENV='TMPDIR=/tmp ATERM_DEBUG_PASTE_FILE=…'`
//! (variables for the lab's OWN instance alone, at `up` and at every relaunch
//! of it, recorded in `env.sh`: `TMPDIR` and the `ATERM_DEBUG_*` dev seams
//! only, never a name the lab sets itself; [`lab::instance_env`]). An
//! interrupted `up` takes down what it started.
//!
//! `crash TAG [--gap SECS] [--keep-tree]` crashes a lab's instance (its own,
//! by the pid `env.sh` records) and starts it again on the same state after
//! the gap, by `restore`'s relaunch: with a PTY keeper and `--keep-tree` the
//! agent lives through it, and the next instance's loop follows the first
//! one's books ([`crash`]).
//!
//! `sleep TAG --for SECS [--hold SECS] [--at-act [--rule RULE]]` emulates a
//! sleep of the Mac on a lab brought up with `LAB_CLOCK=1`, whose instance
//! and Claude Code load the lab's sleep clock (`crates/aterm-lab-clock`,
//! [`clock`]): it SIGSTOPs the lab's OWN instance, steps the clock's offset
//! by `SECS` and continues it, at once or the moment the harness journals an
//! act ([`sleep`]). The Mac never sleeps and the system clock never moves.
//!
//! # Credentials and egress
//!
//! `apiKeyHelper` prints a fake `sk-ant-api03-LABLABLAB…` key. `claude TAG
//! --subscriber` takes the helper out of the scratch settings and puts a FAKE
//! `sk-ant-oat01-LABLABLAB…` token in `CLAUDE_CODE_OAUTH_TOKEN` (the launcher
//! only): Claude Code then runs as a claude.ai subscriber, the one path on
//! which it reads a usage limit's headers (`limit`) and its usage figures off
//! every answer (`headers TAG '{"on200":{"anthropic-ratelimit-unified-5h-utilization":
//! "@SEQ:0.40:0.02@",…}}'`, the `stub` module). Neither is a credential
//! anywhere; a Claude Code the harness relaunches does not get the token. With
//! a scratch `CLAUDE_CONFIG_DIR` Claude Code asks the keychain only for
//! `Claude Code-credentials-<8 hex>`, never the owner's un-suffixed item; the
//! tripwire ([`tripwire`]) answers it. `ANTHROPIC_BASE_URL` is the stub;
//! `HTTP(S)_PROXY` is the black hole; `DISABLE_AUTOUPDATER=1` and
//! `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1` are set. Claude Code
//! does not honour the base URL for flags and telemetry: `egress TAG` shows the
//! 1 Hz witness ([`sampler`]), the proxy's refusals and what the unified log
//! kept of the jail's refusals (on macOS 27 never a refused connect or file
//! access, so its silence shows nothing); `lsof TAG` the sockets of the lab's
//! own processes, never machine-wide. Every process in a lab's jail is found by
//! the jail's mark ([`sys::jail_pids`]): an orphan launchd adopted, a
//! `codex-run`'s leftover, one in a session of its own.
//!
//! # Traps
//!
//! * Set the survey flag LIVE, after launch (`survey TAG on`, as `selftest`
//!   does), not at launch: set at launch with the first-run notice on the
//!   screen, the card's head row sits above the 40 rows the hosted loop reads.
//! * Do not set `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` or
//!   `DISABLE_TELEMETRY`: they switch every survey off.
//! * The managed launcher is not the store binary; the lab launches the store
//!   binary by path and exports what it needs itself, so it cannot show what a
//!   launcher's own environment does.
//! * A forced card is shown once per Claude process: every scenario run is a fresh lab.
//! * The harness types `keep going` when its back-off ends (2 min after the
//!   first idle point, then 4, 8 …): `harness TAG continue=false` keeps a lab
//!   quiet. A control-socket write never counts as a person.
//! * Claude Code takes more than five seconds to re-read settings: wait nine
//!   after `settings` before the turn that needs the rule.
//! * A project made under the scratch home sits wherever `LAB_STATE` does (the
//!   default is outside every repository); a `LAB_STATE` in a linked
//!   worktree is inside the repository, and Claude Code reads the MAIN
//!   checkout's `.claude/settings.local.json` for it.
//! * `down` ends everything in the lab's jail, found by its mark (an orphan, a
//!   Claude the harness relaunched, a codex-run's leftover too), and the witness
//!   last, once the jail is empty.
//!
//! # Outside any lab
//!
//! These verbs start a private aterm of their own, with no jail and no stub
//! ([`instance`]: a scratch HOME and XDG dirs, the automatic lanes off, the
//! control socket its only door, a lifeline):
//!
//! * `instance` holds one headless for an agent or a person to drive with
//!   `aterm ctl --sock` until the command ends (`--image <png>` takes one frame);
//! * `trail-gallery` photographs cursor trail styles, one headless per style,
//!   theme and scenario, with each frame's sidecars ([`trail_gallery`]), and
//!   `trail-contrast` (no instance) turns a frame into WCAG contrast numbers
//!   ([`trail_contrast`]): every trail check is measured with these two;
//! * `presence-flap --window` opens ONE window and judges that it causes no
//!   re-grid flap ([`presence`]); `--headless` checks the lane itself;
//! * `meta-keys --window` opens ONE window, selects the U.S., German and
//!   French layouts in turn (putting the person's back after,
//!   [`input_sources`]) and presses Option chords in it as key presses,
//!   judging the bytes its program reads against what each layout types
//!   ([`meta_keys`]);
//! * `predict-live --window` opens ONE window at a time and types the
//!   predictive-editing corpus into a zsh or bash behind `link` (a real
//!   round trip), in the single pane and then a split pane, holding every
//!   frame that painted a prediction to the screen the far end's own output
//!   settles to ([`predict_live`]); `--headless` runs the press path alone;
//! * `aos-repair` opens ONE window and runs Aos M2's live repair in it: the
//!   momentum glow attached under the demonstration fault, typed hot, witnessed
//!   stranded past its cold instant by reads that repaint nothing, repaired to
//!   the shipped code, then cleared, and fresh keys cleared again with no
//!   further replay ([`aos_repair`]); `--control` is the shipped code alone;
//! * `codex-alab-proof` proves the codex-alab native harness end to end on
//!   the real package ([`codex_alab_proof`], its jail, fake model and
//!   stand-ins in [`codex_rig`], its scripted model in [`codex_alab_model`]):
//!   the proven claim and the parked lane, FOREVER CRUISE (a done claim, the
//!   retired token's included, gets the defend prompt and work goes on),
//!   FOREVER LAND (the instruction, unbounded nudges, a landing only at a
//!   clean defense over a clean folder, then quiet, reopened by a person's
//!   turn; a dirty repository not landed until pushed), NEEDS YOU (raised at
//!   once, never blocking), an approval, the question overlay and Land's
//!   answer, a usage wait, two crash relaunches and a carry-on, the re-exec
//!   upgrade, a hold, Autopilot Off (the claim given back, a manager's
//!   `post kind=task` alone over the lab's own bus, the claim again once On),
//!   and a person's own exchange (typed through aterm's development seam
//!   `send|key as=person`) left to them beside a control write's turn that is
//!   continued; about eleven minutes.
//!
//! One verb runs aterm on Linux: `linux` ([`linux`]) is the Linux
//! runtime lane, a lima VM on this Mac with Xvfb, weston, Mutter, Xwayland,
//! a session bus and at-spi2-core, in which [`linux_proof`] runs this
//! checkout's Linux tests on aarch64 and on x86_64 under Rosetta and drives
//! a Linux aterm over its control socket (`docs/LINUX-RUNTIME-LANE.md`).
//! And `windows` ([`windows`]) is the Windows runtime lane, a QEMU VM on this
//! Mac running Windows 11 on arm64, in which this checkout builds and tests
//! with MSVC on aarch64 and on x86_64 under Prism
//! (`docs/WINDOWS-RUNTIME-LANE.md`).
//!
//! `abrew-admission` starts none either: it runs abrew's own admission over
//! Homebrew's full formula and cask indexes ([`abrew_admission`]), the dated
//! totals of `docs/DESIGN-abrew-2026-10-19.md` §3.
//!
//! And `ptyrec` starts none: it runs a program behind a PTY in the caller's
//! own terminal and keeps a take of every byte both ways, the recordings
//! aterm-effects replays ([`relay`], whose loop it is). `link` is that relay
//! with a round trip, each chunk held half of it each way ([`link`]).
//!
//! Verified by: `targo --unverified test -p aterm-lab` (the stub served on
//! loopback, the jail renderer, the readers, the judges, on typed data; the
//! trail probe on synthetic frames),
//! `selftest` (the whole proof against the real Claude Code),
//! `codex-selftest` (the same against the real Codex), `gemini-selftest` and
//! `agy-selftest` (against the real Gemini CLI and agy: nothing reaches
//! Google, the owner's `~/.gemini` untouched), `muse-selftest` (against the
//! real Muse Code: nothing reaches Meta, the owner's `~/.config/muse`
//! untouched, nothing left in the shared `/tmp`, [`muse_proof`]), the
//! capture verbs (`gemini-captures`, `agy-captures`, `muse-captures`),
//! which end on any step of theirs that did not happen, and `workers` (a
//! manager Claude Code running three real Claude Code workers: [`workers`],
//! measured in `docs/measured/workers-lab-2026-10-16.md`).

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
mod abrew_admission;
#[cfg(target_os = "macos")]
mod aos_repair;
#[cfg(target_os = "macos")]
mod band;
#[cfg(target_os = "macos")]
mod clock;
#[cfg(target_os = "macos")]
mod codex;
#[cfg(target_os = "macos")]
mod codex_alab_model;
#[cfg(target_os = "macos")]
mod codex_alab_proof;
#[cfg(target_os = "macos")]
mod codex_proof;
#[cfg(target_os = "macos")]
mod codex_rig;
#[cfg(target_os = "macos")]
mod crash;
#[cfg(target_os = "macos")]
mod disk_lane;
#[cfg(target_os = "macos")]
mod door;
#[cfg(target_os = "macos")]
mod fleet_fixture;
#[cfg(target_os = "macos")]
mod fleet_judge;
#[cfg(target_os = "macos")]
mod fleet_probe;
#[cfg(target_os = "macos")]
mod fleet_sim;
#[cfg(target_os = "macos")]
mod fleet_world;
#[cfg(target_os = "macos")]
mod footers;
#[cfg(target_os = "macos")]
mod forever;
#[cfg(target_os = "macos")]
mod gemini;
#[cfg(target_os = "macos")]
mod gemini_captures;
#[cfg(target_os = "macos")]
mod gemini_proof;
#[cfg(target_os = "macos")]
mod input_sources;
#[cfg(target_os = "macos")]
mod instance;
#[cfg(target_os = "macos")]
mod jail;
#[cfg(target_os = "macos")]
mod lab;
#[cfg(target_os = "macos")]
mod link;
#[cfg(target_os = "macos")]
mod linux;
#[cfg(target_os = "macos")]
mod linux_a11y;
#[cfg(target_os = "macos")]
mod linux_baseline;
#[cfg(target_os = "macos")]
mod linux_browser;
#[cfg(target_os = "macos")]
mod linux_edges;
#[cfg(target_os = "macos")]
mod linux_gone;
#[cfg(target_os = "macos")]
mod linux_guest;
#[cfg(target_os = "macos")]
mod linux_proof;
#[cfg(target_os = "macos")]
mod linux_xwire;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod meta_keys;
#[cfg(target_os = "macos")]
mod muse;
#[cfg(target_os = "macos")]
mod muse_captures;
#[cfg(target_os = "macos")]
mod muse_proof;
#[cfg(target_os = "macos")]
mod nudge;
#[cfg(target_os = "macos")]
mod paths;
#[cfg(target_os = "macos")]
mod predict_live;
#[cfg(target_os = "macos")]
mod presence;
#[cfg(target_os = "macos")]
mod prices;
#[cfg(target_os = "macos")]
mod probe;
#[cfg(target_os = "macos")]
mod procs;
#[cfg(target_os = "macos")]
mod protected;
#[cfg(target_os = "macos")]
mod pyjson;
#[cfg(target_os = "macos")]
mod record;
#[cfg(target_os = "macos")]
mod recorder;
#[cfg(target_os = "macos")]
mod reduce;
#[cfg(target_os = "macos")]
mod relaunch_agy;
#[cfg(target_os = "macos")]
mod relaunch_check;
#[cfg(target_os = "macos")]
mod relaunch_gemini;
#[cfg(target_os = "macos")]
mod relaunch_muse;
#[cfg(target_os = "macos")]
mod relay;
#[cfg(target_os = "macos")]
mod resource_attribution;
#[cfg(target_os = "macos")]
mod restore;
#[cfg(target_os = "macos")]
#[cfg(target_os = "macos")]
mod run;
#[cfg(target_os = "macos")]
mod runtime_dirs;
#[cfg(target_os = "macos")]
mod sampler;
#[cfg(target_os = "macos")]
mod scn;
#[cfg(target_os = "macos")]
mod screen;
#[cfg(target_os = "macos")]
mod selftest;
#[cfg(target_os = "macos")]
mod settings_witness;
#[cfg(target_os = "macos")]
mod sleep;
#[cfg(target_os = "macos")]
mod slot_lane;
#[cfg(target_os = "macos")]
mod stats;
#[cfg(target_os = "macos")]
mod stop;
#[cfg(target_os = "macos")]
mod stub;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod tape;
#[cfg(target_os = "macos")]
mod trail_contrast;
#[cfg(target_os = "macos")]
mod trail_gallery;
#[cfg(target_os = "macos")]
mod tripwire;
#[cfg(target_os = "macos")]
mod upgrade_agy;
#[cfg(target_os = "macos")]
mod upgrade_muse;
#[cfg(target_os = "macos")]
mod util;
#[cfg(target_os = "macos")]
mod verbs;
#[cfg(target_os = "macos")]
mod windows;
#[cfg(target_os = "macos")]
mod windows_desk;
#[cfg(target_os = "macos")]
mod windows_guest;
#[cfg(target_os = "macos")]
mod windows_smoke;
#[cfg(target_os = "macos")]
mod windows_window;
#[cfg(target_os = "macos")]
mod workers;

/// How a person runs this program: every hint that tells them what to type next spells it so.
pub const LAB: &str = "targo --unverified lab";

/// Poll `pred` every 100 ms until it holds (`true`) or `within` passes
/// (`false`); a hang detector for something that must happen, interruptible.
#[cfg(target_os = "macos")]
#[must_use]
pub fn wait_until(within: std::time::Duration, mut pred: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + within;
    loop {
        if pred() {
            return true;
        }
        if std::time::Instant::now() >= end
            || stop::sleep(std::time::Duration::from_millis(100)).is_err()
        {
            return false;
        }
    }
}

/// The verbs, as `help` prints them.
pub const USAGE: &str = "\
aterm-lab -- the real-Claude lab: the REAL Claude Code TUI (or the REAL Codex, Gemini CLI, agy or Muse Code TUI) in a
PRIVATE headless aterm, against a local STUB of the Anthropic API (or of the Responses API, of the Gemini API, or of
Meta's). Fully local, no tokens, no login. Run as `targo --unverified lab <command> …`.

usage: targo --unverified lab <command> [tag] [args]   (tag defaults to $LAB_TAG or \"lab\"; labs with other tags run side by side)
  up [tag]                       stub + black-hole proxy + jailed private headless aterm (env: LAB_SURVEY=1 LAB_TRUST=1
                                 LAB_FIRST_RUN=1 LAB_RESTORE=1 LAB_HARNESS_KEYS='continue = false' LAB_ATERM_TOML LAB_COLS LAB_LINES
                                 LAB_SCALE LAB_PROXY=dead LAB_CLAUDE=/path/to/claude LAB_CODEX=/path/to/codex
                                 LAB_GEMINI=/path/to/gemini LAB_AGY=/path/to/agy LAB_MUSE=/path/to/muse
                                 LAB_ATERM=/physical/path/to/aterm LAB_STATE=/absolute/dir/outside/every/home
                                 LAB_INSTANCE_ENV='TMPDIR=/abs ATERM_DEBUG_<NAME>=V ATERM_DEV_<NAME>=V': handed to the lab's
                                 own instance at every launch of it, recorded at up; TMPDIR and dev seams only, never a name
                                 the lab sets;
                                 LAB_CLOCK=1: the lab's sleep clock loaded into its instance and its Claude Code, for `sleep`)
  claude [tag] [-e K=V]... [--no-wait] [--relay] [--subscriber] [--debug-log] [-- claude args]   launch the managed Claude
                                 Code in the first tab (--relay: behind the PTY relay of nudge-trial, on the fullscreen renderer;
                                 --subscriber: a FAKE claude.ai OAuth token in place of the fake API key, the path on which
                                 Claude Code reads the usage-limit headers: `limit`; --debug-log: its debug log into the
                                 scratch, read with `debuglog`)
  codex [tag] [--signed-out|--chatgpt[=PLAN]] [-e K=V]... [--no-wait] [-- codex args]   launch the managed Codex in the
                                 first tab, by path (codex-alab with --no-daemon), CODEX_HOME=<scratch>/codex-home: the lab provider (the
                                 stub's Responses API, a fake env_key); --chatgpt: a COMPOSED fake ChatGPT auth.json
                                 (user@example.com, PLAN plus) and the refresh pointed at the stub; --signed-out: the default
                                 provider, no auth (the last two turn the harness off: their screens offer a browser sign-in)
  codex-run [tag] [--lab|--chatgpt|--signed-out] -- <args>   a non-interactive Codex command (codex-alab: --no-daemon) in the jail,
                                 where the egress witness watches it as it does the tab (jailed.pids), on that login's config
                                 (the tab's by default): --help, exec, exactly `login status` or `login --help`, ...; any
                                 other login, logout, app, app-server, cloud, update are refused
  codex-auth [tag] [LABEL [ACCOUNT [PLAN]]]  write a composed fake auth.json (default B, acct-lab-1, plus) into the live
                                 CODEX_HOME
  ratelimits [tag] off | PRIMARY SECONDARY [credits=yes|no|unlimited] [balance=N] [reached=TYPE] [promo=TEXT] [active=ID]
                                 the x-codex-* headers every stub answer carries (used percent of the 5h and the weekly
                                 window, `-` for none) and the account usage a --chatgpt Codex polls; the on200/on429
                                 blocks `headers` writes stay
  rollout [tag] [all]            the token_count lines (all: every line) of the tab's newest rollout under the scratch
                                 CODEX_HOME (codex-run's are never taken)
  codex-selftest [tag] [--keep]  the proof for Codex in ~20 s: up, a codex-run turn and one in the tab, egress witness
                                 (both), ~/.codex untouched, the open recorder, the isolation probes on a replica home, twin
                                 refusing a planted link, an orphan in the jail found and witnessed, down and nothing left in
                                 the jail; exits 1 on a failed check
  gemini [tag] [-e K=V]... [--no-wait] [--untrusted] [-- gemini args]   launch Gemini CLI (LAB_GEMINI, else Homebrew's
                                 gemini) in the first tab, by path, against the stub's Gemini API: a scratch HOME whose
                                 settings select the API-key auth (a fake GEMINI_API_KEY, GOOGLE_GEMINI_BASE_URL the stub),
                                 the folder trust (--untrusted: on, so its dialog draws), statistics, telemetry and updates
                                 off; no sign-in, no browser, every other connection refused by the jail
  agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [--setting KEY=VALUE]... [-- agy args]   launch agy
                                 (LAB_AGY, else the managed store's) the same way: a scratch HOME whose settings name the
                                 Gemini API provider and trust the project (--untrusted: no trust; --onboarding: no onboarding
                                 either; --setting: one of agy's own keys beside them, never the provider), its Google hosts
                                 black-holed; a login is refused in either's arguments
  gemini-selftest|agy-selftest [tag] [--keep]   the proof for Gemini CLI or agy in ~30 s: up, a turn in the tab the stub
                                 answers, nothing reached Google (egress witness, the fake key on every request, agy's
                                 Google hosts refused by the black-hole proxy), ~/.gemini untouched, no other file of the
                                 owner's home held, the open recorder, the isolation probes on a replica home, twin, an
                                 orphan, down and nothing left; exits 1 on a failed check
  gemini-captures|agy-captures [--only <regex>]   every screen a harness needs of this Gemini CLI or agy, in the fixture
                                 shape (first-run trust and onboarding, idle, draft, busy, end of turn, interrupted, the slash
                                 menu, exit, the shell, write and edit boxes, the question, the 429/401/400/503/500 walls and
                                 retries; the API's other refusals, 403 404 400 504 and 429s, as custom modes; Gemini CLI's
                                 shell mode, YOLO, details hidden, hint off, a box kept over the composer, several questions,
                                 the MCP, fetch and sandbox-expansion boxes, a 72-column pane's errors), each lab its own (~16
                                 min; --only to narrow)
  muse [tag] [-e K=V]... [--no-wait] [-- muse args]   launch Muse Code in the first tab, by path, with a scratch HOME (its
                                 .config/muse inside), a fake META_API_KEY, --base-url the stub, telemetry and the feature
                                 config off, every proxy the black hole (-e may not replace any of those; -- -c
                                 provider_retry.max_attempts=N sets its retries; --trust-workspace skips its trust box)
  muse-selftest [tag] [--keep]   the proof for Muse Code in ~30 s: up, a turn in the tab the stub answers, nothing
                                 reached Meta (egress witness, the fake key on every request to the stub's Meta routes),
                                 ~/.config/muse, ~/.local/share/muse and ~/Library/Application Support/Muse untouched
                                 (no kernel refusal names them, no Muse Code process holds a file under them, its own
                                 writes under the scratch HOME), no other file of the owner's home held, the open
                                 recorder, the isolation probes on a replica home, twin, an orphan, down and nothing
                                 left, in the jail nor in its shared /private/tmp/tbh-<uid>-rt/muse; exits 1 on a
                                 failed check
  muse-captures [--only <regex>]  Muse Code's screens on this build, each on a lab of its own (~10 min): the trust box,
                                 idle, a draft, busy, the end of a turn, an interrupt, the slash menu, the exit, the shell,
                                 write and edit boxes, the question, a turn ended on each tool's own rows
                                 (tool-row-*), the 429/401/500 walls, /usage and /status
  turn|send|enter|key [tag] ...  type a prompt+Enter | text | Enter | a named key (0, escape, ctrl-c, ...); turn exits 1
                                 unless the composer took the prompt (Enter again, once, while it still stands there)
  mode [tag] [name ['<spec>']]   stub mode: ok long numbered overloaded too-long compact-fails slow rate-limit
                                 401 500 bad-key bg[:SLEEP[:SLOW]] tool limit:CLAIM:RESET[:PLAN] <custom>   (bg also allows
                                 Bash(sleep ...) in the scratch settings); with a spec, the custom mode is written first
                                 ({\"status\",\"headers\",\"json\"|\"body\",\"stream\",\"times\",…}, the stub module) under a
                                 name that is neither a built-in mode's nor `refresh` (Codex's refresh answer)
  limit [tag] CLAIM SECONDS [PLAN]   the stub answers every request with a SUBSCRIPTION usage limit (429 + the
                                 unified-limit headers, CLAIM five_hour|seven_day|...) that resets SECONDS from now, then
                                 lifts; Claude Code draws the wall only when launched with `claude TAG --subscriber`; Codex
                                 gets its own usage_limit_reached body (PLAN: its plan_type) with the claim's window at 100%
                                 in its x-codex-* headers; Gemini CLI and agy get a 429 RESOURCE_EXHAUSTED, the key's spent
                                 per-day quota, or under CLAIM code_assist a Google account's spent quota (QUOTA_EXHAUSTED,
                                 its reset in RetryInfo), on which Gemini CLI draws its usage-limit dialog
  tool [tag] '<json>'            the stub's tool mode, scripted: {\"name\":\"Bash\",\"input\":{\"command\":\"touch x\"}} or a list of
                                 calls (\"@PROJ@\" = the project dir, \"@PORT@\" = the stub's); then `turn TAG toolstart` calls it
                                 (Gemini CLI and agy: the input is the call's args, {\"name\":\"run_shell_command\",...};
                                 Muse Code: its bare tool names, bash write_file edit_file request_user_input ...)
  headers [tag] ['<json>']       the stub's extra headers on /v1/messages answers of a status,
                                 {\"on200\":{...},\"on429\":{...}} (re-read per request; \"@SEQ:BASE:STEP@\" rises by STEP per 200
                                 answer that carried them): the unified-limit figures a --subscriber reads; '{}' or nothing
                                 clears them, and the flat entries `ratelimits` writes stay
  account [tag] composed|off|show|'<json>'   the bodies of the stub's claude.ai account routes (GET /api/oauth/usage,
                                 /api/oauth/profile, /api/claude_cli_profile, /api/claude_cli/bootstrap): {\"usage\":…,
                                 \"profile\":…,\"bootstrap\":…}, \"@ISO+N@\" an instant N s from the answer; composed: the
                                 lab's own (ana@example.com, Max 5x, a Fable week, credits, a grant)
  claude-json [tag] [KEY ...] | --set KEY '<json>'|composed   the scratch .claude.json's key names, then each KEY's
                                 value, or set one before `claude` (composed: the oauthAccount of the stub's composed
                                 profile); never the owner's file
  statusline [tag] on [SECS]|off|show   a status line that records what Claude Code hands it on stdin; show: per record
                                 the model, the cost and the windows, then the last record's key paths
  transcript [tag]               the newest scratch transcript's rows (type/subtype, the text of system and user rows)
  debuglog [tag] [egrep]         the lines of the debug log of `claude TAG --debug-log` (default: every line naming a
                                 request, the account reads and the telemetry among them)
  settings [tag] '<json>'        deep-merge a JSON object into the scratch settings.json (permissions, hooks, ...; a running
                                 Claude re-reads it; null removes a key; a list replaces the list)
  survey [tag] on|off [--reread] CLAUDE_FORCE_DISPLAY_SURVEY in the running Claude's settings env (live); --reread:
                                 written again every 3 s until Claude Code reports re-reading it (the settings witness)
  setenv [tag] KEY [VALUE]       edit the scratch settings.json env block (a running Claude re-reads it)
  harness [tag] key=value ...    set [harness] keys in the scratch aterm.toml (live), e.g. continue=false; a word that
                                 is no TOML value is written as a string (approve=none: approve = \"none\")
  text|image|status|sid [tag]    read the tab: ctl text | PNG | ls+status | session id
  phase [tag]                    what the hosted loop's reader (`aterm drive phase`) says of the tab: the box it parses
                                 (kind, command, description, notes, options) or busy/idle/...
  prices [<claude-bin>] [--codex <codex-bin>]   aterm's price table (prices.toml) against the build's own price
                                 catalog, read from its bytes, never run (the managed build by default): same / differs /
                                 table-only / build-only per model id; exit 1 when a row differs. Then its OpenAI rows
                                 against the Codex build's model catalog, by id (a Codex build carries no price): priced /
                                 unpriced / table-only. A check: the table is never filled from either
  abrew-admission [--json] [--indexes DIR] [--this-machine | --arch A --macos V --xcode V|none --no-clt]
                                 abrew's own admission (the pre-lock check of every native install) over formulae.brew.sh's
                                 formula.json and cask.json (fetched now, or DIR's) for the newest Mac abrew can name, or the
                                 one modelled or this one: formulae admitted solo and with their runtime closure, casks
                                 admitted, and the ten most common refusals of each (--json: the whole census)
  wait [tag] <egrep> [secs]      until a row of the screen matches (60 s by default; `await match`); exit 1 when none did
  ps [tag]                       the lab's pids; with no tag, EVERY lab that has a run directory (a forgotten one too)
  journal [tag]                  the harness journal+ledger
  watch [tag] [secs]             screen changes + journal lines of the next secs (30) on one clock
  stublog|security|egress|lsof [tag]   stub request log | keychain tripwire | egress witness | sockets of the lab's processes
  capture [tag] <name> \"<what>\"  $LAB_STATE/captures/claude-<version>-<name>.txt (+png) in the repo's fixture shape
                                 (codex-, gemini-, agy-, muse-<version>-<name> after `codex`, `gemini`, `agy`, `muse`)
  catch [tag] <egrep> <name> \"<what>\" [secs]   capture the screen the moment <egrep> shows (a transient dialog)
  header [tag] \"<what>\"          the one-line `# ...` header of a fixture, for this lab
  footers [--out DIR] [--subscriber] [--settings '<json>'] [--no-turn] [--no-bypass] [--columns N] [--lines N]
                                 record the footer screens of the managed Claude Code (every mode, a draft, a turn, bypass)
                                 on a lab of its own, in the fixture shape; --subscriber and --settings pick the variant
                                 (the corpus's own: --subscriber --settings
                                 '{\"model\":\"opus\",\"env\":{\"CLAUDE_CODE_EFFORT_LEVEL\":\"xhigh\"}}')
  probe [tag] [--replica]        the isolation probes: this lab's jail (a fresh one if none is up) and a replica home
                                 (--replica: the replica alone, which never touches the owner's tree)
  selftest [tag] [--keep]        the whole proof in ~25 s: up, launch, \"hi\", forced survey card, harness 0, egress,
                                 keychain tripwire, the open recorder, isolation probes, twin refusing a planted link, an
                                 orphan in the jail found and witnessed, down and nothing left in the jail; exits 1 on a
                                 failed check
  nudge-trial [tag] [--mode on|log|off] [--trials N] [--expect on|log|off] [--via mode|key] [--no-hold]
                                 force N<=3 displacements on the real Claude Code behind the PTY relay and judge what
                                 aterm's repaint nudge does (~2.5 min; no tag: a lab of its own); exit 0 only when every
                                 trial met the expectation, 1 a trial did not, 2 usage, 3 a trial never displaced or the
                                 lab could not run
  restore [tag] quit|crash [--keep] [--hold] [--clear] [--land|--land-open]   the cold restore: Claude Code takes a turn, aterm quits (`invoke Quit`) or is
                                 SIGKILLed, the instance comes back on the same state; reports the relaunch's typed line,
                                 whether the agent resumed the SAME conversation or sits in onboarding, its carry-on, and
                                 timings (~1 min; no tag: a lab of its own, LAB_RESTORE=1). The plain run is the Cruise
                                 control: its carry-on must settle on step=continued, typed, the tab autopilot=cruise
                                 (watched up to 170 s; a tab not under Cruise before aterm ends cannot run); exit 0 resumed
                                 with it, 1 it did not, 2 usage, 3 could not run. --hold (crash only): the owner holds the
                                 tab first; then the held tab must come back held with nothing relaunched or typed, and the
                                 lift its record names, run as written, must lift it (exit 0), else 1. --clear: `/clear`
                                 after the turn; the relaunch must resume the NEW conversation, its carry-on settled on
                                 step=done:taskless with nothing typed (no --land or --land-open with it: taskless, Land
                                 has nothing to land or leave open). --land: the tab under Land LANDS first (the stub's answers a clean defense;
                                 up to 6 min); after the relaunch it must read task=landed, autopilot=land, with
                                 no land instruction and no carry-on typed (its journal's carry-on settles on
                                 step=done:said-done; watched 170 s). --land-open, its negative control: Land mid-work,
                                 aterm ends once the crash journal carries the word; the relaunch must not read it done
                                 and types nothing of Cruise's (autopilot=land, the carry-on settled on
                                 step=done:landing past the waits and busy:another-sweep). A hold or Land the live
                                 instance's journal never carries on the tab within 30 s fails the run before aterm ends
                                 (1); a tab under Land before --land or --land-open sets it cannot run (3).
  forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-day|wall-climb|resid|ladder] [--chains a,b] [--no-surfaces] [--window] [tag] [--keep]   FOREVER LAND and FOREVER
                                 CRUISE live on a supervised harness: the real TUI on a lab of its own (all: one after
                                 another, <tag>-<harness>), a tab per scenario chain, each in a scratch git repository
                                 pushed to a local bare origin and left as the chain says, its Autopilot set, the stub
                                 scripting each turn by what the harness typed (custom mode `forever`). Land (L1 a work
                                 reply: land@v1 typed; L2 a defense beside a stated leftover: a nudge naming it; L3 NEEDS
                                 YOU and a defense: the needs entry raised, the nudge carrying on; L4, L8-L11 a defense
                                 over an untracked file, a changed file, a stash entry, an unpushed commit, a commit off
                                 origin/main: a nudge naming that leg; L5 a clean defense over a clean pushed repository:
                                 DONE rule=land@v1, task=landed, the need cleared, the ladder over and nothing typed
                                 through its next step; L6 a reply taller than the window, NEEDS YOU on top, the defense
                                 at the bottom: not landed, carried on; L7 a worker's task landed: its record done
                                 why=landed), Cruise (C1 a done claim: continue-done-check@v1; C2 its defense read: never
                                 DONE, the next point continues), P4 (every reply sent at once, over before any read sees
                                 the worker busy: P4L under Land each turn judged, landed, no act typed twice; P4C under
                                 Cruise the check's defense judged as it shows, the next point continuing; P4P a prompt
                                 typed mid-chain, answered at once, judged and decided as it shows), the end past the
                                 floor (L16, L17 a land reply busy past 30 s: end=<t> end_seen=0, its • and Done for you
                                 row; a look sees land-j's, a press on its row land-k's), the needs moving (N1-N4,
                                 cruise-n: needs_n 2, -, 1, 2) and the Off control (no supervisor, nothing typed 270 s
                                 after a done reply), --chains naming only some (land-a … off, instant-land,
                                 instant-cruise, land-j, land-k, cruise-n), each judged from the journal, ledger,
                                 status, meta, task record and the stub's dumps (~15-30 min a harness) AND THE WINDOW:
                                 Settings > Harness in a window of its own, the chains' window in front, cruise-n (else
                                 the Off control, which joins a --chains run that has no Cruise chain) its front tab;
                                 at each look chrome (each tab's autopilot line, toolbar-tabs states, the statusmenu
                                 rows, This Mac, presence) and the page's text; at END a 20 s idle window
                                 (timer_wakes=0, n_render=0, nothing armed but presence; where a program writes at
                                 idle, the status sweep alone, at most 3 fires a write), This Mac and the page's
                                 counts at rest (today N landed, Every session), each chain in front (drawn= its
                                 face=), the look begun on the front tab, then land-j under it, and the press on
                                 land-k's row; --no-surfaces makes the window's wants named skips. --mode cover: L12
                                 a question in prose at the land act's yield: land-answer@v1, its defense lands; L13 a
                                 person's own exchange (the draft standing, then Enter): LEFT, task=theirs, nothing typed
                                 270 s, Land's act only after an orchestrator's turn, judged only where status human_ms=
                                 stamps a person's hand (no control verb does: headless, UNOBSERVED); L14, C3 replies
                                 opening `Wrote the …` read (P2 live); P5 a worker's record working past its startup
                                 deadline; L15 a worker of the harness (spawn agent=<harness>) from a manager tab running
                                 it: working from=<mgr>, landed, await task and result. --mode cap (continue_per_hour = 1, ~72 min):
                                 the land act the hour's one act, the next point escalated, nothing typed until an
                                 orchestrator's turn after the hour, that turn acted on. --mode wall (every harness, each
                                 drawing its own usage wall, Claude Code on its subscriber path; ~6-12 min): W1 the stub's
                                 usage wall met by the land act, nothing typed into it, gone on past its reset (nothing
                                 typed on top of Claude Code's own continuation), landed.
                                 --mode wall-hid (every harness; ~15 min): W2 the remembered wall, W1's wall whose notice
                                 leaves the screen while the wait stands — a local command's output (Muse Code /rules, agy
                                 /title, Claude Code /rename after Esc cancels its countdown) scrolls it out of sight, or
                                 Gemini CLI's dialog is answered 2. Stop: a screen without the wall read, nothing typed into
                                 it, the resume typed there past the reset, landed. Gemini CLI's walled runs run it in a zone
                                 not the machine's (TZ).
                                 --mode wall-day (Gemini CLI; ~40 min): W1 on the usage-limit dialog of a key's spent
                                 per-day quota, which names no reset: the person told (the ladder's badge, no stall),
                                 Keep trying pressed at the ladder's first rung (30 min) past the stub's lift, landed.
                                 --mode wall-climb (Gemini CLI; ~2 h 45 min): that dialog, its quota standing past the first
                                 rung: the try there drawn again, NEEDS YOU standing, the next try at the climbed rung (2 h)
                                 getting through, NEEDS YOU down, landed.
                                 --window (a run with land-p): OPENS A WINDOW: the lab's instance windowed, in the same
                                 jail (no Launch Services: never the active app, `hwkey` key=0), and L13's person types
                                 land-p's first prompt key by key through `hwkey` (a real NSEvent in the window's own
                                 event queue, the path a key press takes), at an idle desk, its tab raised, instead of
                                 the as=person seam: judged only where status human_seq= moved by exactly the keys
                                 posted (else UNOBSERVED); one windowed lab at a time, ended by its keeper and `down`.
                                 The windowed instance installs aterm's menu-bar item, as every windowed aterm does: the
                                 lab's, beside the owner's aterm's, until `down`. Refused under LAB_RESTORE=1 (usage,
                                 exit 2)
                                 Exit 0 every scenario PASS, 1 one FAIL or UNOBSERVED, 2 usage, 3 could not run
  crash [tag] [--gap SECS] [--keep-tree]   aterm crashes under a lab that is up: its OWN instance (the pid env.sh records,
                                 refused unless that pid is still the lab's) SIGKILLed, and once it ended, the instance started
                                 again after SECS (0) with no instance, by restore's relaunch on the same state, socket and
                                 LAB_INSTANCE_ENV; judges nothing. Without --keep-tree what of its tree outlives it 15 s is
                                 ended by pid; with it nothing of the tree is touched (a PTY keeper keeps the agent through the
                                 crash: keeper = { enabled = true } in LAB_ATERM_TOML, LAB_INSTANCE_ENV='TMPDIR=/tmp') and
                                 what lives 2 s after is listed. Under LAB_RESTORE it first waits (30 s) for the crash journal
                                 to name the agent's conversation. A windowed lab (forever --window --keep) is refused
                                 (its recorded pid may be its window's keeper). Exit 0 the instance is back, 1 refused or
                                 failed, 2 usage
  sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]   a sleep of the Mac, emulated on a lab up
                                 with LAB_CLOCK=1 (the Mac never sleeps, the system clock never moves): its OWN instance (the
                                 pid env.sh records) SIGSTOPped, the clock's offset stepped SECS, held --hold SECS (0) more,
                                 and SIGCONTed if still confirmed as the lab's; Claude Code is not stopped and takes the step at its next
                                 clock read. --at-act: at the first act (a CONTINUED line, of RULE alone with --rule) the
                                 harness journals after the verb began, within --within SECS (3 h). It then waits (60 s) for
                                 the instance's clock record to say the new offset and lists which processes took it. Exit 0
                                 the step was taken, 1 refused (nothing stopped) or not taken, 2 usage
  muse-relaunch [tag] [--keep]   the Muse Code lane, live (a lab of its own, the harness on, LAB_RESTORE): the real Muse
                                 takes a turn and is SIGKILLed — relaunched in its tab on its session, in its workspace, on
                                 the managed twin, the stub seeing the prior turns; the RESUMED Muse SIGKILLed — its block's
                                 137 read and relaunched again; a clean /exit — not relaunched; `resume <unknown uuid>` —
                                 refused (1), not retried; aterm SIGKILLed — the reopened tab brings it back on its session
                                 (~4 min); exit 0 every phase held, 1 one did not, 3 could not run
  muse-upgrade [tag] [--keep]    Muse Code's live upgrade, its quiet flow, live (a lab of its own, the harness on): a
                                 scratch store laid as atpkg lays one in the instance's prefix, Muse run through its twin;
                                 a newer build landed (tree, record, current, twin, Muse Code's own activation notice) at
                                 idle — restarted onto it on its session, nothing typed, the stub seeing the prior turns;
                                 while a backgrounded shell runs — not restarted until it ended; mid-turn (stub slow) —
                                 not restarted until the turn ended; mid-way through a turn of real work (over 2 min)
                                 with `continue = true` — restarted at that turn's end, no turn typed between (~9 min);
                                 exit 0 every phase held, 1 one did not, 3 could not run
  agy-upgrade [tag] [--keep]     agy's live upgrade, its quiet flow, live (a lab of its own, the harness on; agy by the
                                 seeded `agy` verb through the twin of a scratch store laid as atpkg lays one in the
                                 instance's prefix): a newer build landed (tree, record, current, twin, agy's own
                                 activation notice) at idle — restarted onto it on its conversation, nothing typed, the
                                 stub seeing the prior turns, the probe leaving the instance's HOME untouched; while a
                                 background command task, a subagent (its model call held) or a daemon task runs — not
                                 restarted until it ended; mid-turn (stub slow) — not restarted until the turn ended;
                                 mid-way through a turn of real work with `continue = true` — restarted at that turn's
                                 end, no turn typed between (~12 min); exit 0 every phase held, 1 one did not, 3 could
                                 not run
  agy-relaunch [tag] [--keep]    the agy lane, live (a lab of its own, the harness on, LAB_RESTORE; agy by the seeded
                                 `agy` verb): the real agy takes a turn and is SIGKILLed — relaunched in its tab on its
                                 conversation, in its launch directory, on the managed twin, the stub seeing the prior
                                 turns; the RESUMED agy SIGKILLed — its block's 137 and crash file read, relaunched again;
                                 Ctrl-Z (146, alive) — not relaunched; a clean /quit — not relaunched; a relaunch agy
                                 refuses (2) — typed once; aterm SIGKILLed and quit — the reopened tab brings it back on its
                                 conversation; a relaunch that starts fresh — told, nothing typed (~10 min); exit 0 every
                                 phase held, 1 one did not, 3 could not run
  gemini-relaunch [tag] [--keep] the Gemini CLI lane, live (a lab of its own, the harness on, LAB_RESTORE; Gemini CLI
                                 typed by name, `HOME=<gemini-home> gemini`): its child SIGKILLed (1) — resumed in its tab
                                 on its session, in its project, by the shell's `gemini --resume`, the stub seeing the prior
                                 turn; its parent SIGKILLed (137) — the orphaned child waited out, resumed again; a clean
                                 /quit — not relaunched; a resume Gemini CLI refuses (42) — typed once; two started
                                 together — no witness, told; the G5 empty duplicate — not typed, told; aterm SIGKILLed and
                                 quit — the reopened tab brings it back on its session (~6 min); exit 0 every phase held, 1
                                 one did not, 3 could not run
  workers [tag] [--keep] [--early] [--upgrade <newer-claude-bin>]   the workers proof (~20 s; no tag: a lab of its own): a manager
                                 Claude Code in the first tab
                                 starts three workers (`aterm ctl @self spawn agent=claude -`), waits for them with a
                                 background `aterm ctl await task … all since=0`, is woken by its <task-notification> and
                                 reads each `result`; each worker is the real Claude Code the server launched, against the
                                 stub. Checks every result byte for byte, the tasks as sent, no stall, the survey, egress;
                                 --early: the manager's prompt on its command line, so it spawns before the instance has
                                 read its screen; --upgrade (a lab on an older LAB_CLAUDE): then the live upgrade of the done workers, each
                                 back with its record and result unchanged; exit 0 every check held, 1 one failed, 2 usage,
                                 3 could not run
  twin [tag] <claude-bin>|--crashing [ver]   present <claude-bin> as a NEWER managed Claude Code the way atpkg lays one: a
                                 clone under the scratch HOME's package store, the `agents/claude` twin that execs it, and the
                                 activation notice renamed into place, which wakes the instance's live upgrade; --crashing: a
                                 stand-in build that names <ver> (99.0.0) and exits 1 at every launch
  fleet-sim [--n N] [--runs R] [--keep] [--record]   THIRTY MACHINES ON ONE MAC (design \u{00a7}8): N private headless
                                 aterms (N=30, at least 4) on loopback fleet ports, machine 0 the issuer and every other a
                                 member joined --allow content, each with a lab Mac's five sessions (its idle shell, a build
                                 loop, a `claude` fixture busy, at a question or Bash box, or idle, and two more `claude` at
                                 rest, in deep worktrees, words near the excerpt cap); then the fleet read a Claude Code
                                 session on machine 0 runs (`aterm ctl survey --fleet`, in that session's environment, under
                                 ulimit -n 256), held to the design's laws, R times (R=3), measured; the fault rows (one
                                 SIGSTOPped, one killed) and the security refusals asserted. The record goes to
                                 $LAB_STATE/results/; --record on an all-green run writes docs/measured/fleet-sim-<N>-<date>.md
                                 (each green invocation a row). Exit 0 only when every assertion held; interrupted: 128 +
                                 the signal, nothing recorded. Everything ended by pid, its roots removed. Not jailed (no
                                 Claude Code): own HOME/XDG/state per instance under /tmp/af.*
  scn list|run|show|agg ...      the survey scenarios: `scn run <scenario> [--n N] [--start K]` (N fresh labs tagged
                                 $LAB_TAG or survey), `scn show <run>`, `scn agg [<scenario>]` (the cycle's min/median/max)
  tape promote <results-run|dir|tape> [--as NAME] [--fewer]   the harness tape corpus (crates/aterm-agent/tests/
                                 testdata/tapes/): a run's one ended tape, this machine's names (the lab state, the
                                 checkout, the home, the temp dir, the user, the host) scrubbed to fixed tokens, linted
                                 (/Users/, the home, the size cap: the corpus README), replayed strictly and in acts mode
                                 from its scrubbed text, kept as NAME.atape.txt (default: the run's scenario) or refused;
                                 refused too for a run whose summary.json records an error or exception, and for fewer
                                 acts, or no approval/question/turn end/wall where the tape it replaces held one
                                 (--fewer: a deliberate change); read it before it is committed
  tape lint [FILE|DIR...]        the corpus's lint on this machine (default: the corpus); exit 1 when a tape is flagged
  tape replay-jailed [FILE|DIR...]   each tape replayed by `aterm drive replay --check` under sandbox-exec, every
                                 read but the binary's, the tape's and the loader's denied, every write, the network,
                                 every program and fork; first a copy of a tape the profile does not name must be refused
  tape rerecord NAME [--fewer]   the scenario NAME run again on a fresh lab, its tape promoted over the corpus's NAME
                                 under promote's refusals
  ctl [tag] <verb> ...           raw `aterm ctl` against THIS lab's socket only
  protected HOME CCSOCKS         the owner's data the jail denies, as \"<kind> TAB <path>\" lines (the one list)
  paths                          LAB_STATE (runs/ logs/ captures/ results/) and LAB_ATERM (the aterm a new lab tests; a lab
                                 that is up runs the one its env.sh recorded at up)
  down [tag]                     end everything in the lab's jail (found by its mark: an orphan too), remove the scratch
                                 and what its processes left in a shared runtime dir that TMPDIR, XDG_RUNTIME_DIR and
                                 HOME do not move (Muse Code's /private/tmp/tbh-<uid>-rt/muse: only the entries that
                                 name them), keep the logs in $LAB_STATE/logs

Outside any lab (no jail, no stub):
  instance [--window] [--band app|agent] [--aterm PATH] [--columns N] [--lines N] [--font-px N] [--scale N]
           [--env NAME=VALUE]…   pass a TMPDIR or ATERM_DEBUG_*/ATERM_DEV_* seam; keeps HOME and state private
           [--toml LINE]... [--toml-after LINE]... [--home DIR] [--harness] [--pre VERB]... [--image PNG] [--post VERB]...
           [--pages cold] [--keep] [-- PROGRAM ARGS]
                                 a private headless aterm in a scratch world (HOME, XDG dirs, the automatic lanes off,
                                 the audible bell off: --toml 'bell_sound = true' rings it), held until this command
                                 ends (Ctrl-C, SIGTERM); prints its socket for `aterm ctl --sock`;
                                 --band app (headless default): aterm.app's scheduling band, its own launchd job heading
                                 its own coalition at role UI_FOCAL; --band agent: a child of this command, inside the
                                 caller's coalition and role (a windowed instance's only band);
                                 --window: OPENS A WINDOW, the same instance windowed: only a window presents, so
                                 `present_*`, `gpu_time_*` and `passes_last` in `metrics percentiles` need it;
                                 --toml: a top-level line at the head of its aterm.toml (theme = \"Aterm Day\");
                                 --toml-after: a line after its tables (`[disk]`); --home: a new HOME there (a scratch
                                 volume's folder); --harness: the harness host on (the disk watch runs only with it);
                                 --pre: a control verb once it is ready (open app settings /themes), reply printed;
                                 --image: one settled frame written to PNG, then it ends; --post: a verb after the frame;
                                 --pages cold: launch a copy (its bundle's, for an app) run once and then put out of the
                                 page cache, so the launch pays aterm's own page-ins (the dyld cache stays warm); the
                                 banner says how many of its pages the launch had read by ready
  disk-lane [--aterm PATH] [--keep]
                                 the disk awareness lane: an 8 GiB sparse APFS image mounted -nobrowse outside /Volumes,
                                 a headless instance whose HOME is on it ([disk] warn 6 GiB, floor 5 GiB, tier 3 on),
                                 checkouts idle, edited, locked and linked, a removed workspace's build cache; it fills
                                 the volume short, waits `await sys free>=5GiB` and judges the journal's tiers and what
                                 was kept, then fills it past the emergency and judges the reserve went first; ~1 min;
                                 macOS may show a removable-volume consent dialog for the image where the terminal that
                                 runs it lacks Full Disk Access; exit 0 pass, 1 a check failed, 3 not run (no hdiutil, or
                                 under 24 GiB free here)
  slot-capture [--watch DUR [--dir DIR]]
                                 the build slots' record capture: the real build-slot (~/.local/bin, else $HOME/trust/scripts)
                                 admits `true` into a scratch BUILD_SLOT_DIR, and its slot-0 must read as aterm's slot reader
                                 reads it (the wrapper's pid, started in the run, Python's json.dumps spelling, cmd `true`,
                                 cwd the scratch folder); exit 0 pass, 1 a field changed, 3 not run (no build-slot). --watch
                                 DUR (30s, 20m, 24h): the reader beside lsof over the real ~/.cache/build-slot (or DIR)
                                 every 5 s, read only; a holder one proves and the other does not show fails; an hourly line
  slot-lane [--aterm PATH] [--keep]
                                 `await sys slots>=1` on a headless instance whose HOME holds one slot a stand-in holds: it
                                 times out while held and answers OK sys slots=1/1 once the stand-in is SIGKILLed; exit 0
                                 pass, 1 a check failed, 3 not run (no aterm, or `sys` does not answer headless)
  trail-gallery [--aterm PATH] [--theme dark|light|both] [--scenario NAME[,NAME]]... [--extra 'key = value']...
                [--columns N] [--lines N] [--font-px N] [--no-clean] <out_dir> [style ...]
                                 photograph cursor trail styles: one private headless aterm per style x theme x scenario
                                 (typing context steam enter-bottom idle10 redraw-stall cjk bidi settings, or all), a frame after every
                                 key (headless advances effects only when a frame renders), each kept frame with its clean
                                 twin, cursor, text and dims for trail-contrast; no style: every style the Settings picker
                                 offers; <out_dir> outside the repository; exit 0 all taken, 1 a frame or instance failed,
                                 2 usage, 3 not run; --help for the files and scenarios
  trail-contrast <png> [--find TEXT]... [--cells ROW:COL[-COL]]... [--row N]... [--newest N] [--min X]
                 [--newest-min X] [--caret-min X] [--dims FILE|--origin X,Y --cell WxH] [--no-clean] ...
                                 the WCAG contrast of a frame's cells as JSON (ink and ground from its clean twin), its
                                 caret against the page and the pixels the effects changed outside it; reads the
                                 sidecars trail-gallery writes; exit 1 when a threshold fails, 2 usage or file; --help
  aos-repair [--aterm PATH] [--control] [--keep] [--out DIR]
                                 OPENS A WINDOW: Aos M2's live repair on one private windowed aterm (a debug build): the
                                 momentum glow attached under the demo-stranded-settle fault, 24 keys typed, the stranded
                                 glow witnessed past its cold instant by reads that repaint nothing, `aos repair
                                 code=shipped`, the clearing frame, 12 fresh keys that clear by themselves (~25 s);
                                 --control attaches the shipped code and requires it to clear by itself; one result line;
                                 exit 0 every gate passed, 1 a gate failed, 2 usage, 3 not run; --help
  resource-attribution [--aterm PATH]  private two-tab delegated resource check
  presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases \"A H B D T X\"] [--trials N] [--keep]
                                 OPENS A WINDOW: one private windowed aterm running a Claude-Code-shaped stand-in, driven
                                 through fenced and harness-shaped turns, a foreign lease and cross-source chrome pairs
                                 (the band and the tab strip, phase X) (~3 min); judges that the window caused no re-grid
                                 flap and no render risk. Run it after touching anything that can re-grid a window; exit 0
                                 clean, 1 a row failed, 3 not run or a phase X sweep formed no pair (not measured);
                                 --headless: the same steps with no window, to check the lane itself (no window verdict)
  meta-keys --window [--layout ID]... [--aterm PATH] [--keep] | meta-keys --plan [--layout ID]... | meta-keys --restore
                                 OPENS A WINDOW, SELECTS EACH KEYBOARD LAYOUT AND PRESSES KEYS: one private windowed aterm
                                 running a byte dump, raised to the front; on the U.S., German and French layouts (or each
                                 --layout, an input source ID or its last part), each selected for its rows and the
                                 person's own put back after (a record file covers a run that dies), Option chords pressed
                                 as key presses (`hwkey … shape=press`) with Meta off (the control: an Option dead key and
                                 a letter compose, an Option chord types the layout's character) and on (Option dead keys
                                 and chords go out as ESC and the key's own character; a bare dead key still composes),
                                 every row derived from the layout itself; only at an idle desk; ~1 min; exit 0 pass, 1 a
                                 row failed or the input sources did not come back, 3 not run or not measured;
                                 --plan prints each layout's derived rows and changes nothing; --restore puts back what a
                                 run's record names
  predict-live --window|--headless [--aterm PATH] [--shell zsh|bash]... [--rtt MS]... [--mode on|off]...
               [--rounds N] [--no-split] [--out DIR]
                                 predictive editing on a REAL aterm: a private instance (a debug build, its
                                 $ATERM_TRACE_PREDICT trace on) whose zsh or bash runs behind `link` (a real round trip,
                                 default 100 ms) with aterm's shell integration, the plan's editing corpus pressed through
                                 hwkey (typing, a typo and Backspace, arrows, mid-line inserts and erases, Ctrl-D, Delete,
                                 Home/End, Ctrl-A/E), in the single pane and then a split pane (the composed path); every
                                 painted frame held to the screen its newest key settles to (the far end's own take,
                                 replayed through aterm-core and checked against the instance's text), the keys shown
                                 before their echo, and `metrics percentiles` with prediction on and off. --window opens
                                 a window and presses keys only at an idle desk; --headless runs the press path alone
                                 (`key`); out DIR keeps the logs, takes and a mid-line image; exit 0 held, 1 a wrong
                                 frame or a failed check, 3 not run or not measured
  codex-alab-proof [--package DIR] [--aterm PATH] [--keep] [--policy-only]
                                 the codex-alab native harness end to end on the REAL package (--package, $LAB_CODEX, else
                                 the managed codex when it is an ALab build), linked as the owner links it in a sandboxed
                                 private headless aterm (network denied but a loopback fake model): the proven claim and the
                                 parked lane, FOREVER CRUISE (a done claim, the retired token too, gets the defend prompt and
                                 goes on), FOREVER LAND (land@v1, unbounded nudges, landed@v1 only at a clean defense over a
                                 clean folder, then nothing, task=landed, reopened by a person's turn; a dirty repository not
                                 landed until pushed), NEEDS YOU (attention owner=needs at once, never blocking), an approval,
                                 the question overlay and Land's answer, a usage wait, a crash relaunch and a second, a
                                 carry-on, the re-exec upgrade, a hold, fourteen claims at once, Autopilot Off (the claim
                                 given back; a manager's `post kind=task` alone over the lab's own bus) and On, and a
                                 person's own exchange (typed through the debug seam `send|key as=person`: nothing typed
                                 after it, the rule in codex-alab's journal, task=theirs) beside a control write's turn
                                 (continued); ~11 min. --policy-only runs the focused cybersecurity refusal lane: one
                                 independent-work continuation, a persistent exclusion on later automatic work, a repeated
                                 refusal and exhausted-work stop, plus an unsent draft and unchanged access program;
                                 exit 0 pass, 1 a check failed, 3 not run (no codex-alab package)
  reduce <transcript|rollout> --program claude|codex [--out FILE]
                                 a KEY-ONLY reduction (session log 5H-1, P11 as ruling T7 amends it): keys, row order,
                                 numbers and the closed words a reader reads kept (ruling U7: a word no reading reads
                                 goes), times moved to 2000-01-01 by whole days, ids remapped one to
                                 one, a user row's opening marker kept, a notice written as the canonical phrase of the wall
                                 its words read as, every other string replaced; to FILE (never over one) or stdout, and
                                 only when the harness's readers read it as its source and read every word it keeps (else
                                 exit 1, nothing written). The shape a lab-read build is committed in (aterm-agent
                                 harness/testdata/steps/<build>.jsonl; a Codex rollout's codex-<build>[-<leg>].jsonl)
  ptyrec <log> -- <cmd> [args...]   run <cmd> behind a PTY in this terminal, everything passed through, and append
                                 every chunk to <log>: `<t_ns> O <hex>` (its output), `<t_ns> I <hex>` (input passed to
                                 it); its window follows this terminal's (at start and on SIGWINCH): the takes aterm-effects
                                 replays; exit: the program's
  link [--rtt MS] [--take FILE] -- <cmd> [args...]
                                 run <cmd> behind a PTY whose link holds every chunk RTT/2 each way (default 100 ms),
                                 raw on this side as ssh is; FILE gets the far end's own take (ptyrec's lines: `I` when
                                 <cmd> is handed input, `O` when it writes). Copied under a shell's name (zsh, bash) it is
                                 that shell over the link, for an aterm whose `shell` names the copy (LAB_LINK_RTT_MS,
                                 LAB_LINK_TAKE=<dir>, LAB_LINK_EXEC); `predict-live` runs it so
  mac dmg [--image REF] [--fda] [--keep]
                                 the macOS guest lane: atpkg's, aterm-containment's, aterm-uds' and aterm-update's
                                 real disk-image acceptance (dmg_acceptance benches built here with --no-run) run in a
                                 fresh tart VM with SIP on, tccd's log streamed, the clone deleted; nothing is attached on
                                 this Mac (`mac help`; exit 3 NOT RUN without tart or on a row the guest refused)
  linux up|down|status|exec|proof|clean|destroy
                                 the Linux runtime lane: a lima VM (vz, Ubuntu 24.04 aarch64, Rosetta for x86_64) holding
                                 this checkout read-only; `exec` runs a command in it per arch under Xvfb, weston, Mutter
                                 or Xwayland; `proof` runs aterm-uds/pty/libc and the libc oracle's cells on both arches
                                 and drives a Linux aterm-gui under each display (`linux help`; exit 3 NOT RUN without lima)
  windows up|down|status|exec|smoke|window|destroy
                                 the Windows runtime lane: a QEMU VM (hvf, Windows 11 arm64, Prism for x86_64) made from
                                 Microsoft's ISO, installed unattended and provisioned with MSVC and stock Rust; `exec`
                                 copies this checkout in and runs a command per arch; `window` drives a windowed aterm-gui
                                 with the VM's keyboard and tablet (`windows help`; exit 3 NOT RUN without QEMU, swtpm or
                                 the image)

The operator guide is the crate's documentation: `targo --unverified doc -p aterm-lab --open`, or crates/aterm-lab/src/.
";

/// Why a verb ended: a message for stderr (when there is one), its exit
/// status, and the interrupt that cut it short (when one did).
#[derive(Debug)]
pub struct Fail {
    /// Printed as `aterm-lab: <msg>`.
    pub msg: Option<String>,
    /// The exit status.
    pub code: i32,
    /// The signal of an interrupt.
    pub stopped: Option<i32>,
}

impl Fail {
    /// A refusal: exit 1.
    #[must_use]
    pub fn die(msg: impl Into<String>) -> Fail {
        Fail::exit(1, msg)
    }

    /// A usage error: exit 2.
    #[must_use]
    pub fn usage(msg: impl Into<String>) -> Fail {
        Fail::exit(2, msg)
    }

    /// An ending with this status and message.
    #[must_use]
    pub fn exit(code: i32, msg: impl Into<String>) -> Fail {
        Fail {
            msg: Some(msg.into()),
            code,
            stopped: None,
        }
    }
}

impl From<String> for Fail {
    fn from(msg: String) -> Self {
        Fail::die(msg)
    }
}

impl From<&str> for Fail {
    fn from(msg: &str) -> Self {
        Fail::die(msg)
    }
}

#[cfg(target_os = "macos")]
impl From<stop::Stopped> for Fail {
    fn from(s: stop::Stopped) -> Self {
        Fail {
            msg: None,
            code: 128 + s.0,
            stopped: Some(s.0),
        }
    }
}

/// A verb's outcome: its exit status, or why it ended early.
pub type Res = Result<i32, Fail>;

#[cfg(target_os = "macos")]
fn dispatch(cmd: &str, args: &[String]) -> Res {
    match cmd {
        "up" => lab::cmd_up(args),
        "down" => lab::cmd_down(args),
        "ps" => lab::cmd_ps(args),
        "claude" => lab::cmd_claude(args),
        "codex" => codex::cmd_codex(args),
        "codex-run" => codex::cmd_codex_run(args),
        "codex-auth" => codex::cmd_codex_auth(args),
        "ratelimits" => codex::cmd_ratelimits(args),
        "rollout" => codex::cmd_rollout(args),
        "codex-selftest" => codex_proof::cmd_codex_selftest(args),
        "gemini" => gemini::cmd_gemini(args),
        "agy" => gemini::cmd_agy(args),
        "gemini-captures" => gemini_captures::cmd_gemini_captures(args),
        "gemini-selftest" => gemini_proof::cmd_gemini_selftest(args),
        "agy-selftest" => gemini_proof::cmd_agy_selftest(args),
        "agy-captures" => gemini_captures::cmd_agy_captures(args),
        "muse" => muse::cmd_muse(args),
        "muse-captures" => muse_captures::cmd_muse_captures(args),
        "muse-selftest" => muse_proof::cmd_muse_selftest(args),
        "muse-relaunch" => relaunch_muse::cmd_muse_relaunch(args),
        "muse-upgrade" => upgrade_muse::cmd_muse_upgrade(args),
        "agy-upgrade" => upgrade_agy::cmd_agy_upgrade(args),
        "agy-relaunch" => relaunch_agy::cmd_agy_relaunch(args),
        "gemini-relaunch" => relaunch_gemini::cmd_gemini_relaunch(args),
        "fleet-sim" => fleet_sim::cmd(args),
        "mode" => verbs::cmd_mode(args),
        "limit" => verbs::cmd_limit(args),
        "tool" => verbs::cmd_tool(args),
        "settings" => verbs::cmd_settings(args),
        "headers" => verbs::cmd_headers(args),
        "account" => stats::cmd_account(args),
        "claude-json" => stats::cmd_claude_json(args),
        "statusline" => stats::cmd_statusline(args),
        "transcript" => stats::cmd_transcript(args),
        "debuglog" => stats::cmd_debuglog(args),
        "setenv" => verbs::cmd_setenv(args),
        "survey" => verbs::cmd_survey(args),
        "harness" => verbs::cmd_harness(args),
        "send" => verbs::cmd_send(args),
        "enter" => verbs::cmd_enter(args),
        "turn" => verbs::cmd_turn(args),
        "key" => verbs::cmd_passthrough(Some("key"), args),
        "text" => verbs::cmd_passthrough(Some("text"), args),
        "ctl" => verbs::cmd_passthrough(None, args),
        "image" => verbs::cmd_image(args),
        "status" => verbs::cmd_status(args),
        "sid" => verbs::cmd_sid(args),
        "phase" => verbs::cmd_phase(args),
        "prices" => prices::cmd_prices(args),
        "abrew-admission" => abrew_admission::cmd_abrew_admission(args),
        "stublog" => verbs::cmd_stublog(args),
        "security" => verbs::cmd_security(args),
        "egress" => verbs::cmd_egress(args),
        "lsof" => verbs::cmd_lsof(args),
        "journal" => verbs::cmd_journal(args),
        "wait" => verbs::cmd_wait(args),
        "header" => verbs::cmd_header(args),
        "capture" => verbs::cmd_capture(args),
        "twin" => verbs::cmd_twin(args),
        "watch" => scn::cmd_watch(args),
        "catch" => scn::cmd_catch(args),
        "scn" => scn::cmd_scn(args),
        "tape" => tape::cmd_tape(args),
        "probe" => probe::cmd_probe(args),
        "selftest" => selftest::cmd_selftest(args),
        "nudge-trial" => nudge::cmd_nudge_trial(args),
        "restore" => restore::cmd_restore(args),
        "forever" => forever::cmd_forever(args),
        "crash" => crash::cmd_crash(args),
        "sleep" => sleep::cmd_sleep(args),
        "workers" => workers::cmd_workers(args),
        "footers" => footers::cmd_footers(args),
        "paths" => verbs::cmd_paths(),
        "instance" => instance::cmd_instance(args),
        "resource-attribution" => resource_attribution::cmd(args),
        "disk-lane" => disk_lane::cmd_disk_lane(args),
        "slot-capture" => slot_lane::cmd_slot_capture(args),
        "slot-lane" => slot_lane::cmd_slot_lane(args),
        "trail-gallery" => trail_gallery::cmd_trail_gallery(args),
        "__trail-redraw-fixture" => trail_gallery::cmd_redraw_fixture(args),
        "trail-contrast" => trail_contrast::cmd_trail_contrast(args),
        "aos-repair" => aos_repair::cmd_aos_repair(args),
        "presence-flap" => presence::cmd_presence_flap(args),
        "meta-keys" => meta_keys::cmd_meta_keys(args),
        "predict-live" => predict_live::cmd_predict_live(args),
        "codex-alab-proof" => codex_alab_proof::cmd_codex_alab_proof(args),
        "linux" => linux::cmd_linux(args),
        "windows" => windows::cmd_windows(args),
        "mac" => mac::cmd_mac(args),
        "reduce" => reduce::cmd_reduce(args),
        "protected" => match (args.first(), args.get(1)) {
            (Some(h), Some(c)) => {
                print!("{}", protected::render_lines(h, c));
                Ok(0)
            }
            _ => Err(Fail::die("protected: need HOME and CCSOCKS")),
        },
        "" | "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(0)
        }
        other => Err(Fail::die(format!("unknown command '{other}' ({LAB} help)"))),
    }
}

/// Before anything is made: the owner's home must be known (the account's,
/// or `HOME`'s), and the state dir must be quotable in a sandbox profile,
/// must sit outside every home the jail guards (whose fence its runs'
/// tapes could not get through: [`paths::Paths::need_state_outside`]), and,
/// when it is the default one, is made this user's private directory
/// ([`paths::Paths::need_state`], which every verb that makes something in
/// the state dir goes through, one this check skips among them).
#[cfg(target_os = "macos")]
fn check_state_dir() -> Result<(), Fail> {
    let p = paths::paths();
    let homes = p.need_guarded_homes()?;
    p.need_state_outside(&homes)?;
    let s = p.state.to_string_lossy();
    if !paths::profile_safe(&s) {
        return Err(Fail::die(format!(
            "a quote, a backslash or a control character in LAB_STATE ({s}) cannot go into the sandbox profile"
        )));
    }
    if protected::inside_any(&s, &homes, protected::CCSOCKS) {
        return Err(Fail::die(format!(
            "LAB_STATE may not sit inside the owner's data ({s})"
        )));
    }
    p.need_state()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    let mut argv = std::env::args();
    let argv0 = argv.next().unwrap_or_default();
    let args: Vec<String> = argv.collect();
    // The roles this program plays under other names, inside a lab.
    let code = match std::path::Path::new(&argv0)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some("node") => relay::main(&args),
        // The link relay under a shell's name, so aterm integrates the shell behind it
        // ([`link`]): no env scrubbing, the shell gets aterm's environment as given.
        Some(name @ ("zsh" | "-zsh" | "bash" | "-bash")) => link::as_shell(name, &args),
        Some("security") => tripwire::main(&args),
        Some("open") => recorder::main(&args),
        // The fleet-sim fixtures, run under their own names by a session's
        // shell (a link in the simulation's `bin/`): no env scrubbing, so the
        // Claude-Code-shaped fixture's own HOME reaches it.
        Some("claude") => fleet_fixture::claude(&args),
        Some("build-loop") => fleet_fixture::build_loop(&args),
        Some("ps") => codex_rig::ps_main(&args),
        _ => {
            let cmd = args.first().cloned().unwrap_or_default();
            let rest = args.get(1..).unwrap_or_default();
            match cmd.as_str() {
                // jailed and witness roles: their environment is the lab's, kept as given
                "stub" => stub::main(rest),
                "sampler" => sampler::main(rest),
                "ctty" => relay::ctty(rest),
                // a recorder run by hand: the program it records gets the
                // caller's environment as given, as the take's reader expects
                "ptyrec" => relay::ptyrec(rest),
                // the link relay run by hand: its program gets the caller's environment
                "link" => link::main(rest),
                "probe-inside" => probe::inside_main(rest),
                "probe-tiocsti" => probe::tiocsti_child(),
                "hold" => recorder::hold(rest),
                "keeper" => instance::keeper_main(rest),
                // an app-band instance's launchd job: its environment is the world's, read from the world
                "band" => band::band_main(rest),
                "band-launch" => band::launch_main(rest),
                "band-standin" => band::standin_main(rest),
                "presence-standin" => presence::standin_main(rest),
                // the build-slot holder `slot-lane` starts, this program again
                slot_lane::HOLD_VERB => slot_lane::hold_main(rest),
                "key-dump" => meta_keys::keydump_main(rest),
                "mcp-standin" => codex_rig::mcp_main(rest),
                _ => {
                    // A lab must never reach the owner's live aterm or Claude Code: forget what the
                    // calling shell inherited from them before any thread starts.
                    sys::forget_env(paths::forgotten);
                    sys::confine_env();
                    // These read files or print and make nothing: no state dir to check
                    // (`fleet-sim` writes its record through `need_state`, which makes the
                    // default private first).
                    let r = if matches!(
                        cmd.as_str(),
                        "" | "help"
                            | "-h"
                            | "--help"
                            | "paths"
                            | "protected"
                            | "fleet-sim"
                            | "reduce"
                            | "trail-contrast"
                            | "abrew-admission"
                    ) {
                        dispatch(&cmd, rest)
                    } else {
                        check_state_dir().and_then(|()| dispatch(&cmd, rest))
                    };
                    match r {
                        Ok(c) => c,
                        Err(f) => {
                            if let Some(m) = f.msg {
                                eprintln!("aterm-lab: {m}");
                            }
                            f.code
                        }
                    }
                }
            }
        }
    };
    std::process::ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("aterm-lab: macOS only (sandbox-exec is the lab's jail)");
    std::process::ExitCode::from(3)
}
