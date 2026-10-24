// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-agent lab: launches the real Claude Code, Codex ([`codex`]), Gemini CLI / Antigravity CLI (`agy`, [`gemini`]; screens in [`gemini_captures`]), or Muse Code ([`muse`]; proof in [`muse_proof`]) TUI by path inside a private headless `aterm` against a local API stub (Anthropic, OpenAI Responses, Gemini, or Meta), and runs private aterm instances for checking a change. [`USAGE`] lists the verbs.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
mod abrew_admission;
#[cfg(target_os = "macos")]
mod aos_phase;
#[cfg(target_os = "macos")]
mod aos_repair;
#[cfg(target_os = "macos")]
mod aos_search;
#[cfg(target_os = "macos")]
mod aos_workers;
#[cfg(target_os = "macos")]
mod aos_workers_bench;
#[cfg(target_os = "macos")]
mod band;
#[cfg(target_os = "macos")]
mod bus;
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
mod halfdraw;
#[cfg(target_os = "macos")]
mod idlepoints;
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
mod linux_vz;
#[cfg(target_os = "macos")]
mod linux_vz_host;
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
mod occupancy_break;
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
mod reroute_prefix;
#[cfg(target_os = "macos")]
mod resource_attribution;
#[cfg(target_os = "macos")]
mod restore;
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
mod seamless;
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

/// How a person runs this program, for the hints that tell them what to type next.
pub const LAB: &str = "targo --unverified lab";

/// Poll `pred` every 100 ms until it holds (`true`) or `within` passes (`false`); a hang detector for something that must happen, interruptible.
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
aterm-lab — the real Claude Code, Codex, Gemini CLI, agy or Muse Code TUI in a jailed private headless aterm
against a local API stub (Anthropic, Responses, Gemini or Meta). A lab is fully local: no real account, token or egress.

usage: targo --unverified lab <command> [tag] [args]
       (tag: the first word when it names a lab that is up, else $LAB_TAG or \"lab\" and the word is an argument;
       up, down, ps, crash and sleep take any first word as the tag; a verb that brings up its own lab defaults to a
       tag of its own)
  up [tag]                       start the stub, a black-hole proxy, the egress witness and a jailed private headless aterm
                                 (env: LAB_SURVEY=1 LAB_TRUST=1 LAB_FIRST_RUN=1 LAB_RESTORE=1 LAB_CLOCK=1
                                 LAB_HARNESS_KEYS='continue = false' LAB_ATERM_TOML LAB_COLS LAB_LINES LAB_SCALE
                                 LAB_PROXY=dead LAB_CLAUDE LAB_CODEX LAB_GEMINI LAB_AGY LAB_MUSE
                                 LAB_ATERM=/physical/path LAB_STATE=/abs/outside/homes
                                 LAB_INSTANCE_ENV='TMPDIR=/abs ATERM_DEBUG_<NAME>=V ATERM_DEV_<NAME>=V ATERM_OCCUPANCY_NUDGE_S=N'
                                 LAB_REROUTE=1: the lab's Claude Code store build cloned into the jail's own atpkg
                                 prefix with its twin and reroute stub, the instance rerouting; `claude --by-name`)
  claude [tag] [-e K=V]... [--no-wait] [--relay] [--subscriber] [--debug-log] [--by-name] [-- claude args]
                                 launch the managed Claude Code in tab 1 (--relay: behind the PTY relay; --subscriber: a
                                 fake claude.ai OAuth token, for `limit` headers; --debug-log: a scratch debug log;
                                 --by-name: `claude` through PATH, the reroute stub and the twin of a LAB_REROUTE=1 lab)
  codex [tag] [--signed-out|--chatgpt[=PLAN]] [-e K=V]... [--no-wait] [-- codex args]
                                 launch the managed Codex in tab 1 (CODEX_HOME=<scratch>/codex-home, model
                                 $LAB_CODEX_MODEL when set; --chatgpt: a fake ChatGPT auth.json, plan plus by default,
                                 and stub refresh; --signed-out: the default provider, no auth; either one also sets
                                 [harness] enabled = false in the lab's aterm.toml)
  codex-run [tag] [--lab|--chatgpt|--signed-out] -- <args>
                                 run non-interactive Codex in the jail (--help, exec, `login status`, `login --help|-h`;
                                 any other login, logout, app, app-server, remote-control, agents, exec-server, cloud
                                 and update are refused)
  codex-auth [tag] [LABEL [ACCOUNT [PLAN]]]
                                 write a fake auth.json (default B, acct-lab-1, plus) into the live CODEX_HOME
  ratelimits [tag] off | PRIMARY SECONDARY [credits=yes|no|unlimited] [balance=N] [reached=TYPE] [promo=TEXT] [active=ID]
                                 set the x-codex-* rate-limit headers (5h and weekly used %, - leaves that window out) and
                                 the --chatgpt usage payload
  rollout [tag] [all]            print token_count lines (all: every line) of the tab's newest scratch rollout
  codex-selftest [tag] [--keep]  end-to-end Codex isolation and stub proof (~20 s)
  gemini [tag] [-e K=V]... [--no-wait] [--untrusted] [-- gemini args]
                                 launch Gemini CLI in tab 1 against the stub Gemini API (--untrusted: the folder trust dialog)
  agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [--setting KEY=VALUE]... [-- agy args]
                                 launch agy in tab 1 against the stub Gemini API (--onboarding: the first-run screens and
                                 the trust dialog; --setting: one settings.json key)
  gemini-selftest|agy-selftest [tag] [--keep]
                                 end-to-end Gemini CLI or agy isolation and stub proof (~30 s)
  gemini-captures|agy-captures [--only <regex>]
                                 capture every Gemini CLI or agy fixture screen (~16 min; --only narrows)
  muse [tag] [-e K=V]... [--no-wait] [-- muse args]
                                 launch Muse Code in tab 1 against the stub's Meta routes with a scratch HOME
  muse-selftest [tag] [--keep]   end-to-end Muse Code isolation and stub proof (~30 s)
  muse-captures [--only <regex>] capture every Muse Code fixture screen (~10 min; --only narrows)
  turn|send|enter|key [tag] ...  type prompt+Enter | text | Enter | named key (0, escape, ctrl+c, ...); turn exits 1
                                 unless the composer accepted the prompt
  mode [tag] [name ['<spec>']]   show or set the stub mode: ok long numbered overloaded too-long compact-fails slow
                                 rate-limit 401 500 bad-key bg[:SLEEP[:SLOW]] tool limit:CLAIM:RESET[:PLAN], or a
                                 custom name whose '<spec>' JSON object is written to modes/<name>.json (bg: a prompt
                                 with `bgstart` runs `sleep SLEEP` in the background, any other streams SLOW s of ticks)
  limit [tag] CLAIM SECONDS [PLAN]
                                 answer with a 429 subscription usage limit (CLAIM: five_hour|seven_day|...|code_assist)
                                 that resets SECONDS from now
  tool [tag] '<json>'            script the stub's tool mode ({\"name\":\"Bash\",\"input\":...} or a list; \"@PROJ@\", \"@PORT@\");
                                 trigger it with `turn TAG toolstart`
  headers [tag] ['<json>']       set extra /v1/messages headers {\"on200\":{...},\"on429\":{...}} (\"@SEQ:BASE:STEP@\"; '{}' clears)
  account [tag] composed|off|show|'<json>'
                                 set the stub's claude.ai oauth/usage, profile and bootstrap route bodies (\"@ISO+N@\",
                                 \"@NOW+N@\": now+N s as an ISO instant, as unix seconds)
  claude-json [tag] [KEY ...] | --set KEY '<json>'|composed
                                 read or set keys in the scratch .claude.json (never the owner's file)
  statusline [tag] on [SECS]|off|show
                                 record (every SECS, default 5) or show what Claude Code pipes to its statusLine command
  transcript [tag]               print the newest scratch transcript's rows (type/subtype and system/user text)
  debuglog [tag] [regex]         print lines from `claude TAG --debug-log` (default: request/account/telemetry lines)
  settings [tag] '<json>'        deep-merge JSON into the scratch settings.json (null removes a key; read live)
  survey [tag] on|off [--reread] toggle CLAUDE_FORCE_DISPLAY_SURVEY in the scratch settings env (--reread: rewrite every
                                 3 s until Claude Code reports a re-read)
  setenv [tag] KEY [VALUE]       set KEY in the scratch settings.json env block (no VALUE: remove it)
  harness [tag] key=value ...    set [harness] keys in the scratch aterm.toml live (e.g. continue=false, approve=none)
  text|image|status|sid [tag]    read the tab: ctl text | PNG | ls+status | session id
  phase [tag]                    run `aterm drive phase` on the tab
  prices [<claude-bin>] [--codex <codex-bin>]
                                 check prices.toml against Claude Code's embedded price catalog and Codex's model ids
  wait [tag] <regex> [secs]      wait until a screen row matches <regex> (default 60 s; `await match`); exit 1 if none did
  ps [tag]                       list pids for [tag], or for every lab with a run directory under $LAB_STATE/runs
  journal [tag]                  print the harness journal and ledger
  watch [tag] [secs]             stream screen diffs and journal lines on one clock (default 30 s)
  stublog [tag] [N] | security [tag] | egress [tag] [MIN] | lsof [tag]
                                 the stub's log (last N lines, 40) | the keychain tripwire's log | egress witness, proxy
                                 refusals and sandbox denials (last MIN minutes, 30) | the lab's network sockets
  capture [tag] <name> \"<what>\"  write $LAB_STATE/captures/<agent>-<version>-<name>.txt (+png)
  catch [tag] <regex> <name> \"<what>\" [secs]
                                 capture a fixture the instant <regex> matches on screen (default 60 s)
  header [tag] \"<what>\"          print the one-line `# ...` fixture header for [tag]
  footers [--out DIR] [--subscriber] [--settings '<json>'] [--no-turn] [--no-bypass] [--columns N] [--lines N]
                                 capture managed Claude Code footer screens across modes on a fresh lab
  halfdraw scan <cast> --program claude|agy|muse|gemini [--show N] [--event I]
                                 replay a cast one PTY read per batch to report DEC 2026 brackets and unguarded busy gaps
  idle-points scan <tape|dir>... --program claude|agy|muse|gemini|auto [--show N]
                                 re-read harness tapes' turn-end points: idle with no evidence, and no last line named
  idle-points cast <cast>... --program claude|agy|muse|gemini [--show N]
                                 replay a cast: every idle spell inside a turn, how long it stood, whether it was vouched
  idle-points gate <lab logs dir>...
                                 place a Muse Code lab's harness points against its sessions' runs (mid-reply, in the gate)
  probe [tag] [--replica]        run jail and replica-home isolation probes (--replica: replica home only)
  selftest [tag] [--keep]        end-to-end Claude Code proof (~25 s: turn, survey, harness, egress, tripwire, probes, twin)
  nudge-trial [tag] [--mode on|log|off] [--trials N] [--expect on|log|off] [--via mode|key] [--no-hold]
                                 force N (1-3) PTY displacements on real Claude Code and verify the repaint nudge (~2.5 min)
  restore [tag] quit|crash [--keep] [--hold] [--clear] [--land|--land-open]
                                 cold-restore proof on a lab of its own under LAB_RESTORE=1 (~1 min): Claude Code takes
                                 a turn, aterm quits or is SIGKILLed, and the relaunched instance must resume the
                                 conversation with the expected Autopilot carry-on (default Cruise `step=continued`;
                                 --hold, crash only: the held tab stays held until lifted; --clear: `/clear` settles
                                 `step=done:taskless`; --land: the landed tab stays `task=landed` with
                                 `step=done:said-done`; --land-open: mid-work Land settles `step=done:landing` without
                                 typing Cruise nudges)
  forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-day|wall-climb|wall-shift|wall-redraw|wall-switch|wall-unread|wall-day-stop|wall-cruise|wall-relaunch|wall-bucket|resid|ladder] [--chains a,b] [--no-surfaces] [--window] [tag] [--keep]
                                 supervised Autopilot Land and Cruise scenario chains against real TUIs in scratch git
                                 repos (~15-30 min/harness):
                                 - land / cruise / both: L1-L11, L16-L17, C1-C2, P4, N1-N4, Off control, plus Settings >
                                   Harness window & 20 s idle wake/render checks (--no-surfaces skips window checks)
                                 - cover: L12 prose question, L13 person turn (task=theirs), L14/C3 P2 reads, P5, L15
                                   worker, C4, Off control
                                 - cap: continue_per_hour = 1 escalation and post-hour recovery (~72 min)
                                 - wall / wall-hid / wall-day / wall-climb: usage walls (W1 visible, W2 scrolled/dismissed,
                                   Gemini per-day quota rung 30m and climbed 2h)
                                 - wall-shift / wall-redraw / wall-switch / wall-unread: W3 a reset moved after each try,
                                   W4 the wall redrawn on a typed resume, W5 another agent in the tab under the wall, W6 an
                                   unread limit told an hour past its reset (~30 min each; wall-unread ~90 min)
                                 - wall-day-stop / wall-cruise / wall-relaunch / wall-bucket: W7 Gemini per-day dialog
                                   answered Stop, W8 Cruise at a wall, W9 a relaunch under a wall, W10 Gemini per-model
                                   bucket (Stop then resume, never the switch)
                                 - resid / ladder: residual and backoff-ladder scenario slices
                                 - --window (needs the land-p chain; refused under LAB_RESTORE=1): opens a real window and
                                   types L13's person prompt via NSEvent `hwkey` (`human_seq=` verified)
  crash [tag] [--gap SECS] [--keep-tree]
                                 SIGKILL the lab's own aterm instance and relaunch it on the same state after SECS (default
                                 0; --keep-tree leaves its child processes, for PTY keeper testing; refused on windowed labs)
  sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]
                                 emulate macOS sleep on a LAB_CLOCK=1 lab: SIGSTOP aterm, advance clock offset by SECS,
                                 hold --hold SECS, and SIGCONT (--at-act waits for first CONTINUED journal act)
  muse-relaunch [tag] [--keep]   live Muse Code crash/resume/aterm-restart lane (~4 min)
  muse-upgrade [tag] [--keep]    live Muse Code quiet upgrade lane across idle, background shell, and mid-turn (~9 min)
  agy-upgrade [tag] [--keep]     live agy quiet upgrade lane across idle, background/subagent/daemon tasks, and mid-turn (~12 min)
  agy-relaunch [tag] [--keep]    live agy crash/resume/Ctrl-Z/quit/aterm-restart lane (~10 min)
  gemini-relaunch [tag] [--keep] live Gemini CLI child/parent crash, resume, duplicate, and aterm-restart lane (~6 min)
  workers [tag] [--keep] [--early] [--upgrade <newer-claude-bin>]
                                 manager Claude Code spawns 3 real Claude Code workers, awaits all, and verifies results (~20 s)
  twin [tag] <claude-bin>|--crashing [ver]
                                 stage <claude-bin> (or a crashing stand-in) as a newer managed store build to wake live upgrade
  scn list|run|show|agg ...      survey scenario runner (`scn run <scenario> [--n N] [--start K]`, `show <run>`, `agg [<scenario>]`)
  tape promote <results-run|dir|tape> [--as NAME] [--fewer] | tape lint [FILE|DIR...] | tape replay-jailed [FILE|DIR...] | tape rerecord NAME [--fewer]
                                 promote, scrub, lint, sandbox-replay, or rerecord harness tapes in crates/aterm-agent/tests/testdata/tapes/
  ctl [tag] <verb> ...           run raw `aterm ctl` against [tag]'s control socket
  protected HOME CCSOCKS         print owner paths denied by the jail as \"<kind> TAB <path>\" lines
  paths                          print LAB_STATE and resolved LAB_ATERM
  down [tag] [--keep-dumps]      end everything in [tag]'s jail, remove its scratch and runtime state, and keep its logs
                                 (--keep-dumps: its scratch dumps too)

Outside any lab (no jail, no stub):
  instance [--window] [--band app|agent] [--aterm PATH] [--columns N] [--lines N] [--font-px N] [--scale N] [--scene band-icons]
           [--env NAME=VALUE]... [--toml LINE]... [--toml-after LINE]... [--harness-key LINE]... [--home DIR] [--harness]
           [--reroute-prefix DIR] [--pre VERB]... [--image PNG] [--post VERB]... [--pages cold] [--keep] [-- PROGRAM ARGS]
                                 run a private aterm in a scratch HOME/XDG world, print its socket and hold it until
                                 Ctrl-C (--window: a real window; --image PNG: capture one settled frame, run --post, and
                                 end; --pages cold: run a copy whose pages are evicted, to measure cold page-ins;
                                 --reroute-prefix DIR: DIR, new, is the HOME, its atpkg prefix holding a clone of the
                                 managed Claude Code's store build with its twin and reroute stub, and the instance reroutes)
                                 --scene band-icons draws procedural chrome icons beside H (debug/dev-seams build)
  abrew-admission [--json] [--indexes DIR] [--this-machine | --arch A --macos V --xcode V|none --no-clt]
                                 evaluate abrew admission of Homebrew's formula.json and cask.json (fetched from
                                 formulae.brew.sh unless --indexes DIR holds them) on the newest Mac, that Mac with the
                                 named facts replaced, or this one (--this-machine)
  fleet-sim [--n N] [--runs R] [--keep] [--record]
                                 run N (default 30, 4-64) private headless aterms on loopback fleet ports and verify
                                 `aterm ctl survey --fleet` across R runs (default 3) under `ulimit -n 256` (--record:
                                 a passing run's record goes to docs/measured/)
  disk-lane [--aterm PATH] [--keep]
                                 mount an 8 GiB sparse APFS image and verify disk-pressure tier eviction and reserve order (~1 min)
  slot-capture [--watch DUR [--dir DIR]]
                                 verify `build-slot` record parsing in a scratch BUILD_SLOT_DIR, or watch ~/.cache/build-slot vs lsof
  slot-lane [--aterm PATH] [--keep]
                                 verify `await sys slots>=1` wakes when a SIGKILLed stand-in releases slot-0
  trail-gallery [--aterm PATH] [--theme dark|light|both] [--scenario NAME[,NAME]]... [--extra 'key = value']...
                [--columns N] [--lines N] [--font-px N] [--no-clean] [--keep] <out_dir> [style ...]
                                 render cursor trail styles across themes/scenarios into <out_dir> with sidecars (`--help` for details)
  trail-contrast <png> [--find TEXT]... [--cells ROW:COL[-COL]]... [--row N]... [--newest N] [--min X]
                 [--newest-min X] [--caret-min X] [--dims FILE|--origin X,Y --cell WxH] [--no-clean] ...
                                 compute WCAG contrast JSON from trail-gallery frames and sidecars (`--help` for details)
  aos-repair [--aterm PATH] [--control | --publish PLAN_HEX] [--keep] [--out DIR]
                                 OPENS A WINDOW: verify Aos M2 live repair of a stranded momentum glow, or publish an Aos M4 plan against a control (~25 s; `--help` for details)
  aos-search [--grid] [--plans FILE] --out DIR [--toolchain LINE] | aos-search --verify FILE
                                 run Aos M4's candidate search under the pinned policy, or verify a bundle FILE offline (`--help` for details)
  aos-workers-bench --plans FILE --out DIR [--lab PATH] [--faults] [--rounds N]
                                 design §11.8's pre-registered M5b measurement: six aos-search arms in a Williams order, gates and criteria in DIR/record.txt (~60 min, ~70 with --faults; `--help` for details)
  aos-phase TRACE --build BUILD [--plan PLAN_HEX]
                                 re-derive a windowed aos-repair run's measured phase from its kept trace (`--help` for details)
  resource-attribution [--aterm PATH]
                                 private two-tab delegated resource attribution check
  presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases \"A H B D T X\"] [--trials N] [--keep]
                                 OPENS A WINDOW (with --window): verify zero re-grid flap across fenced/harness turns and chrome pairs (~3 min)
  meta-keys --window [--layout ID]... [--aterm PATH] [--keep] | meta-keys --plan [--layout ID]... | meta-keys --restore
                                 OPENS A WINDOW (with --window): verify Option chords and dead keys across U.S., German, and French layouts (~1 min)
  predict-live --window|--headless [--aterm PATH] [--shell zsh|bash]... [--rtt MS]... [--mode on|off]...
               [--rounds N] [--no-split] [--out DIR]
                                 OPENS A WINDOW (with --window): verify predictive editing over a simulated RTT `link` in
                                 single and split panes (defaults: both shells, 100 ms, both modes, 2 rounds)
  codex-alab-proof [--package DIR] [--upgrade-package DIR] [--aterm PATH]
                   [--policy-only|--upgrade-only|--raw-upgrade-only]
                                 end-to-end native harness proof on the real codex-alab package against a loopback fake
                                 model (~11 min; --policy-only: the cybersecurity-refusal leg alone; --upgrade-only: one
                                 live handoff; --raw-upgrade-only: started through the produced primary with its managed
                                 entry absent, automatic crash repair, then a live handoff); its scratch world is always
                                 kept
  seamless --baseline PATH [--aterm PATH] [--runs N] [--keep]
                                 OPENS A WINDOW, one at a time: one instance of a debug or dev-seams aterm across a real
                                 seamless update (ATERM_DEBUG_SEAMLESS_REEXEC=1, `update apply`), legs gap, topics,
                                 gaptopic, redial, redial75, latch and hwkey; then present: N (default 4, at least 2)
                                 fresh instances each of the tested and the baseline aterm, alternating, the tested
                                 medians judged against the baseline runs (n_present inside their range,
                                 present_p50/p95/p99_ms at most their highest) (~1-2 min, plus ~1 min a run)
  reduce <transcript|rollout> --program claude|codex [--out FILE]
                                 key-only transcript/rollout reducer for harness step fixtures (verifies reader equivalence before writing)
  ptyrec <log> -- <cmd> [args...]
                                 run <cmd> behind a pass-through PTY and record `<t_ns> I|O <hex>` chunks to <log>
  link [--rtt MS] [--take FILE] -- <cmd> [args...]
                                 run <cmd> behind a PTY delaying chunks by RTT/2 each way (default 100 ms)
  mac dmg [--image REF] [--fda] [--keep]
                                 run dmg_acceptance benches inside a fresh SIP-enabled macOS tart VM (`mac help`)
  linux up|down|status|exec|proof|baseline|gone|cpu|a11y|edges|deck|browser|vz-agent|clean|destroy
                                 manage and run the Ubuntu 24.04 Lima VM runtime lane on aarch64 and Rosetta x86_64 (`linux help`)
  windows up|down|status|exec|smoke|window|destroy
                                 manage and run the Windows 11 arm64 QEMU VM runtime lane on aarch64 and Prism x86_64 (`windows help`)

Exit: 0 pass, 1 a finding or a refusal (also an unknown command and, on many verbs, a bad argument), 2 usage,
3 could not run, 128+N cut short by signal N (INT, TERM, HUP).
";

/// Why a verb ended: a message for stderr (when there is one), its exit status, and the interrupt that cut it short (when one did).
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
        "halfdraw" => halfdraw::cmd_halfdraw(args),
        "idle-points" => idlepoints::cmd_idle_points(args),
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
        "aos-search" => aos_search::cmd_aos_search(args),
        "aos-workers-bench" => aos_workers_bench::cmd_aos_workers_bench(args),
        "aos-phase" => aos_phase::cmd_aos_phase(args),
        "presence-flap" => presence::cmd_presence_flap(args),
        "meta-keys" => meta_keys::cmd_meta_keys(args),
        "predict-live" => predict_live::cmd_predict_live(args),
        "codex-alab-proof" => codex_alab_proof::cmd_codex_alab_proof(args),
        "seamless" => seamless::cmd_seamless(args),
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

/// Before anything is made: the owner's home must be known (the account's or
/// `HOME`'s), and the state dir must sit outside every home the jail guards
/// ([`paths::Paths::need_state_outside`]) and the owner's data, be quotable in
/// a sandbox profile, and, when it is the default one, be this user's private
/// directory ([`paths::Paths::need_state`]).
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
    let code = match std::path::Path::new(&argv0)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some("node") => relay::main(&args),
        Some(name @ ("zsh" | "-zsh" | "bash" | "-bash")) => link::as_shell(name, &args),
        Some("security") => tripwire::main(&args),
        Some("open") => recorder::main(&args),
        Some("claude") => fleet_fixture::claude(&args),
        Some("build-loop") => fleet_fixture::build_loop(&args),
        Some("ps") => codex_rig::ps_main(&args),
        _ => {
            let cmd = args.first().cloned().unwrap_or_default();
            let rest = args.get(1..).unwrap_or_default();
            match cmd.as_str() {
                "stub" => stub::main(rest),
                "sampler" => sampler::main(rest),
                "ctty" => relay::ctty(rest),
                "ptyrec" => relay::ptyrec(rest),
                "link" => link::main(rest),
                "probe-inside" => probe::inside_main(rest),
                "probe-tiocsti" => probe::tiocsti_child(),
                "hold" => recorder::hold(rest),
                "keeper" => instance::keeper_main(rest),
                "band" => band::band_main(rest),
                "band-launch" => band::launch_main(rest),
                "band-standin" => band::standin_main(rest),
                "presence-standin" => presence::standin_main(rest),
                slot_lane::HOLD_VERB => slot_lane::hold_main(rest),
                aos_workers::WORKER_VERB => aos_workers::worker_main(rest),
                "key-dump" => meta_keys::keydump_main(rest),
                "mcp-standin" => codex_rig::mcp_main(rest),
                _ => {
                    sys::forget_env(paths::forgotten);
                    sys::confine_env();
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
