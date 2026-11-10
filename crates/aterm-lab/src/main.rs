// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-agent lab: launches the real Claude Code, Codex ([`codex`]), Antigravity CLI (`agy`, [`agy`]; screens in [`agy_captures`]), Muse Code ([`muse`]; proof in [`muse_proof`]), or Grok Build ([`grok`]) TUI by path inside a private headless `aterm` against a local API stub (Anthropic, OpenAI Responses, Gemini, Meta, or xAI), and runs private aterm instances for checking a change. [`USAGE`] lists the verbs.

#![deny(unsafe_code)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(unix)]
mod artifact_serve;

#[cfg(target_os = "macos")]
mod abrew_admission;
#[cfg(target_os = "macos")]
mod admission;
#[cfg(all(target_os = "macos", debug_assertions))]
mod window_custody;
#[cfg(target_os = "macos")]
mod window_guards;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod window_custody {
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die("window-custody needs a debug lab image"))
    }
    pub fn owner(args: &[String]) -> crate::Res {
        cmd(args)
    }
    pub fn app_band(args: &[String]) -> crate::Res {
        cmd(args)
    }
}
#[cfg(target_os = "macos")]
mod agy;
#[cfg(target_os = "macos")]
mod agy_captures;
#[cfg(target_os = "macos")]
mod agy_proof;
#[cfg(target_os = "macos")]
mod allocation_monitor;
#[cfg(target_os = "macos")]
mod aos_cadence;
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
mod autopilot_captures;
#[cfg(target_os = "macos")]
mod autopilot_scenes;
#[cfg(target_os = "macos")]
mod band;
#[cfg(target_os = "macos")]
mod bench_stats;
#[cfg(all(target_os = "macos", debug_assertions))]
mod build_admit;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod build_admit {
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "build-admit needs a debug lab image for its scratch-ledger seam",
        ))
    }
}
#[cfg(all(target_os = "macos", debug_assertions))]
mod reveal_tab;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod reveal_tab {
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "reveal-tab needs a debug lab image for its scratch-registry seams",
        ))
    }
}
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
mod codex_source_proof;
#[cfg(target_os = "macos")]
mod commons_lan;
#[cfg(target_os = "macos")]
mod commons_startup;
#[cfg(target_os = "macos")]
mod composer_turn;
#[cfg(target_os = "macos")]
mod crash;
#[cfg(target_os = "macos")]
mod deck_motion;
#[cfg(target_os = "macos")]
mod disk_ask_lane;
#[cfg(target_os = "macos")]
mod disk_lane;
#[cfg(target_os = "macos")]
mod door;
#[cfg(target_os = "macos")]
mod first_key;
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
#[cfg(all(target_os = "macos", debug_assertions))]
mod service_acceptance;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod service_acceptance {
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "service-acceptance requires a debug lab binary",
        ))
    }
}
#[cfg(target_os = "macos")]
mod fleet_world;
#[cfg(target_os = "macos")]
mod footers;
#[cfg(target_os = "macos")]
mod forever;
#[cfg(target_os = "macos")]
mod git_starts;
#[cfg(target_os = "macos")]
mod grok;
#[cfg(target_os = "macos")]
mod halfdraw;
#[cfg(target_os = "macos")]
mod harness_proof;
#[cfg(target_os = "macos")]
mod harness_proof_judge;
#[cfg(target_os = "macos")]
mod idlepoints;
#[cfg(target_os = "macos")]
mod input_sources;
#[cfg(target_os = "macos")]
mod instance;
#[cfg(target_os = "macos")]
mod jail;
#[cfg(target_os = "macos")]
mod kimi;
#[cfg(target_os = "macos")]
mod lab;
#[cfg(target_os = "macos")]
mod lab_build;
#[cfg(target_os = "macos")]
mod link;
#[cfg(target_os = "macos")]
mod link_notice;
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
#[cfg(all(target_os = "macos", debug_assertions))]
mod local_signins;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod local_signins {
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "local-signins needs a debug lab image for its lapse-fixture seam",
        ))
    }

    pub fn is_standin(_: &str) -> bool {
        false
    }

    pub fn standin_main(_: &str, _: &[String]) -> i32 {
        1
    }

    pub const LOOK_UP_VERB: &str = "local-signins-look-up";
    pub const REAP_VERB: &str = "local-signins-reap";

    pub fn look_up_main(_: &[String]) -> i32 {
        2
    }

    pub fn reap_main(_: &[String]) -> i32 {
        2
    }

    pub const SETUP_TOKEN_VERB: &str = "local-signins-setup-token";

    pub fn setup_token_main(_: &[String]) -> i32 {
        2
    }
}
#[cfg(target_os = "macos")]
mod cpu_observation;
#[cfg(target_os = "macos")]
mod idle_cpu;
#[cfg(all(target_os = "macos", debug_assertions))]
mod installer_handoff;
#[cfg(all(target_os = "macos", debug_assertions))]
mod local_signins_heal;
#[cfg(target_os = "macos")]
mod look_away;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod mac_commons_startup;
#[cfg(target_os = "macos")]
mod mac_nux;
#[cfg(target_os = "macos")]
mod mac_nux_guest;
#[cfg(target_os = "macos")]
mod mac_runtime;
#[cfg(target_os = "macos")]
mod mac_slots;
#[cfg(target_os = "macos")]
mod measure;
#[cfg(target_os = "macos")]
mod mesh_churn;
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
mod occupancy_standin;
#[cfg(target_os = "macos")]
mod occupancy_wake;
#[cfg(target_os = "macos")]
mod outlook_captures;
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
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod installer_handoff {
    pub fn cold_cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die("cold-handoff requires a debug lab image"))
    }
    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "installer-handoff requires a debug lab image",
        ))
    }
}
#[cfg(target_os = "macos")]
mod seamless_carry;
#[cfg(target_os = "macos")]
mod selftest;
#[cfg(all(target_os = "macos", debug_assertions))]
mod setup_token_lab;
#[cfg(all(target_os = "macos", not(debug_assertions)))]
mod setup_token_lab {
    pub const PROBE_VERB: &str = "setup-token-probe";

    pub fn cmd(_: &[String]) -> crate::Res {
        Err(crate::Fail::die(
            "setup-token-lab needs a debug lab image for the vault-token seam",
        ))
    }

    pub fn probe_main(_: &[String]) -> i32 {
        2
    }
}
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
mod system_state_gallery;
#[cfg(target_os = "macos")]
mod system_state_measure;
#[cfg(target_os = "macos")]
mod system_state_sheet;
#[cfg(target_os = "macos")]
mod tab_gallery;
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
mod trail_replay;
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
mod video_sheet;
#[cfg(target_os = "macos")]
mod vm_admit;
#[cfg(target_os = "macos")]
mod windows;
#[cfg(target_os = "macos")]
mod windows_desk;
#[cfg(target_os = "macos")]
mod windows_guest;
#[cfg(target_os = "macos")]
mod windows_settings;
#[cfg(target_os = "macos")]
mod windows_smoke;
#[cfg(target_os = "macos")]
mod windows_uia;
#[cfg(target_os = "macos")]
mod windows_window;
#[cfg(target_os = "macos")]
mod work_overlap;
#[cfg(target_os = "macos")]
mod workers;

/// How a person runs this program, for the hints that tell them what to type next.
pub const LAB: &str = "targo --unverified lab";

/// Poll `pred` every 100 ms until it holds (`true`) or `within` passes (`false`); a hang detector for something that must happen, interruptible.
#[cfg(target_os = "macos")]
#[must_use]
pub fn wait_until(within: std::time::Duration, mut pred: impl FnMut() -> bool) -> bool {
    let end = stop::deadline_after(within);
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
aterm-lab — the real Claude Code, Codex, agy, Muse Code or Grok Build TUI in a jailed private headless aterm
against a local API stub (Anthropic, Responses, Gemini, Meta or xAI). Most labs are fully local; artifact-serve is an explicit peer-bound LAN transfer.

usage: targo --unverified lab <command> [tag] [args]
       (tag: the first word when it names a lab that is up, else $LAB_TAG or \"lab\" and the word is an argument;
       up takes one optional tag beside --window, --fabric and --tab-strip; down, ps, crash, relaunch and sleep
       take any first word as the tag;
       a verb that brings up its own lab defaults to a tag of its own)
  artifact-serve --bind LAN_IP:PORT --peer IP --status NEW_PATH --file NAME=ABSOLUTE_PATH...
                                 serve immutable named snapshots to one peer using a capability stored only in the private status file
                                 bounded by --lifetime-secs (default 900, maximum 3600) and --max-requests (default 32, maximum 128)
  up [tag] [--window] [--fabric] [--tab-strip]
                                 start the stub, a black-hole proxy, the egress witness and a jailed private aterm
                                 (headless by default; --window opens one real window in the same jail), and name the
                                 build that instance runs (<version>+g<commit>, as its `ctl version` and harness tapes do)
                                 --fabric: the lab's own bus: a private broker in the jail and the lab's socket dir, node and
                                 observer caps (<run>/fabric), the [fabric] table and the rendezvous file `aterm link` reads,
                                 the bridge connected before up returns; `down` ends it (refused beside a LAB_ATERM_TOML [fabric])
                                 --tab-strip: tab_strip_rows = 1, the row whose tab menu holds the session clause (`open
                                 tab-menu`); it takes a grid row (refused beside a LAB_ATERM_TOML tab_strip_rows)
                                 (env: LAB_SURVEY=1 LAB_TRUST=1 LAB_FIRST_RUN=1 LAB_RESTORE=1 LAB_CLOCK=1
                                 LAB_HARNESS_KEYS='continue = false' LAB_ATERM_TOML LAB_COLS LAB_LINES LAB_SCALE
                                 LAB_PROXY=dead LAB_CLAUDE LAB_CODEX LAB_AGY LAB_MUSE LAB_GROK LAB_KIMI
                                 LAB_ATERM=/physical/path LAB_STATE=/abs/outside/homes
                                 LAB_INSTANCE_ENV='TMPDIR=/abs ATERM_DEBUG_<NAME>=V ATERM_DEV_<NAME>=V ATERM_OCCUPANCY_NUDGE_S=N'
                                 LAB_HOME_IMAGE=<n>g|<n>m: the scratch home on a sparse APFS image of that size (1 GiB to 64 GiB)
                                 LAB_REROUTE=1: the lab's Claude Code store build cloned into the jail's own atpkg
                                 prefix with its twin and reroute stub, the instance rerouting; `claude --by-name`
                                 LAB_JAIL_SCAN=1: the instance's disk scans read the jail's processes alone (a debug
                                 build's ATERM_DEBUG_JAIL_MARK), so `aterm harness disk free` can free in the lab)
                                 LAB_ROUTE_SEAM=1: the instance reads the stub's base URL as Claude Code's default
                                 route (ATERM_DEBUG_LAB_DEFAULT_ROUTE), so a tab launched on a roster row's stored
                                 login (no --subscriber) is that row's process to the accounts host
  claude [tag] [-e K=V]... [--no-wait] [--prepare-only] [--relay] [--subscriber | --stored | --own] [--debug-log] [--by-name] [-- claude args]
                                 launch the managed Claude Code in tab 1 (--relay: behind the PTY relay; --subscriber: a
                                 fake claude.ai OAuth token, for `limit` headers; --stored: the stored login a /login
                                 leaves, in a login directory of its own, so a relaunch can admit its account;
                                 --own: that login in the scratch HOME itself (no CLAUDE_CONFIG_DIR) with aterm's usage
                                 tap on the line and, where none is there, a roster naming it `main`: the default home's
                                 own account whose tap row line E claims (--subscriber's outside token, --stored's own
                                 home and a Mac with no roster show none); --stored and --own: LAB_ROUTE_SEAM=1 labs
                                 only; --debug-log: a scratch debug log;
                                 --by-name: types `claude` at the prompt as a person does, the lab's environment as
                                 assignment words before it; LAB_REROUTE=1 labs only)
                                 --prepare-only: write the canonical launcher and emit JSON argv/cwd for `instance --`;
                                 no typing or saved socket/PID/agent changes. Keep the headless preparation lab alive;
                                 native input uses the native instance's own socket. No relay/subscriber/stored/own/by-name;
                                 private HOME/XDG/TMP/provider values cannot be overridden with -e.
  codex [tag] [--signed-out|--chatgpt[=PLAN]] [-e K=V]... [--no-wait] [--prepare-only] [-- codex args]
                                 launch the managed Codex in tab 1 (CODEX_HOME=<scratch>/codex-home, model
                                 $LAB_CODEX_MODEL when set; --chatgpt: a fake ChatGPT auth.json, plan plus by default,
                                 and stub refresh; --signed-out: the default provider, no auth; either one also sets
                                 [harness] enabled = false in the lab's aterm.toml)
                                 --prepare-only: write canonical scratch config/auth and launcher, emit JSON argv/cwd
                                 for `instance --`; no typing, harness edit or saved socket/PID/agent/login changes.
                                 Keep the headless preparation lab alive. Native input uses the native instance's
                                 own socket; private HOME/CODEX_HOME/XDG/TMP/provider values cannot be overridden.
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
  grok [tag] [-e K=V]... [--no-wait] [-- grok args]
                                 launch Grok Build (LAB_GROK at up) in tab 1 with the lab's XAI_API_KEY, both of its
                                 base URLs at the stub, a scratch HOME and GROK_HOME and BROWSER=/usr/bin/false, once
                                 the launcher has seen the jail deny /usr/bin/open
  kimi [tag] [-e K=V]... [--no-wait] [-- kimi args]
                                 launch Kimi Code in tab 1 with a scratch HOME and KIMI_CODE_HOME whose config.toml names
                                 one openai-type provider at the stub's Chat Completions routes and the lab's fake key
  outlook-captures --out DIR [--aterm ATERM] [--tag NAME] [--scenes ABCDEFGHIJK] [--look day|night|both] [--scale 1|2|both]
                                 the accounts outlook's footer at three widths and Accounts card at 60 and 44 cells, one
                                 headless instance per scene, look and scale, seeded with synthetic identities only; I, J
                                 and K: line E's quota row over the fixture's usage-tap statusLine, under Cruise; the card
                                 of A, B, C and J (case (c), Cruise: hand-offs, gap and out lines beside the ticks)
  outlook-perf --main ATERM --out DIR [--aterm ATERM] [--pairs N]
                                 the outlook against another build on scene A: paired typing and idle takes, and the
                                 resolver's own read and replay cost
  autopilot-captures --harness all|claude|agy|muse|codex-alab [--scale 1|2] [--look day|night|both]
                                 Autopilot's scenes on every harness, one headless instance each per look, launched in it
                                 (the forever lab's chains; codex-alab-proof's legs): an image per scene, the chip's whole
                                 hovered title at 400 columns, chrome and status beside each png, manifest.tsv; a paired
                                 agy typing take (--baseline ATERM, alone without --harness; --typing codex-alab: on
                                 codex-alab, each arm then idle 20 s) (~40 min a look and scale; runs side by side
                                 share one folder)
  turn|send|enter|key [tag] ...  type prompt+Enter | text | Enter | named key (0, escape, ctrl+c, ...); turn types
                                 only into an empty composer, and exits 1 unless the composer accepted the prompt
  mode [tag] [name ['<spec>']]   show or set the stub mode: ok long numbered overloaded too-long compact-fails slow
                                 rate-limit 401 500 bad-key bg[:SLEEP[:SLOW]] tool limit:CLAIM:RESET[:PLAN],
                                 limit-after:N:CLAIM:AHEAD_MS[:MODE] (global only), or a
                                 custom name whose '<spec>' JSON object is written to modes/<name>.json (bg: a prompt
                                 with `bgstart` runs `sleep SLEEP` in the background, any other streams SLOW s of ticks)
                                 limit-after admits N main requests in MODE (default ok), installing a usage wall
                                 before the last admitted reply; side/metadata spend none, reset measured at admission
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
  debuglog [tag] [regex]         print lines from `claude TAG --debug-log`, then from each forever tab's own log
                                 (`[<chain>] `-led; default: request/account/telemetry lines)
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
  trail-replay [tag] --out DIR --text ASCII --cadence-ms N [--no-images]
               [--expect-trail-landing ROW,COL]
                                 attach an existing isolated lab and record 1..4096 printable ASCII edit keys without Enter
                                 or paste; explicit cadence 1..60000 ms, no catch-up; DIR must be new with an existing parent.
                                 Buffers replies/admissions during typing, then reads metrics before effect/clean images;
                                 --no-images skips both captures/files and retains metrics/drains/status/cursor/text/dims.
                                 --expect-trail-landing observes a fresh source-bound admission at the explicit 0-based
                                 row,column plus a matching closed-frame cursor before metrics, with a 60-second hang
                                 detector. Licensed or declined verdicts are retained with separate observation timing;
                                 separate reads establish observed agreement, not an atomic snapshot or glyph delivery.
                                 exit 0 means recording complete, 1 inconclusive, never a pixel-continuity or latency verdict.
  first-key [--aterm PATH] [--arm NAME=REST:PRESSES[:burst]]... [--arm-toml NAME=LINE]...
            [--min-quiet SECS] [--settle LO..HI] [--no-pet] [--toml LINE]... [--seed N] --out DIR
            | first-key summarize PRESSES.TSV...
                                 a window's first key after each quiet, arm against arm, press by press (a window's
                                 rest and each arm's config lines): per-press rows, each press's replies whole, and
                                 nearest-rank, bootstrap and exact-test summaries (ATERM-KI-1027-4)
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
                                 (~32 min): a wall moves the tab with its conversation (R1), no usable row waits for the
                                 first to open and moves back (R3), a row whose login directory is gone gets an automatic
                                 sign-in attempt (recorded no-route) while the tab waits on its own reset and resumes
                                 (R4), a sign-in on the default login makes its row usable and displaces the one it held,
                                 there and back (R5a, R5b), the usage tap's high figure moves it before any wall (R2), a
                                 wall during the tab's own background job moves it only once the job ran to its end
                                 (R6); nobody asked, no /login, no browser, no egress; a verdict table, exit 1 on FAIL
                                 (--no-route-seam: the live negative control, R1 must FAIL and its control row PASS)
  nudge-trial [tag] [--mode on|log|off] [--trials N] [--expect on|log|off] [--via mode|key] [--no-hold]
                                 force N (1-3) PTY displacements on real Claude Code and verify the repaint nudge (~2.5 min)
  restore [tag] quit|crash [--keep] [--hold] [--clear] [--land|--land-open]
                                 cold-restore proof on a lab of its own under LAB_RESTORE=1 (~1 min): Claude Code takes
                                 a turn, aterm quits or is SIGKILLed, and the relaunched instance must resume the
                                 conversation with the expected Autopilot carry-on (default Cruise `step=continued`;
                                 --hold, crash only: the held tab stays held until lifted; --clear: `/clear` settles
                                 `step=done:taskless`; --land: the landed tab stays `task=landed` with
                                 `step=done:said-done`; --land-open: mid-work Land settles `step=continued`, the
                                 relaunch's facts typed once with no done check)
  forever --harness <claude|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-redraw|wall-switch|wall-unread|wall-cruise|wall-relaunch|wall-tap|wall-tap-narrow|resid|ladder|angles|switch|outage-land|outage-cruise|sysmsg|dialog] [--chains a,b] [--no-surfaces] [--window] [--untapped] [--token-login|--forced-login] [--no-ready-gate] [tag] [--keep]
                                 supervised Autopilot Land and Cruise scenario chains against real TUIs in scratch git
                                 repos (~15-30 min/harness):
                                 - land / cruise / both: L1-L11, L16-L19, C1-C2, HL/HC (same-land, same-cruise), P4,
                                   N1-N4, Off control, plus Settings > Harness window & 20 s idle wake/render checks
                                   (--no-surfaces skips window checks); a chain aterm stops typing into is watched 60 s
                                   past its hold, brought in front twice, and must say `stopped typing` (the
                                   chip or hover, the state line, `status held=`, the journal's `held=`), with
                                   no Messages row; Cruise holds no repeated reply: HC wants each act paced on
                                   the ladder, then a request for a different approach, never a stop (~20 min)
                                 - cover: L12 prose question, L13 person turn (task=theirs), L14/C3 P2 reads, P5, L15
                                   worker, C4, Off control
                                 - cap: continue_per_hour = 1 escalation and post-hour recovery (~72 min)
                                 - wall / wall-hid: usage walls (W1 visible, W2 scrolled away)
                                 - wall-redraw / wall-switch / wall-unread: W4 the wall redrawn on a typed resume, W5 another
                                   agent in the tab under the wall, W6 an unread limit told an hour past its reset (~30 min
                                   each; wall-unread ~90 min)
                                 - wall-cruise / wall-relaunch: W8 Cruise at a wall, W9 a relaunch under a wall
                                 - wall-tap: W10 (Claude Code only) a five-hour wall the usage tap times: the
                                   window's reset 2 min past the notice's, Esc on the countdown, a new loop
                                   started in the wait by a live [harness] change; the resume at the tap's reset
                                   plus the grace and the session's share, `tap_unix=` on wall-wait@v1 (~15 min)
                                 - wall-tap-narrow: W11 (Claude Code only) the same wall narrowed to 16 columns, where
                                   the countdown's words break: the wait decided there timed by the tap (~10 min)
                                   (--untapped, either: the live negative control, no tap on the line, it must FAIL)
                                 - outage-land / outage-cruise: O1 an API overload from Land's first prompt, O2 one met
                                   by Cruise (Claude Code only): tries on the outage's rungs alone (15 s doubling, the
                                   ladders at an eighth), `status waiting=overload`, one resume after the lift, then
                                   work: Land lands, Cruise goes on (~5 min each)
                                 - sysmsg: a system message is a wait (12a), each form the stub plays (OVL overload, 5XX
                                   server error, 429 rate limit, NET a dropped host) on every harness, met by Cruise and
                                   cleared 90 s on: nothing typed while it stands, `status waiting=<word>`, one resume
                                   in the cleared line, then Cruise goes on (~7 min each; agy's 429 out of reach);
                                   each also under Land (sys-land-overload, -outage, -rate, -drop: none of Land's acts
                                   inside the wall, then Land lands); sys-login and sys-land-login an expired login
                                   (Claude Code on a stored fake subscription login, the environment's token with
                                   --token-login: /login once, its picker left to the person, the browser never run on
                                   aterm's key, one quiet need; then the person signs in and the tab goes on once;
                                   --forced-login: the stored login under settings that pin its method, where /login
                                   opens the browser on its Enter: no /login of aterm's, one quiet need naming the pin)
                                 - dialog: L20 a question dialog under Land answered by the land text
                                   (`answer-land@v1`, never handed over, landed), C5 the same under Cruise answered by
                                   its Recommended option, Off control (Claude Code only)
                                 - resid / ladder: residual and backoff-ladder scenario slices
                                 - angles: cruise-angles, each done claim after real work asked another of the six kinds
                                   of work in turn, never held past the sixth; the back-off ladder at an eighth (~30 min)
                                 - switch: a running Cruise tab switched to Land (`meta set autopilot land`): at rest in its
                                   back-off (land@v1 within 5 s, before the due), under a standing draft (nothing typed until
                                   it clears and the hold for the person who typed it ends, then land@v1 within 5 s),
                                   flipped to Land then Off within 300 ms (nothing typed), its reply repeated (the
                                   person's switch to Land asks once, Land holds at the same answer), and 5 s into a
                                   long busy turn (noted in the turn, nothing mid-turn, land@v1 within 3 s of its
                                   point, no back-off before it);
                                   land, cruise and both also want aterm's own share of land@v1 within 10 s of the first
                                   point, the program's hold of the text said apart, land@v1 never twice in a row, and one
                                   held-text NOTE where the first look past its write saw nothing move (~10 min)
                                 - --no-ready-gate: every tab launched at once and prompted with no look that its program
                                   has finished starting (the lab before slice at1), so Land writes behind unread input
                                 - each Claude Code tab writes its own debug log (claude-debug-<chain>.log), and a start
                                   the ready gate watches whose input stands unread 1 s is captured once (process tree,
                                   lsof, a 2 s sample) into the run's input-holds/, which `down` keeps (none under
                                   --no-ready-gate); LAB_HOLD_CAPTURE_MS=<ms> moves the threshold (0: every start, once
                                   it is ready)
                                 - --window (needs the land-p or same-land chain; refused under LAB_RESTORE=1): opens a real
                                   window, verifies L13's native controller draft via NSEvent `hwkey` with unchanged
                                   `human_seq=` (person takeover stays UNOBSERVED), and reads same-land's hold off it
  crash [tag] [--gap SECS] [--keep-tree]
                                 SIGKILL the lab's own aterm instance and relaunch it on the same state after SECS (default
                                 0; --keep-tree leaves its child processes, for PTY keeper testing; refused on a windowed lab
                                 whose recorded pid may be its window's keeper, so crash an `up --window` lab's)
  relaunch [tag]                 start the lab's instance again on the same state and socket once it ended by itself
                                 (`ctl invoke Quit`): refused while it still runs; an `up --fabric` lab's relaunch
                                 waits for its bridge to connect again, so its bus reads a clean quit's relaunch
  sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]
                                 emulate macOS sleep on a LAB_CLOCK=1 lab: SIGSTOP aterm, advance clock offset by SECS,
                                 hold --hold SECS, and SIGCONT (--at-act waits for first CONTINUED journal act)
  muse-relaunch [tag] [--keep]   live Muse Code crash/resume/aterm-restart lane (~4 min)
  muse-upgrade [tag] [--keep]    live Muse Code quiet upgrade lane across idle, background shell, and mid-turn (~9 min)
  agy-upgrade [tag] [--keep]     live agy quiet upgrade lane across idle, background/subagent/daemon tasks, and mid-turn (~12 min)
  agy-relaunch [tag] [--keep]    live agy crash/resume/Ctrl-Z/quit/aterm-restart lane (~10 min)
  workers [tag] [--keep] [--early] [--upgrade <newer-claude-bin>]
                                 manager Claude Code spawns 3 real Claude Code workers, awaits all, and verifies results (~20 s)
  work-overlap [tag] [--keep]    two real Claude Code workers change one function of one file in two worktrees of a
                                 scratch repository: the second is told the overlap with the first's sid, the first
                                 lands and its work= row clears (~2 min)
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
           [--reroute-prefix DIR] [--pre VERB]... [--image PNG] [--post VERB]... [--pages cold] [--network-away-in-ms N] [--keep | --world DIR] [-- PROGRAM ARGS]
                                 run a private aterm in a scratch HOME/XDG world, print its socket and build and hold it until
                                 Ctrl-C or SIGTERM (--window: a real window; --band: app (the default) runs it as its own
                                 launchd job, in a coalition of its own as aterm.app is, a window under its keeper there, and agent
                                 as this command's child, in its coalition; --image PNG: capture one settled frame, run
                                 --post, and end; --pages cold: run a copy whose pages are evicted, to measure cold page-ins;
                                 --pre and --post run in order; a failed control ends the run with an error
                                 and later controls are not run (a failed --pre captures no image);
                                 --reroute-prefix DIR: DIR, new, is the HOME, its atpkg prefix holding a clone of the
                                 managed Claude Code's store build with its twin and reroute stub, and the instance reroutes;
                                 --world DIR: run in the lab world at DIR, laid out there when DIR is new (a short path under
                                 /tmp/.ubd) and kept, so a later --world DIR starts on what this one left, its state and its
                                 aterm.toml among it, and takes no --toml, --toml-after, --harness or --harness-key)
                                 the signal ends the instance, waits for its end and says whether it came (a second signal cuts
                                 that wait short); the world is removed only once the instance has ended
                                 --scene band-icons draws procedural chrome icons beside H (debug/dev-seams build)
                                 --scene harness-layouts draws labeled synthetic footer states through the real renderer
                                 (debug/dev-seams build; no live agent, upgrade candidate or account is read)
                                 --network-away-in-ms N seeds a fresh private world's remembered home and scripted remote
                                 route; its away entry becomes eligible in N ms (1..120000, debug Lab and checkout's default debug app
                                 only, no --aterm or LAB_ATERM fallback; no LAN proof)
  instance compare --before LAB --after LAB --aterm PATH --output DIR [--samples N]
                                 compare capture validity and runner wall time against another lab build, in private worlds
  abrew-admission [--json] [--indexes DIR] [--this-machine | --arch A --macos V --xcode V|none --no-clt]
                                 evaluate abrew admission of Homebrew's formula.json and cask.json (fetched from
                                 formulae.brew.sh unless --indexes DIR holds them) on the newest Mac, that Mac with the
                                 named facts replaced, or this one (--this-machine)
  reveal-tab [--json] [--ax-helper HELPER] [--plain-publication] [ATERM]
                                 one-mesh A17 on a private window: synthetic logins sealed in a scratch Commons;
                                 `aterm db emails --show-passwords` prints outside aterm and, in a tab, opens a
                                 read-only Editor tab that `text|screen|image|window|video|key|doc read` refuse,
                                 leaving no value in the tab's text, blocks or log, and the window's accessibility
                                 tree carries the grid and every login and no value (read here, or with --ax-helper
                                 by the guest helper's ax-texts, as `mac nux --scenario reveal-tab` runs it in a
                                 guest); --plain-publication: the GUI's negative control, under which the tree's
                                 no-value check must fail (debug lab image)
  commons-lan prepare-recipient|prepare-source|join|pin|home-lan|run|node|pair|follow|stop|db|secrets|members|declare|write|read|emails-* …
                                 prepare preserved private profiles on separate LAN hosts; synthetic prior vault approval,
                                 ordinary executable startup, exact generic table readback, no installed state changes
                                 pair: run the owner and a local follower under one capacity reservation
                                 node: the headless node service in a profile, its LAN use charged to the caller, with
                                 its fleet key (`aterm fleet init`), a served vault's server, and network.jsonl
                                 prepare-source --serve <ip:port>: the lab vault served at a LAN URL; --port: the lab
                                 fleet port (7190; never 7100); pin: a Mac nobody approved waits at that vault;
                                 home-lan: the lab's home LAN on a member, whose desk then lets a Mac on it in by its
                                 proof (debug image); members: `aterm members join --at <ip:port>...`, no phrase
                                 emails-fixture|emails-set|emails-wait: the owner's emails table across the
                                 profiles (seeded fleet_db replica and flag-only sheet, stdin-only edits,
                                 keyed-digest waits); no value crosses Macs or reaches a report
                                 emails-reveal: A17 on that table in a windowed instance of the profile (debug image)
                                 db: run the shipped proposal/data CLI in that profile, online or offline
                                 secrets: an admin's approval of a key, a join of a lab profile's vault file or of the
                                 lab URL it follows, status
                                 a world on the live default ALab vault (one an earlier run left admitted there) opens for no
                                 verb but behind --live-alab-vault-keeps-member: no run takes that member out again
  commons-startup <aterm> <new-output-directory> [--log-backed] [--suppressed | --observe SECONDS | --peer-cycle [--peer-details] | --local-follower]
                                 start an ordinary window and node host on private custody and loopback Commons;
                                 require enrollment, fresh signed table data in both directions and concurrent offline values after restart
                                 (--suppressed: negative control, whose private socket prevents window runtime startup;
                                 --observe: keep the successful window running 1-60 s, then save its frame and live state;
                                 --peer-cycle: stop the source, restart the observer with that peer absent, then reconnect; saves retained-history/account, reachability and Overview evidence;
                                 --peer-details: requires --peer-cycle and a bundled app; adds selected-peer images after one owned-bundle reopen with identity/focus checks;
                                 --local-follower: adds a fresh window on the source vault with a separate configuration; checks its live owner, exact table inventory and native Network State)
  fleet-menu <aterm> <new-output-directory>
                                 run an isolated reader window and a peer with a sibling on loopback; retain the
                                 peer's two-instance survey, the reader's menu dump and frame (all private worlds kept)
  local-signins [--aterm PATH] [--n N] [--hold S] [--keep] [--out FILE]
                                 one-mesh E4: two jailed private aterms on loopback, each with claude, codex, grok and
                                 agy logins signed in, out and expired; judge that survey, report, upkeep and fleet
                                 survey read each state from local files alone, contact no provider, open nothing,
                                 sign nothing in, and carry only their own Mac's rows (debug lab image)
  setup-token-lab [a|b|c|all] [tag] [--keep]
                                 DESIGN-claude-setup-token S9's gate (debug lab image, no real sign-in): (a) the real
                                 `claude setup-token` on the capture's terminal in the jail reaches an authorize URL the
                                 capture admits and its paste prompt, stopped there; (b) `accounts token` on a stand-in
                                 stores the stub's sentinel token and `token-run` serves it, with no copy in scrollback,
                                 casts, the lab's files, $TMPDIR or any process's argv and environment (each scanner
                                 with a planted control); (c) the real Claude Code on the wrapper's descriptor: its
                                 SessionStart hook, MCP server and statusLine command find no token (control: the
                                 wrapper's spare socket, which they find); exit 1 on FAIL, 3 when a lane could not run
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
  swarm partition [--one-url] [--writers both|a] [--minter-away] [--hold SECS] [--aterm PATH] [--keep] [--record] [--label WORD]
                                 split seven private headless aterms and a vault-only Mac on loopback into two islands
                                 (the ATERM_DEBUG_PARTITION cut), publish and admit on each side, heal, and report what
                                 the vault and the fleet did, with timings and what was lost; every fleet Mac surveys,
                                 drives every other and is read for whom its heartbeat reached (--one-url: F8's negative
                                 control; --minter-away: a1's instance stops before the heal; --record: write
                                 docs/measured/, its name carrying --label's WORD)
  swarm e2e [--aterm PATH] [--keep]
                                 one-mesh E2's membership checks no other lane stages: a drive by a Mac's roster
                                 label and a fleet survey while a writer holds the vault's state lock (N1, N2), and
                                 founders an admin approved by code (seatless, S1), one seated by its own proof under
                                 its own label and one by a person's Allow, then a rotation that carries both and drops
                                 the third, which nothing seated
  mesh churn [--n N] [--seed S] [--minutes M] [--whole-wraps] [--negative-control NAME] [--aterm PATH] [--keep] [--record] [--label WORD]
                                 found a membership log on N private Macs (default 30) on loopback, each its own vault
                                 server and subscriber on the run's genesis; let each in by Allow and the phrase, stop
                                 two for M minutes, remove two at once, restart two, split two islands (a remover killed
                                 mid-batch on one; a write, a newcomer and a rebuild on the other), heal, and judge the
                                 frontier, newest key, Wrap bound, removed seats and the per-seat view, every time a
                                 measurement (--whole-wraps: island A takes every Wrap of the remover's [everyone] epoch,
                                 so the newcomer's gap heals at once; --negative-control old-gap-rule|unsettled-ok, or
                                 frontier-blind with --whole-wraps: a run that holds only when its check fails;
                                 --record: write docs/measured/, its name carrying --label's WORD)
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
  build-admit --targo PATH [--keep] [--sample-after SECONDS (0..59)]
                                 use this debug lab as helper to observe two real Trust builds holding separate memory claims in one scratch ledger,
                                 then release each and verify its claim leaves; does not measure CPU sharing or admission waits
                                 --sample-after samples each owned front still awaiting its barrier once, retaining scratch evidence
  tab-gallery [--aterm PATH] [--themes NAME,...|all] [--quick] <out_dir>
                                 real windowed frames of the tab strip per theme, one sheet each, and the contrast of every app tile measured from their pixels (`--help` for details)
  trail-gallery [--aterm PATH] [--theme dark|light|both] [--scenario NAME[,NAME]]... [--extra 'key = value']...
                [--columns N] [--lines N] [--font-px N] [--no-clean] [--keep] <out_dir> [style ...]
                                 render cursor trail styles across themes/scenarios into <out_dir> with sidecars (`--help` for details)
  deck-motion [--aterm PATH] [--secs N] [--repeat N] [--panel-hz N] [--json-out FILE] [--keep] <out_dir> [take ...]
                                 measure the stats deck in motion: headless takes (idle load swing nodeck sill) recorded,
                                 each frame sorted by what it changed and the needle's angle read from it (`--help` for details)
  trail-contrast <png> [--find TEXT]... [--cells ROW:COL[-COL]]... [--row N]... [--newest N] [--min X]
                 [--newest-min X] [--caret-min X] [--dims FILE|--origin X,Y --cell WxH] [--no-clean] ...
                                 compute WCAG contrast JSON from trail-gallery frames and sidecars (`--help` for details)
  video-sheet INDEX --dims FILE --rows A:B --cols C:D --before-us US --out NEWDIR [--expect-text TEXT]
                                 offline native-pixel atlas of every indexed frame; coverage report, no visual verdict
  system-state-gallery [--aterm PATH] [--fixtures NAME,...] [--quick] [--looks LOOK,...] [--sizes SIZE,...]
                       [--at MS,...] [--reduce-motion] [--keep] <out_dir> | --measure <out_dir>
                                 photograph System State's fixtures across looks, sizes and motion times, one headless
                                 instance at a time, into contact sheets, and judge each frame from the page's own
                                 layout: text >= 4.5:1, glyph >= 3:1, card >= 1.2:1, no clipped text, a 4 pt rhythm
                                 (`--help` for details)
  allocation-monitor --aterm PATH --targo PATH --sdk-bin ABS --out DIR [--look dark|light] [--keep]
                                 one attachment observes actual two-to-one fenced Targo builds in a private Allocation
                                 window; preserve both frames/audits, close the panel before peer release, run both outputs
  window-custody --aterm ABS --out ABS [--band agent|app]
                                 actual GUI retains its original lane/memory after keeper and owner crash; debug lab
  window-handoff-app --aterm ABS --out ABS [--cut-keeper]
                                 authenticated launchd handoff and normal Quit or keeper-lifeline closure; debug lab
  installer-handoff --aterm ABS --out ABS [--handoffs N] [--stream] [--program-workers]
                                 live same-image handoff with an inert receipt-bound installer terminal; debug lab
                                 --stream checks every numbered output record across the handoffs while output stays active
                                 --program-workers checks successful and failed task completion after adoption, tab status off
  cold-handoff --aterm ABS --out ABS [--handoffs N]
                                 verify repeated in-place updates of a private native window with no terminal sessions
                                 both default to 2 handoffs; N is 1..32, with one-minute progress hang detectors
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
  aos-cadence --a PATH --b PATH [--no-attach] [--silent] [--keep]
                                 OPENS A WINDOW, four in turn: typing latency to the glass with a recording holding each key for its echo, builds A B B A (~3 min plus each window's wait for a quiet Mac; `--help` for details)
  aos-search [--grid] [--plans FILE] --out DIR [--toolchain LINE] | aos-search --verify FILE
                                 run Aos M4's candidate search under the pinned policy, or verify a bundle FILE offline (`--help` for details)
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
  idle-cpu --output FILE [--aterm PATH] [--duration-ms N] [--interval-ms N] [--settle-ms N] [--band app|agent]
                                 all observed threads in one private headless idle instance, no video or status polling;
                                 exact deltas refuse missing/reset/late counters, native cleanup and boundary metrics
  recording-cpu --pid PID --aterm PATH --sock PATH --output PATH --duration-ms N --interval-ms N
                                 read-only named-thread CPU and sampled temporal queue observations; no verdict
  git-starts [--minutes N] [--every-ms M]
                                 read-only, on this Mac as it is: list this uid's processes every M ms (100) for N
                                 minutes (10) and each local aterm's survey every 1 s; print process starts and git
                                 starts a minute, agent sessions' turn ends, and what a work record read with git at
                                 turn ends (at most once per 20 s per session) would add
  git-starts --read [--reads K]
                                 read each local agent session's work record K times (5) under the repository probe's
                                 rules; print the git starts and the wall time per read (names no folder)
  presence-flap --window|--headless [--aterm PATH] [--client PATH] [--phases \"A H B D T\"] [--trials N] [--keep]
                                 OPENS A WINDOW (with --window): verify zero re-grid flap across fenced/harness turns and a lease (~2 min)
  look-away [--aterm PATH] [--keep]
                                 OPENS A WINDOW (~30 s): its front tab's task is marked done while Calculator, opened and
                                 quit by PID here, is the active app; wants the Done for you row to stand (done=1), still
                                 to stand after `ctl focus in` and another done, to go once aterm is active again, and a
                                 done while aterm is active seen as it lands even after `ctl focus out` (§14.11;
                                 --aterm main's build is the negative control)
  meta-keys --window [--layout ID]... [--aterm PATH] [--keep] | meta-keys --plan [--layout ID]... | meta-keys --restore
                                 OPENS A WINDOW (with --window): verify Option chords and dead keys across U.S., German, and French layouts (~1 min)
  meta-keys --current-control     read the selected layout's first composition control as JSON; changes no input sources
  predict-live --window|--headless [--aterm PATH] [--shell zsh|bash]... [--rtt MS]... [--mode on|off]...
               [--rounds N] [--no-split] [--out DIR]
                                 OPENS A WINDOW (with --window): verify predictive editing over a simulated RTT `link` in
                                 single and split panes (defaults: both shells, 100 ms, both modes, 2 rounds)
  codex-alab-proof [--package DIR] [--upgrade-package DIR] [--aterm PATH] [--expect-rollout bound|partial]
                   [--policy-only|--terminals-only|--facts-only|--upgrade-only|--raw-upgrade-only|--window-only
                    |--capacity-only]
                                 end-to-end native harness proof on the real codex-alab package against a loopback fake
                                 model (~11 min; --policy-only: the cybersecurity-refusal leg alone; --terminals-only:
                                 the background terminals leg alone, about 1 min; --facts-only: the facts leg alone
                                 (a stalled call's no-progress fact riding one continuation), about 7 min, not yet in
                                 the default run; --upgrade-only: one live handoff;
                                 --raw-upgrade-only: started through the produced primary with its managed entry
                                 absent, automatic crash repair, then a live handoff; --window-only: the window
                                 pass, four tabs' chip lines, pads, state lines, hovers, This Mac and menu rows, judged by
                                 what the package's build tells, then END's idle window with Settings ▸ Harness open and
                                 shut; the default run leaves it out; --capacity-only: the full proof's last leg alone,
                                 the package's `codex exec` recovering from a 503 with Retry-After and from a 429
                                 account quota (its reset, or a 30 s poll without one) and refusing a 403 and the
                                 four terminal 429 types, in a jail of its own against the lab's loopback model,
                                 ~1 min, no aterm; one lane a run); its scratch world is kept once its bus stands; the rollout leg expects what
                                 --expect-rollout says, else what the lab's compiled CODEX_MEASURED_BUILDS says of the
                                 package's build; one proof at a time on this Mac (it
                                 waits up to 20 min for another to end, then is NOT RUN); an --aterm older than the
                                 lifeline every lab launch is armed with (its broker refuses --lifeline-fd) is NOT RUN
  codex-boxes [--runs N] [--package DIR] [--aterm PATH] [--keep]
                                 how many of a codex-alab's approval boxes its tab's session log holds as claimed steps
                                 (GAP 26), on codex-alab-proof's rig: each run (default 3, at most 9) a tab whose model
                                 asks for 20 escalated commands, each forced to codex's box by a `ctl hold` the lab lifts
                                 once `status` shows it; reports the boxes forced against the claim steps opened (the
                                 undercount) and their closes' out= and by=, never failing on those; FAIL on a stalled
                                 run (no proven claim within 180 s, a hold that did not go on or lift, a box or the done
                                 line past its one-minute hang detector, or a log that did not read), a blocked mark
                                 with no claim step, or a step left open (the wait for the last close a one-minute hang
                                 detector too)
  harness-proof <id> --repo DIR [--aterm PATH] [--target-dir DIR.noindex]
                                 the harness proof (design harness-repos §9 step 2) of a rostered harness built from its
                                 checkout (muse-alab; the others are NOT RUN until the stand-in plays their vendor) in a
                                 private headless jailed aterm against the stand-in vendor laid into a scratch atpkg store
                                 as two builds: the proven scoped claim, a crash relaunch, no move while a draft or a
                                 person's typing is present then the move at a safe point, its own re-exec, signal and
                                 exit propagation; then the negative control (an aterm serving no contract=3: it passes
                                 through, the built-in lanes take the session); its worlds are always kept
  codex-accounts-proof --package DIR --aterm PATH [--keep]
                                 Read-only /accounts in a private native host, real package and fake loopback model.
  codex-source-proof --package DIR --aterm NATIVE_HOST_FIXTURE [--keep]
                                 Debug-only isolated Source candidate against the real native control host; no vendor access.
  codex-measure [--package DIR] [--keep]
                                 measure a codex-alab build for the rollout tap, in a clean checkout (~25 min): the proof
                                 in process expecting it partial, its six legs' rollouts reduced to a fixed point the
                                 privacy oracle passes (each run's) and written to aterm-agent's step testdata (which lists
                                 the build), the standing tests, the proof again expecting it bound, then a docs/measured
                                 note; or a refusal by class (STEM KEY ROLLOUTS FIXED-POINT TEXT NEW-KEY GONE PROOF-RED
                                 STANDING NOT-BOUND) that leaves the checkout as it was and keeps the worlds; exit 0,
                                 writing nothing, for a build whose six legs HEAD holds; NOT RUN for a codex reduction HEAD
                                 does not hold (a killed run's leftover). A key the fold reads that no measured build showed
                                 refuses NEW-KEY; any other new key or record kind (no rollout row kind is inert) waits on
                                 the bound run, is admitted by the row of its leg that carries it, which the note names, and
                                 else refuses NEW-KEY as one no run exercised
  claude-measure [--keep]
                                 measure the managed Claude Code build (`claude --version`) for the session log's
                                 transcript steps, in a clean checkout (two debug builds of aterm and the standing
                                 tests' build, ~35 min; ~10 min to a refusal at step 5): one session in a lab of its own
                                 (harness off, the stub's subscriber path: a turn; four Bash calls, granted once, granted
                                 for the session, declined with Esc and sandboxed with no box offered, each call's end read
                                 off the screen and held to the transcript's; the five-hour limit) expecting it partial,
                                 its transcript reduced to a fixed point the privacy oracle passes and written to
                                 aterm-agent's step testdata as <build>.jsonl (which lists the build), the standing tests,
                                 the session again expecting it on, then a docs/measured note; or a refusal by class (STEM
                                 KEY TRANSCRIPTS FIXED-POINT TEXT NEW-KEY GONE PROOF-RED STANDING NOT-BOUND) that leaves the
                                 checkout as it was and keeps the worlds; exit 0, writing nothing, for a build HEAD holds;
                                 NOT RUN for a Claude Code reduction HEAD does not hold. What no measured build showed is
                                 noted on a row the fold does not act on (an attachment, tools-used); on a row it acts on
                                 (a key no measured build showed there, wherever else it was noted) it refuses NEW-KEY as a
                                 key the fold reads, and is otherwise admitted by the row of run 2's transcript carrying it
                                 and the check that read it, which the note names, or refuses NEW-KEY as one no run
                                 exercised; GONE reads a key the fold reads only on the rows it acts on
  seamless --baseline PATH [--aterm PATH] [--runs N] [--keep]
                                 OPENS A WINDOW, one at a time: one instance of a debug or dev-seams aterm across a real
                                 seamless update (ATERM_DEBUG_SEAMLESS_REEXEC=1, `update apply`), legs gap, topics,
                                 gaptopic, redial, redial75, latch, hwkey and privacy (the files the update wrote, kept
                                 by ATERM_DEBUG_SEAMLESS_KEEP and swept for planted sentinels); then present: N
                                 (default 4, at least 2) fresh instances each of the tested and the baseline aterm,
                                 alternating, the tested medians judged against the baseline runs (n_present inside
                                 their range, present_p50/p95/p99_ms at most their highest) (~1-2 min, plus ~1 min a
                                 run); an --aterm whose broker refuses --lifeline-fd (older than the lifeline) is NOT RUN
  reduce <transcript|rollout> --program claude|codex [--out FILE]
                                 key-only transcript/rollout reducer for harness step fixtures (verifies reader equivalence before writing)
  ptyrec <log> -- <cmd> [args...]
                                 run <cmd> behind a pass-through PTY and record `<t_ns> I|O <hex>` chunks to <log>
  link [--rtt MS] [--take FILE] -- <cmd> [args...]
                                 run <cmd> behind a PTY delaying chunks by RTT/2 each way (default 100 ms)
  mac dmg [--image REF] [--fda] [--keep] | mac nux [--app PATH] [--scenario ROW,...] [--standard-user]
      [--person | --reader] [--no-clt | --vanilla] [--repeat N] [--record [FILE]] | mac provision | mac clean
  mac commons-startup --aterm FILE --record NEW_DIR [--image REF]
                                 the macOS tart guest lane, each run in a fresh clone of a kept SIP-on base: `dmg`, the
                                 dmg_acceptance benches; `nux`, what a person meets on a new Mac (init, doctor, first
                                 launch, terminal and app launch, vault, a vault of the guest's own, successor,
                                 install.sh, and the fault rows: kills, rerun, legacy upgrade, offline, lossy, mDNS,
                                 dmg, agent and SSH, clock) timed in the guest, with a dialog census and the ALab
                                 guard; `provision`, locate Tart or fetch its pinned version without building an app or starting a guest;
                                 `clean`, delete the kept bases (`mac help`; tart fetched when absent)
  linux [--gpu] up|down|status|exec|proof|baseline|gone|cpu|a11y|edges|deck|browser|gpu-host|vz-agent|clean|destroy
                                 manage and run the Ubuntu 24.04 Lima VM runtime lane on aarch64 and Rosetta x86_64, or with
                                 --gpu its krunkit instance on this Mac's GPU (`linux help`)
  windows up|down|status|exec|smoke|window|destroy
                                 manage and run the Windows 11 arm64 QEMU VM runtime lane on aarch64 and Prism x86_64 (`windows help`)

Windowed Instances coordinate one original lab run at a time through the account's window lane.
Ordinary windows request measured Timing admission; unknown CPU observation, expired patience or
`--admit now` cannot qualify them. The explicit functional allocation-monitor window reserves memory
without a quiet-CPU requirement or timing claim and also refuses `--admit now`. Other resource-only
lanes retain their own --admit policy. Admission says once a minute what it is waiting for.

Exit: 0 pass, 1 a finding or a refusal (also an unknown command and, on many verbs, a bad argument), 2 usage,
3 could not run, 128+N cut short by signal N (INT, TERM, HUP).
";

/// The lines of [`USAGE`] that name `verb`: its entry and the lines that continue it.
#[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
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
        "grok" => grok::cmd_grok(args),
        "muse-captures" => muse_captures::cmd_muse_captures(args),
        "autopilot-captures" => autopilot_captures::cmd_autopilot_captures(args),
        "outlook-captures" => outlook_captures::cmd_outlook_captures(args),
        "outlook-perf" => outlook_captures::cmd_outlook_perf(args),
        "muse-selftest" => muse_proof::cmd_muse_selftest(args),
        "muse-relaunch" => relaunch_muse::cmd_muse_relaunch(args),
        "muse-upgrade" => upgrade_muse::cmd_muse_upgrade(args),
        "kimi" => kimi::cmd_kimi(args),
        "agy-upgrade" => upgrade_agy::cmd_agy_upgrade(args),
        "agy-relaunch" => relaunch_agy::cmd_agy_relaunch(args),
        "fleet-sim" => fleet_sim::cmd(args),
        "service-acceptance" => service_acceptance::cmd(args),
        "artifact-serve" => artifact_serve::cmd(args),
        "fleet-menu" => fleet_menu::cmd(args),
        "local-signins" => local_signins::cmd(args),
        "setup-token-lab" => setup_token_lab::cmd(args),
        "reveal-tab" => reveal_tab::cmd(args),
        "commons-lan" => commons_lan::cmd(args),
        "commons-startup" => commons_startup::cmd(args),
        "swarm" => swarm::cmd_swarm(args),
        "mesh" => mesh_churn::cmd(args),
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
        "relaunch" => crash::cmd_relaunch(args),
        "sleep" => sleep::cmd_sleep(args),
        "workers" => workers::cmd_workers(args),
        "work-overlap" => work_overlap::cmd_work_overlap(args),
        "git-starts" => git_starts::cmd_git_starts(args),
        "footers" => footers::cmd_footers(args),
        "halfdraw" => halfdraw::cmd_halfdraw(args),
        "idle-points" => idlepoints::cmd_idle_points(args),
        "restart-jobs" => restart_jobs::cmd_restart_jobs(args),
        "paths" => verbs::cmd_paths(),
        "instance" => instance::cmd_instance(args),
        "resource-attribution" => resource_attribution::cmd(args),
        "recording-cpu" => cpu_observation::cmd(args),
        "idle-cpu" => idle_cpu::cmd(args),
        "disk-lane" => disk_lane::cmd_disk_lane(args),
        "disk-ask" => disk_ask_lane::cmd_disk_ask(args),
        "slot-capture" => slot_lane::cmd_slot_capture(args),
        "slot-lane" => slot_lane::cmd_slot_lane(args),
        "build-admit" => build_admit::cmd(args),
        "tab-gallery" => tab_gallery::cmd(args),
        "trail-gallery" => trail_gallery::cmd_trail_gallery(args),
        "deck-motion" => deck_motion::cmd_deck_motion(args),
        "__trail-redraw-fixture" => trail_gallery::cmd_redraw_fixture(args),
        "trail-contrast" => trail_contrast::cmd_trail_contrast(args),
        "video-sheet" => video_sheet::cmd(args),
        "system-state-gallery" => system_state_gallery::cmd_system_state_gallery(args),
        "allocation-monitor" => allocation_monitor::cmd(args),
        "window-custody" => window_custody::cmd(args),
        "window-custody-owner" => window_custody::owner(args),
        "window-handoff-app" => window_custody::app_band(args),
        "trail-diff" => trail_diff::cmd_trail_diff(args),
        "trail-census" => trail_census::cmd_trail_census(args),
        "trail-companion" => trail_companion::cmd_trail_companion(args),
        "trail-hue" => trail_hue::cmd_trail_hue(args),
        "trail-keys" => trail_keys::cmd_trail_keys(args),
        "trail-replay" => trail_replay::cmd(args),
        "first-key" => first_key::cmd_first_key(args),
        "aos-repair" => aos_repair::cmd_aos_repair(args),
        "aos-cadence" => aos_cadence::cmd_aos_cadence(args),
        "aos-search" => aos_search::cmd_aos_search(args),
        "aos-gpu-bench" => aos_gpu_bench::cmd_aos_gpu_bench(args),
        "aos-learn" => aos_learn::cmd_aos_learn(args),
        "aos-learn-bench" => aos_learn_bench::cmd_aos_learn_bench(args),
        "aos-phase" => aos_phase::cmd_aos_phase(args),
        "presence-flap" => presence::cmd_presence_flap(args),
        "meta-keys" => meta_keys::cmd_meta_keys(args),
        "predict-live" => predict_live::cmd_predict_live(args),
        "codex-alab-proof" => codex_alab_proof::cmd_codex_alab_proof(args),
        "codex-accounts-proof" => codex_alab_proof::cmd_codex_accounts_proof(args),
        "codex-source-proof" => codex_source_proof::cmd(args),
        "codex-boxes" => codex_alab_proof::cmd_codex_boxes(args),
        "harness-proof" => harness_proof::cmd_harness_proof(args),
        "codex-measure" => codex_measure::cmd_codex_measure(args),
        "claude-measure" => claude_measure::cmd_claude_measure(args),
        "seamless" => seamless::cmd_seamless(args),
        "installer-handoff" => installer_handoff::cmd(args),
        "cold-handoff" => installer_handoff::cold_cmd(args),
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
    // First, while this process has one thread, before the argv0 dispatch: the debug build-admit
    // helper reexecs this image to hold a build's claim, and a tethered child starts as this
    // image's tether verb (`argv[1]`, which no argv0 shape shadows). The tether's `entry` also
    // records the unchanged image the front hands its claim to and tethered spawns run through.
    #[cfg(debug_assertions)]
    if let Some(code) = atpkg::build_admit::hold::entry() {
        return code;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(debug_assertions)]
    {
        let mut hidden = std::env::args_os().skip(1);
        if hidden
            .next()
            .is_some_and(|arg| arg == atpkg::build_admit::VERB)
        {
            return atpkg::build_admit::run_hidden(&hidden.collect::<Vec<_>>());
        }
    }
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
        #[cfg(target_os = "macos")]
        Some("claude" | "codex" | "grok" | "agy") if local_signins::is_standin(&argv0) => {
            local_signins::standin_main(&argv0, &args)
        }
        Some("claude") => {
            if swarm::standin_configured() {
                swarm::claude_main(&args)
            } else if occupancy_standin::configured(&argv0) {
                occupancy_standin::claude_main(&args)
            } else {
                fleet_fixture::claude(&args)
            }
        }
        Some("build-loop") => fleet_fixture::build_loop(&args),
        Some("ps") => codex_rig::ps_main(&args),
        _ => {
            link_notice::arm();
            let cmd = args.first().cloned().unwrap_or_default();
            let rest = args.get(1..).unwrap_or_default();
            match cmd.as_str() {
                #[cfg(debug_assertions)]
                "__handoff-stream" => installer_handoff::stream_fixture_main(rest),
                #[cfg(debug_assertions)]
                "__handoff-program" => installer_handoff::program_fixture_main(rest),
                "stub" => stub::main(rest),
                "sampler" => sampler::main(rest),
                "ctty" => relay::ctty(rest),
                "ptyrec" => relay::ptyrec(rest),
                "link" => link::main(rest),
                "probe-inside" => probe::inside_main(rest),
                "probe-tiocsti" => probe::tiocsti_child(),
                #[cfg(target_os = "macos")]
                local_signins::LOOK_UP_VERB => local_signins::look_up_main(rest),
                #[cfg(target_os = "macos")]
                local_signins::REAP_VERB => local_signins::reap_main(rest),
                #[cfg(target_os = "macos")]
                local_signins::SETUP_TOKEN_VERB => local_signins::setup_token_main(rest),
                #[cfg(target_os = "macos")]
                setup_token_lab::PROBE_VERB => setup_token_lab::probe_main(rest),
                "hold" => recorder::hold(rest),
                "keeper" => instance::keeper_main(rest),
                "band" => band::band_main(rest),
                "band-launch" => band::launch_main(rest),
                "band-standin" => band::standin_main(rest),
                "presence-standin" => presence::standin_main(rest),
                slot_lane::HOLD_VERB => slot_lane::hold_main(rest),
                swarm::partition::AGENT_VERB => swarm::partition::agent_main(rest),
                swarm::partition::ACT_VERB => swarm::partition::act_main(rest),
                "init" if std::env::var_os(swarm::partition::AGENT_SEAM).is_some() => {
                    swarm::partition::converge_standin(rest).unwrap_or(2)
                }
                mesh_churn::MEMBER_VERB => mesh_churn::member_main(rest),
                mesh_churn::ACT_VERB => mesh_churn::act_main(rest),
                "init" if std::env::var_os(mesh_churn::MEMBER_SEAM).is_some() => {
                    mesh_churn::converge_standin(rest).unwrap_or(2)
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
                                | "artifact-serve"
                                | "swarm"
                                | "mesh"
                                | "reduce"
                                | "trail-contrast"
                                | "video-sheet"
                                | "trail-census"
                                | "trail-companion"
                                | "trail-hue"
                                | "abrew-admission"
                                | "build-admit"
                                | "recording-cpu"
                                | "git-starts"
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
    // First, while this process has one thread: a tethered child starts as this image's verb.
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(unix)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().is_some_and(|word| word == "artifact-serve") {
            let code = match artifact_serve::cmd(&args[1..]) {
                Ok(code) => code,
                Err(fail) => {
                    if let Some(message) = fail.msg {
                        eprintln!("aterm-lab: {message}");
                    }
                    fail.code
                }
            };
            return std::process::ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1));
        }
    }
    eprintln!("aterm-lab: macOS only (sandbox-exec is the lab's jail)");
    std::process::ExitCode::from(3)
}

/// Create a short `.noindex` temporary directory for unit tests in this crate.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let short = &tag[..tag.floor_char_boundary(8)];
    let builder = aterm_tempfile::Builder::new()
        .prefix(&format!("alb-{short}-"))
        .suffix(".noindex");
    #[cfg(unix)]
    let made = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let made = builder.tempdir();
    made.expect("test scratch dir")
}

/// A canonicalized `.noindex` test scratch directory that dereferences to [`std::path::Path`] and
/// removes itself on drop.
#[cfg(test)]
pub(crate) struct TestScratch {
    _dir: aterm_tempfile::TempDir,
    path: std::path::PathBuf,
}

#[cfg(test)]
impl TestScratch {
    pub(crate) fn new(tag: &str) -> Self {
        let dir = test_scratch_dir(tag);
        let path = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        Self { _dir: dir, path }
    }

    pub(crate) fn as_path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
impl std::ops::Deref for TestScratch {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
impl AsRef<std::path::Path> for TestScratch {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
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
