// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-agent lab: launches the real Claude Code, Codex ([`codex`]), Antigravity CLI (`agy`, [`agy`]; screens in [`agy_captures`]), or Muse Code ([`muse`]; proof in [`muse_proof`]) TUI by path inside a private headless `aterm` against a local API stub (Anthropic, OpenAI Responses, Gemini, or Meta), and runs private aterm instances for checking a change. [`USAGE`] lists the verbs.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
mod abrew_admission;
#[cfg(target_os = "macos")]
mod admission;
#[cfg(target_os = "macos")]
mod agy;
#[cfg(target_os = "macos")]
mod agy_captures;
#[cfg(target_os = "macos")]
mod agy_proof;
#[cfg(target_os = "macos")]
mod aos_gpu_bench;
#[cfg(target_os = "macos")]
mod aos_learn;
#[cfg(target_os = "macos")]
mod aos_learn_bench;
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
mod autopilot_captures;
#[cfg(target_os = "macos")]
mod autopilot_scenes;
#[cfg(target_os = "macos")]
mod band;
#[cfg(target_os = "macos")]
mod bus;
#[cfg(target_os = "macos")]
mod claude_login_proof;
#[cfg(target_os = "macos")]
mod claude_measure;
#[cfg(target_os = "macos")]
mod claude_rotation_proof;
#[cfg(target_os = "macos")]
mod clock;
#[cfg(target_os = "macos")]
mod codex;
#[cfg(target_os = "macos")]
mod codex_alab_model;
#[cfg(target_os = "macos")]
mod codex_alab_proof;
#[cfg(target_os = "macos")]
mod codex_measure;
#[cfg(target_os = "macos")]
mod codex_proof;
#[cfg(target_os = "macos")]
mod codex_rig;
#[cfg(target_os = "macos")]
mod commons_lan;
#[cfg(target_os = "macos")]
mod commons_startup;
#[cfg(target_os = "macos")]
mod crash;
#[cfg(target_os = "macos")]
mod disk_ask_lane;
#[cfg(target_os = "macos")]
mod disk_lane;
#[cfg(target_os = "macos")]
mod door;
#[cfg(target_os = "macos")]
mod fleet_fixture;
#[cfg(target_os = "macos")]
mod fleet_judge;
#[cfg(target_os = "macos")]
mod fleet_menu;
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
mod lab_build;
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
mod linux_driver;
#[cfg(target_os = "macos")]
mod linux_edges;
#[cfg(target_os = "macos")]
mod linux_gone;
#[cfg(target_os = "macos")]
mod linux_gpu;
#[cfg(target_os = "macos")]
mod linux_guest;
#[cfg(target_os = "macos")]
mod linux_owned;
#[cfg(target_os = "macos")]
mod linux_proof;
#[cfg(target_os = "macos")]
mod linux_vz;
#[cfg(target_os = "macos")]
mod linux_vz_driver;
#[cfg(target_os = "macos")]
mod linux_vz_host;
#[cfg(target_os = "macos")]
mod linux_xwire;
#[cfg(target_os = "macos")]
mod look_away;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod mac_nux;
#[cfg(target_os = "macos")]
mod mac_nux_guest;
#[cfg(target_os = "macos")]
mod measure;
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
mod relaunch_muse;
#[cfg(target_os = "macos")]
mod relay;
#[cfg(target_os = "macos")]
mod reroute_prefix;
#[cfg(target_os = "macos")]
mod resource_attribution;
#[cfg(target_os = "macos")]
mod restart_jobs;
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
mod seamless_carry;
#[cfg(target_os = "macos")]
mod selftest;
#[cfg(target_os = "macos")]
mod settings_witness;
#[cfg(target_os = "macos")]
mod sleep;
#[cfg(target_os = "macos")]
mod slot_lane;
#[cfg(target_os = "macos")]
mod spread;
#[cfg(target_os = "macos")]
mod stats;
#[cfg(target_os = "macos")]
mod stop;
#[cfg(target_os = "macos")]
mod stub;
#[cfg(target_os = "macos")]
mod swarm;
#[cfg(target_os = "macos")]
mod sys;
#[cfg(target_os = "macos")]
mod tape;
#[cfg(target_os = "macos")]
mod trail_census;
#[cfg(target_os = "macos")]
mod trail_companion;
#[cfg(target_os = "macos")]
mod trail_contrast;
#[cfg(target_os = "macos")]
mod trail_diff;
#[cfg(target_os = "macos")]
mod trail_gallery;
#[cfg(target_os = "macos")]
mod trail_hue;
#[cfg(target_os = "macos")]
mod trail_keys;
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
mod vm_admit;
#[cfg(target_os = "macos")]
mod windows;
#[cfg(target_os = "macos")]
mod windows_desk;
#[cfg(target_os = "macos")]
mod windows_guest;
#[cfg(target_os = "macos")]
mod windows_smoke;
#[cfg(target_os = "macos")]
mod windows_uia;
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
aterm-lab — the real Claude Code, Codex, agy or Muse Code TUI in a jailed private headless aterm
against a local API stub (Anthropic, Responses, Gemini or Meta). A lab is fully local: no real account, token or egress.

usage: targo --unverified lab <command> [tag] [args]
       (tag: the first word when it names a lab that is up, else $LAB_TAG or \"lab\" and the word is an argument;
       up takes one optional tag beside --window, --fabric and --tab-strip; down, ps, crash and sleep take any
       first word as the tag;
       a verb that brings up its own lab defaults to a tag of its own)
  up [tag] [--window] [--fabric] [--tab-strip]
                                 start the stub, a black-hole proxy, the egress witness and a jailed private aterm
                                 (headless by default; --window opens one real window in the same jail)
                                 --fabric: the lab's own bus: a private broker in the jail and the lab's socket dir, node and
                                 observer caps (<run>/fabric), the [fabric] table and the rendezvous file `aterm link` reads,
                                 the bridge connected before up returns; `down` ends it (refused beside a LAB_ATERM_TOML [fabric])
                                 --tab-strip: tab_strip_rows = 1, the row whose tab menu holds the session clause (`open
                                 tab-menu`); it takes a grid row (refused beside a LAB_ATERM_TOML tab_strip_rows)
                                 (env: LAB_SURVEY=1 LAB_TRUST=1 LAB_FIRST_RUN=1 LAB_RESTORE=1 LAB_CLOCK=1
                                 LAB_HARNESS_KEYS='continue = false' LAB_ATERM_TOML LAB_COLS LAB_LINES LAB_SCALE
                                 LAB_PROXY=dead LAB_CLAUDE LAB_CODEX LAB_AGY LAB_MUSE
                                 LAB_ATERM=/physical/path LAB_STATE=/abs/outside/homes
                                 LAB_INSTANCE_ENV='TMPDIR=/abs ATERM_DEBUG_<NAME>=V ATERM_DEV_<NAME>=V ATERM_OCCUPANCY_NUDGE_S=N'
                                 LAB_HOME_IMAGE=<n>g|<n>m: the scratch home on a sparse APFS image of that size (1 GiB to 64 GiB)
                                 LAB_REROUTE=1: the lab's Claude Code store build cloned into the jail's own atpkg
                                 prefix with its twin and reroute stub, the instance rerouting; `claude --by-name`
                                 LAB_JAIL_SCAN=1: the instance's disk scans read the jail's processes alone (a debug
                                 build's ATERM_DEBUG_JAIL_MARK), so `aterm harness disk free` can free in the lab)
  claude [tag] [-e K=V]... [--no-wait] [--relay] [--subscriber] [--debug-log] [--by-name] [-- claude args]
                                 launch the managed Claude Code in tab 1 (--relay: behind the PTY relay; --subscriber: a
                                 fake claude.ai OAuth token, for `limit` headers; --debug-log: a scratch debug log;
                                 --by-name: types `claude` at the prompt as a person does, the lab's environment as
                                 assignment words before it; LAB_REROUTE=1 labs only)
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
  agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [--setting KEY=VALUE]... [-- agy args]
                                 launch agy in tab 1 against the stub Gemini API (--onboarding: the first-run screens and
                                 the trust dialog; --setting: one settings.json key)
  agy-selftest [tag] [--keep]    end-to-end agy isolation and stub proof (~30 s)
  agy-captures [--only <regex>]  capture every agy fixture screen (~16 min; --only narrows)
  muse [tag] [-e K=V]... [--no-wait] [-- muse args]
                                 launch Muse Code in tab 1 against the stub's Meta routes with a scratch HOME
  muse-selftest [tag] [--keep]   end-to-end Muse Code isolation and stub proof (~30 s)
  muse-captures [--only <regex>] capture every Muse Code fixture screen (~10 min; --only narrows)
  autopilot-captures --harness all|claude|agy|muse|codex-alab [--scale 1|2] [--look day|night|both]
                                 Autopilot's scenes on every harness, one headless instance each per look, launched in it
                                 (the forever lab's chains; codex-alab-proof's legs): an image per scene, the chip's whole
                                 hovered title at 400 columns, chrome and status beside each png, manifest.tsv; a paired
                                 agy typing take (--baseline ATERM, alone without --harness) (~40 min a look and
                                 scale; runs side by side share one folder)
  turn|send|enter|key [tag] ...  type prompt+Enter | text | Enter | named key (0, escape, ctrl+c, ...); turn exits 1
                                 unless the composer accepted the prompt
  mode [tag] [name ['<spec>']]   show or set the stub mode: ok long numbered overloaded too-long compact-fails slow
                                 rate-limit 401 500 bad-key bg[:SLEEP[:SLOW]] tool limit:CLAIM:RESET[:PLAN], or a
                                 custom name whose '<spec>' JSON object is written to modes/<name>.json (bg: a prompt
                                 with `bgstart` runs `sleep SLEEP` in the background, any other streams SLOW s of ticks)
  limit [tag] CLAIM SECONDS [PLAN]
                                 answer with a 429 subscription usage limit (CLAIM: five_hour|seven_day|...)
                                 that resets SECONDS from now
  tool [tag] '<json>'            script the stub's tool mode ({\"name\":\"Bash\",\"input\":...} or a list; \"@PROJ@\", \"@PORT@\");
                                 trigger it with `turn TAG toolstart`
  headers [tag] ['<json>']       set extra /v1/messages headers {\"on200\":{...},\"on429\":{...}} (\"@SEQ:BASE:STEP@\"; '{}' clears)
  headers-for [tag] LABEL ['<json>']
                                 the same blocks for one credential alone (LABEL as the stub logs it: oat:lab-a, key),
                                 merged over the shared ones
  mode-for [tag] LABEL MODE|off  answer one credential's /v1/messages turns in MODE (any mode name; off: the stub's own)
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
  journal [tag] [@sid]           print the harness journal and ledger (the first tab's, or @sid's)
  watch [tag] [secs]             stream screen diffs and journal lines on one clock (default 30 s)
  stublog [tag] [N] | security [tag] | egress [tag] [MIN] | lsof [tag]
                                 the stub's log (last N lines, 40) | the keychain tripwire's log | egress witness, proxy
                                 refusals and sandbox denials (last MIN minutes, 30) | the lab's network sockets
  capture [tag] [@sid] <name> \"<what>\"
                                 write $LAB_STATE/captures/<agent>-<version>-<name>.txt (+png) of the active tab, or @sid
  catch [tag] <regex> <name> \"<what>\" [secs]
                                 capture a fixture the instant <regex> matches on screen (default 60 s)
  header [tag] [@sid] \"<what>\"   print the one-line `# ...` fixture header for [tag]'s active tab, or @sid, naming
                                 the launch that ran there (the stub's route, or Claude Code's default route)
  footers [--out DIR] [--subscriber] [--settings '<json>'] [--no-turn] [--no-bypass] [--columns N] [--lines N]
                                 capture managed Claude Code footer screens across modes on a fresh lab
  trail-keys [--vim] [--keep]
                                 on a fresh lab, real Claude Code fullscreen with its caret drawn, Rainbow Kitty on: whether
                                 the composer's keys light the trail after a drag over a path, Escape, Ctrl-L, Ctrl-C, Escape or
                                 Ctrl-C interrupting a streaming turn and the wheel (`--vim`: NORMAL's motions stay dark, `i`
                                 and `a` light typing again); exit 1 on a mismatch, 3 when it could not run (~1 min a mode)
  halfdraw scan <cast> --program claude|agy|muse [--show N] [--event I]
                                 replay a cast one PTY read per batch to report DEC 2026 brackets and unguarded busy gaps
  idle-points scan <tape|dir>... --program claude|agy|muse|auto [--show N]
                                 re-read harness tapes' turn-end points: idle with no evidence, and no last line named
  idle-points cast <cast>... --program claude|agy|muse [--show N]
                                 replay a cast: every idle spell inside a turn, how long it stood, whether it was vouched
  idle-points gate <lab logs dir>...
                                 place a Muse Code lab's harness points against its sessions' runs (mid-reply, in the gate)
  restart-jobs [tag]             whether the upgrade restart's SIGTERM ends Claude Code's background shells, against
                                 upgrade::RESTART_ENDS_BACKGROUND (exit 0 agrees, 1 disagrees, 3 could not run)
  probe [tag] [--replica]        run jail and replica-home isolation probes (--replica: replica home only)
  selftest [tag] [--keep]        end-to-end Claude Code proof (~25 s: turn, survey, harness, egress, tripwire, probes, twin)
  claude-login-proof [tag] [--keep]
                                 the real Claude Code's lost login on its default route, end to end (~5 min): a roster
                                 row's lost login waits on aterm's sign-in (S1), one no row names gets /login (S2), the
                                 row added by `accounts add` backs aterm out of its picker before the host's own look at
                                 the roster (S3), a person's /login is left alone (S3b); no browser, no `auth login`, no
                                 egress, no owner's keychain item; a verdict table, exit 1 on FAIL (--keep: judged live)
  claude-rotation-proof [tag] [--keep] [--no-route-seam]
                                 the real Claude Code moved between the roster accounts a fleet file names, end to end
                                 (~25 min): a wall moves the tab with its conversation (R1), no usable row waits for the
                                 first to open and moves back (R3), a row whose login directory is gone gets an automatic
                                 sign-in attempt (recorded no-route) while the tab waits on its own reset and resumes
                                 (R4), a sign-in on the default login makes its row usable and displaces the one it held,
                                 there and back (R5a, R5b), the usage tap's high figure moves it before any wall (R2);
                                 nobody asked, no /login, no browser, no egress; a verdict table, exit 1 on FAIL
                                 (--no-route-seam: the live negative control, R1 must FAIL and its control row PASS)
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
  forever --harness <claude|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-redraw|wall-switch|wall-unread|wall-cruise|wall-relaunch|resid|ladder|angles|switch|outage-land|outage-cruise] [--chains a,b] [--no-surfaces] [--window] [tag] [--keep]
                                 supervised Autopilot Land and Cruise scenario chains against real TUIs in scratch git
                                 repos (~15-30 min/harness):
                                 - land / cruise / both: L1-L11, L16-L17, C1-C2, HL/HC (same-land, same-cruise), P4,
                                   N1-N4, Off control, plus Settings > Harness window & 20 s idle wake/render checks
                                   (--no-surfaces skips window checks); a chain aterm stops typing into is watched 60 s
                                   past its hold, brought in front twice, and must say `stopped typing` (the
                                   chip or hover, the state line, `status held=`, the journal's `held=`), with
                                   one `Stopped typing` Messages row only for Land over a repository leg, gone
                                   once same-land's person types their turn
                                 - cover: L12 prose question, L13 person turn (task=theirs), L14/C3 P2 reads, P5, L15
                                   worker, C4, Off control
                                 - cap: continue_per_hour = 1 escalation and post-hour recovery (~72 min)
                                 - wall / wall-hid: usage walls (W1 visible, W2 scrolled away)
                                 - wall-redraw / wall-switch / wall-unread: W4 the wall redrawn on a typed resume, W5 another
                                   agent in the tab under the wall, W6 an unread limit told an hour past its reset (~30 min
                                   each; wall-unread ~90 min)
                                 - wall-cruise / wall-relaunch: W8 Cruise at a wall, W9 a relaunch under a wall
                                 - outage-land / outage-cruise: O1 an API overload from Land's first prompt, O2 one met
                                   by Cruise (Claude Code only): tries on the outage's rungs alone (15 s doubling, the
                                   ladders at an eighth), `status waiting=overload`, one resume after the lift, then
                                   work: Land lands, Cruise goes on (~5 min each)
                                 - resid / ladder: residual and backoff-ladder scenario slices
                                 - angles: cruise-angles, each done claim after real work asked another of the six kinds
                                   of work in turn, never held past the sixth; the back-off ladder at an eighth (~30 min)
                                 - switch: a running Cruise tab switched to Land (`meta set autopilot land`): at rest in its
                                   back-off (land@v1 within 5 s, before the due), under a standing draft (nothing typed until
                                   it clears and the hold for the person who typed it ends, then land@v1 within 5 s),
                                   flipped to Land then Off within 300 ms (nothing typed), held at the same reply (still
                                   held, its state line still `stopped typing:`), and 5 s into a long busy turn (noted in
                                   the turn, nothing mid-turn, land@v1 within 3 s of its point, no back-off before it);
                                   land, cruise and both also want land@v1 within 10 s of the first point (~10 min)
                                 - --window (needs the land-p or same-land chain; refused under LAB_RESTORE=1): opens a real
                                   window, types L13's person prompt via NSEvent `hwkey` (`human_seq=` verified), and reads
                                   same-land's hold (its chip's and its band's screen-reader nodes) off it
  crash [tag] [--gap SECS] [--keep-tree]
                                 SIGKILL the lab's own aterm instance and relaunch it on the same state after SECS (default
                                 0; --keep-tree leaves its child processes, for PTY keeper testing; refused on a windowed lab
                                 whose recorded pid may be its window's keeper, so crash an `up --window` lab's)
  sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]
                                 emulate macOS sleep on a LAB_CLOCK=1 lab: SIGSTOP aterm, advance clock offset by SECS,
                                 hold --hold SECS, and SIGCONT (--at-act waits for first CONTINUED journal act)
  muse-relaunch [tag] [--keep]   live Muse Code crash/resume/aterm-restart lane (~4 min)
  muse-upgrade [tag] [--keep]    live Muse Code quiet upgrade lane across idle, background shell, and mid-turn (~9 min)
  agy-upgrade [tag] [--keep]     live agy quiet upgrade lane across idle, background/subagent/daemon tasks, and mid-turn (~12 min)
  agy-relaunch [tag] [--keep]    live agy crash/resume/Ctrl-Z/quit/aterm-restart lane (~10 min)
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
  instance [--window] [--band app|agent] [--aterm PATH] [--columns N] [--lines N] [--font-px N] [--scale N] [--scene band-icons|harness-layouts]
           [--env NAME=VALUE]... [--toml LINE]... [--toml-after LINE]... [--harness-key LINE]... [--home DIR] [--harness]
           [--reroute-prefix DIR] [--pre VERB]... [--image PNG] [--post VERB]... [--pages cold] [--keep] [-- PROGRAM ARGS]
                                 run a private aterm in a scratch HOME/XDG world, print its socket and hold it until
                                 Ctrl-C (--window: a real window; --band: app (the default) runs it as its own launchd
                                 job, in a coalition of its own as aterm.app is, a window under its keeper there, and agent
                                 as this command's child, in its coalition; --image PNG: capture one settled frame, run
                                 --post, and end; --pages cold: run a copy whose pages are evicted, to measure cold page-ins;
                                 --reroute-prefix DIR: DIR, new, is the HOME, its atpkg prefix holding a clone of the
                                 managed Claude Code's store build with its twin and reroute stub, and the instance reroutes)
                                 --scene band-icons draws procedural chrome icons beside H (debug/dev-seams build)
                                 --scene harness-layouts draws labeled synthetic footer states through the real renderer
                                 (debug/dev-seams build; no live agent, upgrade candidate or account is read)
  abrew-admission [--json] [--indexes DIR] [--this-machine | --arch A --macos V --xcode V|none --no-clt]
                                 evaluate abrew admission of Homebrew's formula.json and cask.json (fetched from
                                 formulae.brew.sh unless --indexes DIR holds them) on the newest Mac, that Mac with the
                                 named facts replaced, or this one (--this-machine)
  commons-lan prepare-recipient|prepare-source|join|run|follow|stop|declare|write|read …
                                 prepare preserved private profiles on separate LAN hosts; synthetic prior vault approval,
                                 ordinary executable startup, exact generic table readback, no installed state changes
  commons-startup <aterm> <new-output-directory> [--log-backed] [--suppressed | --observe SECONDS]
                                 start an ordinary window and node host on private custody and loopback Commons;
                                 require enrollment, fresh signed table data in both directions and recovery after restart
                                 (--suppressed: negative control, whose private socket prevents window runtime startup;
                                 --observe: keep the successful window running 1-60 s, then save its frame and live state)
  fleet-menu <aterm> <new-output-directory>
                                 run an isolated reader window and a peer with a sibling on loopback; retain the
                                 peer's two-instance survey, the reader's menu dump and frame (all private worlds kept)
  fleet-sim [--n N] [--runs R] [--keep] [--record]
                                 run N (default 30, 4-64) private headless aterms on loopback fleet ports and verify
                                 `aterm ctl survey --fleet` across R runs (default 3) under `ulimit -n 256` (--record:
                                 a passing run's record goes to docs/measured/)
  swarm rotate [--aterm PATH] [--keep]
                                 run a private headless aterm with two enrolled Claude Code logins, wall the first through
                                 the usage-tap seam (`harness tap`), verify `rotations[]` grows, and serve a turn on the second
  swarm mailbox-signin [--live] [--email EMAIL] [--label LABEL] [--mailbox PATH] [--record] [--aterm PATH] [--keep]
                                 verify an unenrolled Claude row records `no-route` while mailbox config is withheld, then
                                 signs in through `ai-mailbox.toml`, ends enrolled, and serves a proven turn (--live: run
                                 against a real Claude account and ~/.secrets/ai-mailbox.toml; --record: write docs/measured/)
  swarm partition [--one-url] [--writers both|a] [--hold SECS] [--aterm PATH] [--keep] [--record]
                                 split five private headless aterms and a vault-only Mac on loopback into two islands
                                 (the ATERM_DEBUG_PARTITION cut), publish and admit on each side, heal, and report what
                                 the vault and the fleet did, with timings and what was lost (--one-url: F8's negative
                                 control; --record: write docs/measured/)
  disk-lane [--aterm PATH] [--keep]
                                 mount an 8 GiB sparse APFS image and verify disk-pressure tier eviction and reserve order (~1 min)
  disk-ask [tag] [--keep] [--scenario clear|partial|unanswered|exhausted|worker]
                                 real Claude Code tabs in a lab whose HOME is a sparse APFS image (10-16 GiB) below
                                 its floor once aterm's own pass is spent: verify the disk-free@v1 ask types its line to
                                 the one tab whose checkout holds the most, naming that build as the tab's own, and the
                                 ledger and `aterm harness disk why` credit its cleanup. clear (the default): the cleanup
                                 clears the volume and ends the episode, and, the volume filled short again, the cooldown
                                 opens none, aterm's disk worker runs instead and no tab gets a second line (~15 min).
                                 partial: the cleanup frees a GiB or more with the volume still short, and the settled
                                 episode asks nobody else (~10 min). unanswered: the asked tab answers with words alone,
                                 its ask is handed off once its 20-minute answer window passes, aterm's disk worker runs
                                 first, then the next largest build's tab is asked and frees, settling the episode, so
                                 the third tab is never asked (~38 min). exhausted: the cap on lines; the second tab
                                 asked answers with words too, the episode runs out at its two lines, no second worker
                                 runs and the third tab is never asked (~60 min). worker: two tabs each hold a draft, so
                                 none is asked and aterm's disk worker runs: its `rm -rf` of a tool cache is declined,
                                 `aterm harness disk free` removes the cache (journalled, the volume risen by its size),
                                 it reports `Nothing left:` and finishes, and no tab is typed or loses its draft
                                 (~15 min). Every worker runs real Claude Code, started by name through the lab's
                                 reroute (LAB_REROUTE), and each scenario fails unless it finishes, in its own folder
                                 and a session of its own, its task in no tab's conversation
  slot-capture [--watch DUR [--dir DIR]]
                                 verify `build-slot` record parsing in a scratch BUILD_SLOT_DIR, or watch ~/.cache/build-slot vs the holder scan
  slot-lane [--aterm PATH] [--keep]
                                 verify `await sys slots>=1` wakes when a SIGKILLed stand-in releases slot-0
  trail-gallery [--aterm PATH] [--theme dark|light|both] [--scenario NAME[,NAME]]... [--extra 'key = value']...
                [--columns N] [--lines N] [--font-px N] [--no-clean] [--keep] <out_dir> [style ...]
                                 render cursor trail styles across themes/scenarios into <out_dir> with sidecars (`--help` for details)
  trail-contrast <png> [--find TEXT]... [--cells ROW:COL[-COL]]... [--row N]... [--newest N] [--min X]
                 [--newest-min X] [--caret-min X] [--dims FILE|--origin X,Y --cell WxH] [--no-clean] ...
                                 compute WCAG contrast JSON from trail-gallery frames and sidecars (`--help` for details)
  trail-census <png> [--dims FILE|--origin X,Y --cell WxH] [--clean PNG] [--caret ROW,COL] [--pet X0,Y0,X1,Y1]...
               [--selection RRGGBB] [--pixels FILE] [--mud-max N] [--glyph-min X] ...
                                 the Prism Glass census of a trail frame and its clean twin, layer by layer: OKLab mud,
                                 glyph contrast, the band's steps, seams, the stroke's hang, stars, caret (`--help` for details)
  trail-companion <path>... [--shave SHARE] [--max N] [--list all|hits]
                                 the companion over the letters: each gallery frame's pet_body against its lettered cells,
                                 the caret and the word icons, from the sidecars (`--help` for details)
  trail-hue <path>... [--min-px N] [--max N] [--list all|hits]
                                 the hue rulings: Fire's pixels never green, Sparkle's in the gold family, each gallery
                                 frame read against its clean twin (`--help` for details)
  trail-diff [--base REF|merge-base] [--control same|plant|dark-arm] [--keep] [--jobs N] [SCRIPT.trail|DIR]...
                                 Prism Glass's dark proof: build aterm-gui's trail_dump here and in a worktree of the base
                                 (HEAD~1), run both on the scripts and compare main's state and streams (`--help` for details)
  aos-repair [--aterm PATH] [--control | --publish PLAN_HEX] [--keep] [--out DIR]
                                 OPENS A WINDOW: verify Aos M2 live repair of a stranded momentum glow, or publish an Aos M4 plan against a control (~25 s; `--help` for details)
  aos-search [--grid] [--plans FILE] --out DIR [--toolchain LINE] | aos-search --verify FILE
                                 run Aos M4's candidate search under the pinned policy, or verify a bundle FILE offline (`--help` for details)
  aos-workers-bench --plans FILE --out DIR [--lab PATH] [--faults] [--rounds N]
                                 design §11.8's pre-registered M5b measurement: six aos-search arms in a Williams order, gates and criteria in DIR/record.txt (~60 min, ~70 with --faults; `--help` for details)
  aos-gpu-bench --harness PATH --out DIR [--rounds N]
                                 design §12.9's pre-registered M5a measurement: four aos_gpu arms (F I T G) in a Williams order, gates and criteria in DIR/record.md (~25 min; `--help` for details)
  aos-learn VERB ...
                                 Aos M6's learner: build, init, chain, train, rollback, verify, and session, the one verb that labels a held-out set (`--help` for details)
  aos-learn-bench --lab PATH --out DIR PAIR --tests-log FILE
                                 design §13.9's pre-registered M6 session on PAIR (a real pair, or --practice J; `--help` names both): child A, then child B replaying it, gates G0 to G5, the pair's void or record line, DIR/record.txt
  aos-phase TRACE --build BUILD [--plan PLAN_HEX]
                                 re-derive a windowed aos-repair run's measured phase from its kept trace (`--help` for details)
  resource-attribution [--aterm PATH]
                                 private two-tab delegated resource attribution check
  presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases \"A H B D T X\"] [--trials N] [--keep]
                                 OPENS A WINDOW (with --window): verify zero re-grid flap across fenced/harness turns and chrome pairs (~3 min)
  look-away [--aterm PATH] [--keep]
                                 OPENS A WINDOW (~30 s): its front tab's task is marked done while Calculator, opened and
                                 quit by PID here, is the active app; wants the Done for you row to stand (done=1), still
                                 to stand after `ctl focus in` and another done, to go once aterm is active again, and a
                                 done while aterm is active seen as it lands even after `ctl focus out` (§14.11;
                                 --aterm main's build is the negative control)
  meta-keys --window [--layout ID]... [--aterm PATH] [--keep] | meta-keys --plan [--layout ID]... | meta-keys --restore
                                 OPENS A WINDOW (with --window): verify Option chords and dead keys across U.S., German, and French layouts (~1 min)
  predict-live --window|--headless [--aterm PATH] [--shell zsh|bash]... [--rtt MS]... [--mode on|off]...
               [--rounds N] [--no-split] [--out DIR]
                                 OPENS A WINDOW (with --window): verify predictive editing over a simulated RTT `link` in
                                 single and split panes (defaults: both shells, 100 ms, both modes, 2 rounds)
  codex-alab-proof [--package DIR] [--upgrade-package DIR] [--aterm PATH] [--expect-rollout bound|partial]
                   [--policy-only|--upgrade-only|--raw-upgrade-only]
                                 end-to-end native harness proof on the real codex-alab package against a loopback fake
                                 model (~11 min; --policy-only: the cybersecurity-refusal leg alone; --upgrade-only: one
                                 live handoff; --raw-upgrade-only: started through the produced primary with its managed
                                 entry absent, automatic crash repair, then a live handoff); its scratch world is always
                                 kept; the rollout leg expects what --expect-rollout says, else what the lab's compiled
                                 CODEX_MEASURED_BUILDS says of the package's build; one proof at a time on this Mac (it
                                 waits up to 20 min for another to end, then is NOT RUN)
  codex-measure [--package DIR] [--keep]
                                 measure a codex-alab build for the rollout tap, in a clean checkout (~25 min): the proof
                                 in process expecting it partial, its six legs' rollouts reduced to a fixed point and
                                 written to aterm-agent's step testdata (which lists the build), the standing tests, the
                                 proof again expecting it bound, then a docs/measured note; or a refusal by class (STEM KEY
                                 ROLLOUTS FIXED-POINT NEW-KEY GONE PROOF-RED STANDING NOT-BOUND) that leaves the checkout
                                 as it was and keeps the worlds; exit 0, writing nothing, for a build whose six legs HEAD
                                 holds; NOT RUN for a codex reduction HEAD does not hold (a killed run's leftover)
  claude-measure [--keep]
                                 measure the managed Claude Code build (`claude --version`) for the session log's
                                 transcript steps, in a clean checkout (two debug builds of aterm and the standing
                                 tests' build, ~25 min; ~10 min to a refusal at step 5): one session in a lab of its own
                                 (harness off, the stub's subscriber path: a turn, a Bash call through its box, the
                                 five-hour limit) expecting it partial, its transcript reduced to a fixed point the
                                 privacy oracle passes and written to aterm-agent's step testdata as <build>.jsonl
                                 (which lists the build), the standing tests, the session again expecting it on, then a
                                 docs/measured note; or a refusal by class (STEM KEY TRANSCRIPTS FIXED-POINT TEXT
                                 NEW-KEY GONE PROOF-RED STANDING NOT-BOUND) that leaves the checkout as it was and
                                 keeps the worlds; exit 0, writing nothing, for a build HEAD holds; NOT RUN for a Claude
                                 Code reduction HEAD does not hold
  seamless --baseline PATH [--aterm PATH] [--runs N] [--keep]
                                 OPENS A WINDOW, one at a time: one instance of a debug or dev-seams aterm across a real
                                 seamless update (ATERM_DEBUG_SEAMLESS_REEXEC=1, `update apply`), legs gap, topics,
                                 gaptopic, redial, redial75, latch, hwkey and privacy (the files the update wrote, kept
                                 by ATERM_DEBUG_SEAMLESS_KEEP and swept for planted sentinels); then present: N
                                 (default 4, at least 2) fresh instances each of the tested and the baseline aterm,
                                 alternating, the tested medians judged against the baseline runs (n_present inside
                                 their range, present_p50/p95/p99_ms at most their highest) (~1-2 min, plus ~1 min a
                                 run)
  reduce <transcript|rollout> --program claude|codex [--out FILE]
                                 key-only transcript/rollout reducer for harness step fixtures (verifies reader equivalence before writing)
  ptyrec <log> -- <cmd> [args...]
                                 run <cmd> behind a pass-through PTY and record `<t_ns> I|O <hex>` chunks to <log>
  link [--rtt MS] [--take FILE] -- <cmd> [args...]
                                 run <cmd> behind a PTY delaying chunks by RTT/2 each way (default 100 ms)
  mac dmg [--image REF] [--fda] [--keep] | mac nux [--app PATH] [--scenario ROW,...] [--standard-user]
      [--person] [--no-clt | --vanilla] [--repeat N] [--record [FILE]] | mac clean
                                 the macOS tart guest lane, each run in a fresh clone of a kept SIP-on base: `dmg`, the
                                 dmg_acceptance benches; `nux`, what a person meets on a new Mac (init, doctor, first
                                 launch, terminal and app launch, vault, a vault of the guest's own, successor,
                                 install.sh, and the fault rows: kills, rerun, legacy upgrade, offline, lossy, mDNS,
                                 dmg, agent and SSH, clock) timed in the guest, with a dialog census and the ALab
                                 guard; `clean`, delete the kept bases (`mac help`; tart fetched when absent)
  linux [--gpu] up|down|status|exec|proof|baseline|gone|cpu|a11y|edges|deck|browser|gpu-host|vz-agent|clean|destroy
                                 manage and run the Ubuntu 24.04 Lima VM runtime lane on aarch64 and Rosetta x86_64, or with
                                 --gpu its krunkit instance on this Mac's GPU (`linux help`)
  windows up|down|status|exec|smoke|window|destroy
                                 manage and run the Windows 11 arm64 QEMU VM runtime lane on aarch64 and Prism x86_64 (`windows help`)

Every verb that opens a window (--window, and the Linux lane's GUI rows) first waits for a quiet machine: alone
among quiet claims in the per-user capacity ledger, until the 1-minute load is low or, next in line, its patience
runs out. Once a minute it says what it waits for, when it holds back claimed work that asks after it, and when it
starts whatever the load. `--admit now` (or `--admit=now`), given to any verb, starts it at once, and a lane's VM
boot without waiting for disk.

Exit: 0 pass, 1 a finding or a refusal (also an unknown command and, on many verbs, a bad argument), 2 usage,
3 could not run, 128+N cut short by signal N (INT, TERM, HUP).
";

/// The lines of [`USAGE`] that name `verb`: its entry and the lines that continue it.
fn verb_usage(verb: &str) -> Option<String> {
    let mut out = String::new();
    let mut inside = false;
    for line in USAGE.lines() {
        if let Some(entry) = line
            .strip_prefix("  ")
            .filter(|entry| !entry.starts_with(' '))
        {
            let names = entry.split(' ').next().unwrap_or_default();
            inside = names.split('|').any(|name| name == verb);
        } else if !line.starts_with("   ") {
            inside = false;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A verb with no help of its own that refused `rest` as a usage error answers `-h`/`--help`
/// before any `--` with its lines of [`USAGE`].
fn help_instead(verb: &str, rest: &[String], fail: &Fail) -> Option<String> {
    let asked = rest
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "-h" || arg == "--help");
    if fail.code == 2 && asked {
        verb_usage(verb)
    } else {
        None
    }
}

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
        "claude-login-proof" => claude_login_proof::cmd_claude_login_proof(args),
        "claude-rotation-proof" => claude_rotation_proof::cmd_claude_rotation_proof(args),
        "agy" => agy::cmd_agy(args),
        "agy-selftest" => agy_proof::cmd_agy_selftest(args),
        "agy-captures" => agy_captures::cmd_agy_captures(args),
        "muse" => muse::cmd_muse(args),
        "muse-captures" => muse_captures::cmd_muse_captures(args),
        "autopilot-captures" => autopilot_captures::cmd_autopilot_captures(args),
        "muse-selftest" => muse_proof::cmd_muse_selftest(args),
        "muse-relaunch" => relaunch_muse::cmd_muse_relaunch(args),
        "muse-upgrade" => upgrade_muse::cmd_muse_upgrade(args),
        "agy-upgrade" => upgrade_agy::cmd_agy_upgrade(args),
        "agy-relaunch" => relaunch_agy::cmd_agy_relaunch(args),
        "fleet-sim" => fleet_sim::cmd(args),
        "fleet-menu" => fleet_menu::cmd(args),
        "commons-lan" => commons_lan::cmd(args),
        "commons-startup" => commons_startup::cmd(args),
        "swarm" => swarm::cmd_swarm(args),
        "mode" => verbs::cmd_mode(args),
        "limit" => verbs::cmd_limit(args),
        "tool" => verbs::cmd_tool(args),
        "settings" => verbs::cmd_settings(args),
        "headers" => verbs::cmd_headers(args),
        "headers-for" => verbs::cmd_headers_for(args),
        "mode-for" => verbs::cmd_mode_for(args),
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
        "look-away" => look_away::cmd_look_away(args),
        "crash" => crash::cmd_crash(args),
        "sleep" => sleep::cmd_sleep(args),
        "workers" => workers::cmd_workers(args),
        "footers" => footers::cmd_footers(args),
        "halfdraw" => halfdraw::cmd_halfdraw(args),
        "idle-points" => idlepoints::cmd_idle_points(args),
        "restart-jobs" => restart_jobs::cmd_restart_jobs(args),
        "paths" => verbs::cmd_paths(),
        "instance" => instance::cmd_instance(args),
        "resource-attribution" => resource_attribution::cmd(args),
        "disk-lane" => disk_lane::cmd_disk_lane(args),
        "disk-ask" => disk_ask_lane::cmd_disk_ask(args),
        "slot-capture" => slot_lane::cmd_slot_capture(args),
        "slot-lane" => slot_lane::cmd_slot_lane(args),
        "trail-gallery" => trail_gallery::cmd_trail_gallery(args),
        "__trail-redraw-fixture" => trail_gallery::cmd_redraw_fixture(args),
        "trail-contrast" => trail_contrast::cmd_trail_contrast(args),
        "trail-diff" => trail_diff::cmd_trail_diff(args),
        "trail-census" => trail_census::cmd_trail_census(args),
        "trail-companion" => trail_companion::cmd_trail_companion(args),
        "trail-hue" => trail_hue::cmd_trail_hue(args),
        "trail-keys" => trail_keys::cmd_trail_keys(args),
        "aos-repair" => aos_repair::cmd_aos_repair(args),
        "aos-search" => aos_search::cmd_aos_search(args),
        "aos-workers-bench" => aos_workers_bench::cmd_aos_workers_bench(args),
        "aos-gpu-bench" => aos_gpu_bench::cmd_aos_gpu_bench(args),
        "aos-learn" => aos_learn::cmd_aos_learn(args),
        "aos-learn-bench" => aos_learn_bench::cmd_aos_learn_bench(args),
        "aos-phase" => aos_phase::cmd_aos_phase(args),
        "presence-flap" => presence::cmd_presence_flap(args),
        "meta-keys" => meta_keys::cmd_meta_keys(args),
        "predict-live" => predict_live::cmd_predict_live(args),
        "codex-alab-proof" => codex_alab_proof::cmd_codex_alab_proof(args),
        "codex-measure" => codex_measure::cmd_codex_measure(args),
        "claude-measure" => claude_measure::cmd_claude_measure(args),
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
        Some("dash") if swarm::standin_configured() => {
            swarm::claude_main(args.get(1..).unwrap_or_default())
        }
        Some(name @ ("zsh" | "-zsh" | "bash" | "-bash")) => link::as_shell(name, &args),
        Some("security") => tripwire::main(&args),
        Some("open") => recorder::main(&args),
        Some("claude") => {
            if swarm::standin_configured() {
                swarm::claude_main(&args)
            } else {
                fleet_fixture::claude(&args)
            }
        }
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
                swarm::partition::AGENT_VERB => swarm::partition::agent_main(rest),
                "init" if std::env::var_os(swarm::partition::AGENT_SEAM).is_some() => {
                    swarm::partition::converge_standin(rest).unwrap_or(2)
                }
                "key-dump" => meta_keys::keydump_main(rest),
                "mcp-standin" => codex_rig::mcp_main(rest),
                _ => {
                    sys::forget_env(paths::forgotten);
                    sys::confine_env();
                    let mut rest = rest.to_vec();
                    let r = admission::take(&cmd, &mut rest).and_then(|()| {
                        if matches!(
                            cmd.as_str(),
                            "" | "help"
                                | "-h"
                                | "--help"
                                | "paths"
                                | "protected"
                                | "fleet-sim"
                                | "swarm"
                                | "reduce"
                                | "trail-contrast"
                                | "trail-census"
                                | "trail-companion"
                                | "trail-hue"
                                | "abrew-admission"
                        ) {
                            dispatch(&cmd, &rest)
                        } else {
                            check_state_dir().and_then(|()| dispatch(&cmd, &rest))
                        }
                    });
                    let found = matches!(r, Ok(1));
                    let code = match r {
                        Ok(c) => c,
                        Err(f) => {
                            if let Some(usage) = help_instead(&cmd, &rest, &f) {
                                print!("{usage}");
                                return std::process::ExitCode::SUCCESS;
                            }
                            if let Some(m) = f.msg {
                                eprintln!("aterm-lab: {m}");
                            }
                            f.code
                        }
                    };
                    stop::status(code, found)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verb_with_no_help_of_its_own_answers_help_with_its_usage() {
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let refused = Fail::usage("instance: unexpected '--help'");
        let usage = help_instead("instance", &words(&["--help"]), &refused).expect("its usage");
        assert!(usage.starts_with("  instance [--window]"), "{usage}");
        assert!(usage.contains("[-- PROGRAM ARGS]") && usage.contains("--scene band-icons"));
        assert!(!usage.contains("abrew-admission"), "{usage}");
        assert!(help_instead("instance", &words(&["-h", "--keep"]), &refused).is_some());
        for (rest, fail) in [
            (words(&["--", "/bin/sh", "--help"]), Fail::usage("x")),
            (words(&["--keep"]), Fail::usage("x")),
            (words(&["--help"]), Fail::die("x")),
        ] {
            assert_eq!(help_instead("instance", &rest, &fail), None, "{rest:?}");
        }
        let send = verb_usage("send").expect("turn|send|enter|key");
        assert!(send.starts_with("  turn|send|enter|key"), "{send}");
        assert_eq!(verb_usage("no-such-verb"), None);
    }
}
