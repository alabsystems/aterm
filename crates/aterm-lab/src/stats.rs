// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account-stats verbs (the lab slice L0 of
//! `docs/DESIGN-account-stats-2026-10-15.md`): the stub's claude.ai account
//! bodies (`account`), and the readers of what the real Claude Code wrote in
//! the lab's scratch — its `.claude.json` (`claude-json`), what its status
//! line was handed on stdin (`statusline`), its transcript's rows
//! (`transcript`) and its debug log (`debuglog`, after `claude TAG
//! --debug-log`). Every file they read is the lab's own scratch, read from
//! outside the jail through no link ([`crate::sys::scratch_read`]); the owner's
//! `~/.claude` and `~/.claude.json` stay denied to the jail and are never
//! named here. What they print has the scratch's paths replaced by
//! `<scratch>`, `<sockdir>` and `<home>`.
//!
//! # Account stats facts
//!
//! MEASURED on the managed Claude Code 2.1.301, 2026-10-15, macOS 27, in
//! private headless aterms of this tree (0.105.0, `--features dev-seams`)
//! against the stub: every run launched `claude TAG --subscriber` (the fake
//! claude.ai token), `ANTHROPIC_BASE_URL` the stub, `HTTPS_PROXY` the black
//! hole, and every run's `egress` said `EXTERNAL endpoints seen: 0` with the
//! witness's `claude[<pid>] TCP conn 127.0.0.1:<stub port>`. Three logins:
//! PLAIN (the token's scopes are `user:inference` alone: the bundle's
//! `CLAUDE_CODE_OAUTH_SCOPES` else `["user:inference"]`); PROFILE (`-e
//! 'CLAUDE_CODE_OAUTH_SCOPES=user:inference user:profile'`); MAX (PROFILE plus
//! `-e CLAUDE_CODE_SUBSCRIPTION_TYPE=max -e
//! CLAUDE_CODE_RATE_LIMIT_TIER=default_claude_max_5x` and `claude-json TAG
//! --set oauthAccount composed` before `claude`). Screens are the fixtures
//! `crates/aterm-phase/src/fixtures/claude-2.1.301-*.txt` (`usage-*`,
//! `fable-warning`, `wall-fable-*`, `limit-dialog-no-reset-rows`), whose
//! readings aterm-phase's tests pin; the request counts are each run's stub
//! log, black-hole lines and `debuglog`, and the row counts its
//! `transcript`, which `down` keeps in `logs/<tag>-<time>/` (`stub.log`,
//! `claude-debug.log`, `claude-projects/`) from the runs of 2026-10-16 on:
//! in the state dir, which is the checkout's `target/` unless `LAB_STATE`
//! names another, so they go when the checkout does.
//!
//! * WHERE THE ACCOUNT READS GO (READ-FROM-CODE in the 2.1.301 bundle, then
//!   seen below). Every claude.ai account read is built on the OAuth
//!   config's `BASE_API_URL`, `https://api.anthropic.com` in the shipped
//!   build, never on `ANTHROPIC_BASE_URL`: the usage body
//!   (`plain:"/api/oauth/usage",at_wall:"/api/oauth/usage?at_wall=1&skip_spend=1",cedar_ember:…`),
//!   the profile (`${an().BASE_API_URL}/api/oauth/profile`,
//!   `/api/claude_cli_profile`) and the bootstrap (`/api/claude_cli/bootstrap`).
//!   The local and staging OAuth configs are unreachable (`function
//!   s(){return"prod"}`), and `CLAUDE_CODE_CUSTOM_OAUTH_URL` takes three
//!   listed remote hosts only (`is not an approved endpoint`). So the stub's
//!   account routes ([`crate::stub::account`]) are never called by 2.1.301;
//!   their composed bodies are what A0 and A6 build their fixtures from. (A
//!   gateway login reads `${gateway url}/api/oauth/usage`: another login, not
//!   a subscriber's.)
//! * (1) DOES `/usage` FETCH THE USAGE BODY BEHIND THE BASE URL: NO. One
//!   `/usage` and Enter, counted in the stub's log, the black hole's and the
//!   debug log (the black hole names a host only, as a CONNECT carries no
//!   path; the debug log names the paths): PLAIN, NO request (no
//!   `fetchUtilization` line: the read returns `{}` without the profile
//!   scope); PROFILE and MAX, on both renderers, EXACTLY ONE,
//!   `fetchUtilization: GET /api/oauth/usage (attempt 1)`, to
//!   `api.anthropic.com:443` (the black hole's `CONNECT api.anthropic.com:443`
//!   2 to 7 ms later, refused; `Failed to load usage data: proxy refused the
//!   connection`), and none to the stub. A second `/usage` 37 s later sent
//!   one more: a failed read caches nothing. Measured again 2026-10-16 (the
//!   same Claude Code and aterm) with the logs kept, Enter 24 s after the
//!   turn's answer: `uc2` PLAIN, `uc1` PROFILE inline, `uc3` MAX fullscreen.
//!
//!   LEFT OUT of those counts: Claude Code's own telemetry, which the black
//!   hole refuses in every run, with or without `/usage` (the control `uc0`:
//!   PROFILE, one turn and no `/usage`, shows the same CONNECTs on the same
//!   clock). After each answer, `$.http.fetch (cc-plugin-telemetry): POST
//!   https://api.anthropic.com/api/event_logging/v2/batch` twice (+4.4 to
//!   4.7 s and +4.9 to 5.2 s); the first-party exporter's batch, `1P event
//!   logging: N events failed to export` (+9.3 to 9.5 s, and another 10 s
//!   after Enter holding the `/usage`'s 8 events); and that exporter's
//!   retries, which log no line of their own: CONNECTs within 40 ms of 0.5,
//!   2, 4.5, 8 and 12.5 s apart, its backoff (READ-FROM-CODE: a base delay,
//!   500 ms by default, times the square of the attempt count; its endpoint
//!   `/api/event_logging/v2/batch` on `https://api.anthropic.com` unless its
//!   config names another path or base). In PLAIN one such retry fell
//!   under 260 ms after Enter, 8.005 s after the retry before it.
//!
//!   The tab then draws the session block, the 5h and 7d windows of the
//!   stub's answer HEADERS (`Current session` 42 %, `Current week (all
//!   models)` 61 %, no Fable row), `Could not refresh usage data` and `r to
//!   retry · Esc to cancel` (`usage-header-readings-*`); PLAIN draws the
//!   session block alone (`usage-session-only`).
//! * (2) `cachedUsageUtilization`: NEVER WRITTEN in the lab (absent from the
//!   scratch `.claude.json` after every `/usage`; `claude-json`). By the
//!   bundle (READ-FROM-CODE) only a read that succeeded writes it, `{fetchedAtMs,
//!   accountUuid, utilization}`, not again while the copy for the same
//!   `accountUuid` is under 60 s old; a reader drops one whose
//!   `accountUuid` is not `oauthAccount`'s and uses one for 1 h at most.
//!   How often a real home rewrites it: UNKNOWN (no read
//!   reached anything). Seen written: `cachedExtraUsageDisabledReason: null`
//!   after the first answer whose headers name no disabled reason. Never
//!   written: `oauthAccount` (the bootstrap read at start went to the black
//!   hole, `[Bootstrap] Fetch failed: ERR_PROXY_TUNNEL`, and so did a PROFILE
//!   login's profile read, `Failed to fetch oauth profile from OAuth token:
//!   [REDACTED] proxy refused the connection` (`uc0`, `uc1`); PLAIN logs no
//!   profile read, nor MAX, whose `oauthAccount` the lab sets), so A0's
//!   profile fixture is the composed one
//!   ([`crate::stub::account::composed_oauth_account`], the fields the
//!   bundle's profile merge writes).
//! * (3) THE FABLE WARNING AND WALLS: DRAWN. 200s carrying
//!   `anthropic-ratelimit-unified-7d_oi-utilization: 0.85`, `-7d_oi-reset`
//!   and `-7d_oi-surpassed-threshold: 0.75` (5h 0.42, 7d 0.61, no threshold)
//!   draw `You've used 85% of your Fable limit · resets Oct 19 at 11:36pm
//!   (America/Los_Angeles)` on the right of the footer's last row, and it
//!   stays (`fable-warning`; the bundle warns from 0.7 up). A 429 claiming
//!   `seven_day_overage_included` with a `-reset` ends the turn on `⎿
//!   You've hit your Fable limit · resets 1:36am (America/Los_Angeles)`;
//!   without one, on `⎿  You've hit your Fable limit` (`wall-fable-reset`,
//!   `wall-fable-no-reset`). No dialog opened at either, with
//!   `autoContinueAtUsageLimit` off. With the harness ON (its default in the
//!   lab) it read the first as the Fable bucket and relaunched the session
//!   `--model opus` (`model-fallback@v1`), which the lab's relaunch leaves
//!   without the fake token (`Not logged in`): measure walls with
//!   `LAB_HARNESS_KEYS='enabled = false'`.
//! * (4) THE AT-WALL GRANT BLOCKS: NOT DRAWN FROM THE STUB, for two reasons.
//!   Behind a base URL that is not Anthropic's the reset programs do not ask
//!   (their gate needs both a first-party login, which a gateway login is
//!   not, and a first-party base URL: no `ANTHROPIC_BASE_URL`,
//!   `api.anthropic.com`, `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL`, or a
//!   `--client-data-url` (`CLAUDE_CODE_CLIENT_DATA_URL`) document with an
//!   entry whose `clientData.assume_first_party_base_url` is `true` (not
//!   applied when the host manages the provider or that variable is set,
//!   withdrawn when the launch does not use the document;
//!   its URL https on `downloads.claude.ai` and the document signed to the
//!   bundle's compiled roots, so the lab cannot serve one)): at a five-hour
//!   wall with `tengu_cedar_ember` and `tengu_nifty_lemur` enabled in the
//!   scratch `cachedGrowthBookFeatures`, no read was made. With
//!   `_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL=1` as well, the wall sent TWO
//!   `GET /api/oauth/usage?at_wall=1&skip_spend=1`, both to
//!   `api.anthropic.com:443` (two black-hole refusals within 11 ms;
//!   `[juniper-tide] status fetch failed` and `[cedar-ember] status fetch
//!   failed`, twice each), and the dialog it opened offered no reset row:
//!   `Stop and wait …`, `Wait here, …`, `Add funds …`, `Upgrade your plan`
//!   (`limit-dialog-no-reset-rows`). NOTHING was pressed on it (the harness
//!   off, `limit_reset = false`). The grant rows (`Use your reset now
//!   ({left} left, until {date})` then; 2.1.304's `Use a {kind} ({left}
//!   left, until {date})`) and `available again {date}` stay
//!   READ-FROM-CODE, their fixtures composed (aterm-phase's
//!   `reset::fixtures`).
//! * (5) THE STATUS LINE'S STDIN (`statusline TAG on`, then `show`): after
//!   one turn, `cost.total_cost_usd` 0.045628 (`model.id`
//!   `claude-sonnet-5-5`, PLAIN: 22.8k input, 12 output) and 0.092048
//!   (`claude-opus-5-5`, MAX: 23.0k, 12) — Claude Code's own price of the
//!   stub's usage, a float with binary noise (`0.09206400000000001` seen);
//!   `model.display_name` `Sonnet 5.5` / `Opus 5.5`;
//!   `rate_limits.five_hour.used_percentage` 42.6 off a `-5h-utilization` of
//!   0.4257 (one decimal; the tab floors the same reading to `42% used`),
//!   `resets_at` in unix seconds; no `rate_limits` before the first answer.
//!   The record's 47 key paths include `cost.total_lines_added`,
//!   `context_window.*`, `prompt_cache.*`, `effort.level`, `session_id`,
//!   `transcript_path` and `cwd` (names only; the paths are the lab's).
//! * (6) ENTER AFTER `/usage` AND THE ROWS IT LEAVES. Typed, `/usage` stands
//!   highlighted first in the command menu, `/usage-credits` right under it
//!   (MAX; PLAIN lists no `/usage-credits`), and Enter opens the USAGE tab on
//!   both renderers, never `/usage-credits` (`usage-menu-*`, then the tab).
//!   One Escape then leaves, in the transcript, TWO rows on the inline
//!   renderer — `system/local_command` `<command-name>/usage</command-name>…`
//!   and `<local-command-stdout>Settings dialog dismissed</local-command-stdout>`,
//!   drawn `⎿  Settings dialog dismissed` (`usage-dismissed-inline`) — and a
//!   THIRD as the session ends: a `last-prompt` whose `leafUuid` is that
//!   `Settings dialog dismissed` row, before the exit `cost-state`, so the
//!   `/usage` is the transcript's leaf. Measured again 2026-10-16 with the
//!   transcripts kept: that row is not there 60 s after the Escape and is
//!   there once `down` has ended the session (`m7ui` PLAIN, and `uc1`,
//!   `uc2`); the inline controls without `/usage` (`m7ci` PLAIN, `uc0`) end
//!   on their turn's `last-prompt` and one `cost-state`. On the fullscreen
//!   renderer ZERO rows, to the session's end (`transcript`;
//!   `usage-dismissed-fullscreen`): `m7uf` PLAIN and `uc3` end as the
//!   fullscreen control `m7cf` does, on two `cost-state` rows. A second
//!   `/usage` inline left two more `system/local_command` rows right after
//!   its Escape (2026-10-15).
//!
//! NOT MEASURED, and why: anything Claude Code would draw or store from a
//! usage body, a profile or a grant block (`Current week (Fable)` off a
//! `limits[]` row, credits dollars, the one-time credit blocks, a written
//! `cachedUsageUtilization`, `oauthAccount` from a profile, a grant row),
//! since 2.1.301 sends none of those reads to a base URL the lab can serve;
//! whether the server's utilization is finer than whole percents.
//!
//! The recipe, as run: `LAB_HARNESS_KEYS='enabled = false' LAB_TRUST=1 up
//! TAG; account TAG composed; headers TAG '{"on200":{…}}'; statusline TAG
//! on 2; [claude-json TAG --set oauthAccount composed;] claude TAG
//! --subscriber --debug-log [-e …]; turn TAG hi; send TAG /usage; enter TAG;
//! key TAG escape;` then `stublog`, `debuglog` (by default every line that
//! names a request, telemetry's included: [`DEBUG_DEFAULT`]), `claude-json
//! TAG cachedUsageUtilization oauthAccount`, `transcript`, `statusline TAG
//! show` and `egress`; the control is the same without `/usage`. A wall:
//! `limit TAG CLAIM SECONDS`, or a custom 429 with `mode TAG NAME '<spec>'`
//! (a name that is no built-in mode's).

use std::path::PathBuf;

use crate::paths::paths;
use crate::pyjson::{J, Style};
use crate::run::{Run, take_tag};
use crate::util::{atomic_write, one_line, scratch_text, shell_quote};
use crate::{Fail, LAB, Res};

fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

const PRETTY: Style = Style {
    indent: Some(2),
    compact: false,
    ascii: false,
};

/// `text` with the lab's own paths named, never spelled: the scratch, the
/// socket dir (the project sits in it) and the owner's homes (the account's,
/// and `$HOME` when it names another; the deeper first).
#[must_use]
pub fn scrub(run: &Run, text: &str) -> String {
    scrub_in(paths(), run, text)
}

/// [`scrub`] with the homes of the lab's facts `p` (the seam a test hands a
/// fake account home: [`crate::paths::Paths::resolve_with`]).
fn scrub_in(p: &crate::paths::Paths, run: &Run, text: &str) -> String {
    let mut out = text.to_string();
    let w = run.w.to_string_lossy().into_owned();
    let mut homes = p.guarded_homes();
    homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
    let mut pairs = vec![
        (w.as_str(), "<scratch>"),
        (run.sockdir.as_str(), "<sockdir>"),
    ];
    pairs.extend(homes.iter().map(|h| (h.as_str(), "<home>")));
    for (from, to) in pairs {
        if from.len() > 1 {
            out = out.replace(from, to);
            // the same directory as the kernel spells it under /private
            if let Some(rest) = from.strip_prefix("/tmp/") {
                out = out.replace(&format!("/private/tmp/{rest}"), to);
            }
        }
    }
    out
}

/// The account file the stub's claude.ai routes answer from.
fn account_file(run: &Run) -> PathBuf {
    run.w.join("account.json")
}

/// `account [tag] composed|off|show|'<json>'`: the bodies of the stub's
/// claude.ai account routes ([`crate::stub::account`]).
pub fn cmd_account(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let what = a.first().map_or("show", String::as_str);
    let j = match what {
        "show" => {
            let text = scratch_text(&account_file(&run));
            println!(
                "{}",
                if text.trim().is_empty() {
                    "{}"
                } else {
                    text.trim()
                }
            );
            return Ok(0);
        }
        "off" => J::obj([]),
        "composed" => crate::stub::account::composed(),
        text => J::parse(text).map_err(|e| Fail::die(format!("account: {e}")))?,
    };
    if !matches!(j, J::Obj(_)) {
        return Err(Fail::die(
            "account: an object {\"usage\": …, \"profile\": …, \"bootstrap\": …}",
        ));
    }
    atomic_write(&account_file(&run), j.dump(PRETTY).as_bytes())
        .map_err(|e| Fail::die(e.to_string()))?;
    println!("{}", j.dump(PRETTY));
    Ok(0)
}

/// The scratch `.claude.json` (`CLAUDE_CONFIG_DIR` is the scratch
/// `home/.claude`), parsed.
fn claude_json(run: &Run) -> Result<J, Fail> {
    let p = run.w.join("home/.claude/.claude.json");
    let text = crate::verbs::read_scratch(&p)?;
    J::parse(&text).map_err(|e| Fail::die(format!("the scratch .claude.json: {e}")))
}

/// `claude-json [tag] [KEY ...]`: the top-level key names of the scratch
/// `.claude.json`, then each named key's value; `claude-json [tag] --set KEY
/// '<json>'|composed` sets one (null removes it; `composed` is the
/// `oauthAccount` of the stub's composed profile,
/// [`crate::stub::account::composed_oauth_account`]) — before `claude`, since
/// a running Claude Code rewrites the file.
pub fn cmd_claude_json(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    if a.first().map(String::as_str) == Some("--set") {
        let (Some(key), Some(text)) = (a.get(1), a.get(2)) else {
            return Err(Fail::die("claude-json --set KEY '<json>'|composed"));
        };
        let value = if text == "composed" {
            crate::stub::account::composed_oauth_account(crate::util::now_ms())
        } else {
            J::parse(text).map_err(|e| Fail::die(format!("claude-json --set: {e}")))?
        };
        let mut j = claude_json(&run)?;
        if matches!(value, J::Null) {
            j.remove(key);
        } else {
            j.set(key, value);
        }
        atomic_write(
            &run.w.join("home/.claude/.claude.json"),
            j.dump(PRETTY).as_bytes(),
        )
        .map_err(|e| Fail::die(e.to_string()))?;
        println!("{key} set in the scratch .claude.json");
        return Ok(0);
    }
    let j = claude_json(&run)?;
    let J::Obj(m) = &j else {
        return Err(Fail::die("the scratch .claude.json is not an object"));
    };
    let names: Vec<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
    println!("keys ({}): {}", names.len(), names.join(" "));
    for k in &a {
        match j.get(k) {
            Some(v) => println!("{k} = {}", scrub(&run, &v.dump(PRETTY))),
            None => println!("{k}: absent"),
        }
    }
    Ok(0)
}

/// Where the recording status line appends what it was handed.
fn statusline_file(run: &Run) -> PathBuf {
    run.w.join("statusline.jsonl")
}

/// The recording status line's command: stdin appended to the scratch's
/// `statusline.jsonl`, one record a line, and one row drawn.
#[must_use]
pub fn statusline_command(run: &Run) -> String {
    let f = shell_quote(&statusline_file(run).to_string_lossy());
    format!("cat >> {f}; echo >> {f}; echo 'lab status line'")
}

/// Every dotted key path of `j` (arrays as `[]`), names only.
fn key_paths(j: &J, prefix: &str, out: &mut Vec<String>) {
    match j {
        J::Obj(m) => {
            for (k, v) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if matches!(v, J::Obj(_) | J::Arr(_)) {
                    key_paths(v, &p, out);
                } else {
                    out.push(p);
                }
            }
        }
        J::Arr(a) => {
            let p = format!("{prefix}[]");
            match a.first() {
                Some(v) => key_paths(v, &p, out),
                None => out.push(p),
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

/// The value at a dotted path, as text (`-` when absent).
fn at_path(j: &J, path: &str) -> String {
    let mut v = j;
    for part in path.split('.') {
        match v.get(part) {
            Some(n) => v = n,
            None => return "-".into(),
        }
    }
    match v {
        J::Str(s) => s.clone(),
        other => other.py(),
    }
}

/// The figures of one record the status line was handed.
const SHOWN: &[&str] = &[
    "model.id",
    "model.display_name",
    "cost.total_cost_usd",
    "cost.total_api_duration_ms",
    "cost.total_duration_ms",
    "rate_limits.five_hour.used_percentage",
    "rate_limits.five_hour.resets_at",
    "rate_limits.seven_day.used_percentage",
    "rate_limits.seven_day.resets_at",
];

/// `statusline [tag] on [SECS]|off|show`: a status line that records what
/// Claude Code hands it on stdin (its `refreshInterval` SECS, 5 by default),
/// taken out again, or the records: per record the model, the cost and the
/// windows, then the key paths of the last one (names only).
pub fn cmd_statusline(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    match a.first().map_or("show", String::as_str) {
        "on" => {
            let secs: i64 = a
                .get(1)
                .map_or(Ok(5), |s| s.parse())
                .map_err(|_| Fail::die("statusline on SECS: a whole number"))?;
            let line = J::obj([
                ("type", J::s("command")),
                ("command", J::s(statusline_command(&run))),
                ("refreshInterval", J::Int(secs)),
            ]);
            crate::verbs::settings_merge(&run, &J::obj([("statusLine", line)]))?;
            println!(
                "statusline: recording into {}",
                statusline_file(&run).display()
            );
            Ok(0)
        }
        "off" => {
            crate::verbs::settings_merge(&run, &J::obj([("statusLine", J::Null)]))?;
            println!("statusline: off");
            Ok(0)
        }
        "show" => {
            let text = scratch_text(&statusline_file(&run));
            let recs: Vec<J> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| J::parse(l).ok())
                .collect();
            println!("records: {}", recs.len());
            for (i, r) in recs.iter().enumerate() {
                let fields: Vec<String> = SHOWN
                    .iter()
                    .map(|p| format!("{p}={}", at_path(r, p)))
                    .collect();
                println!("#{} {}", i + 1, fields.join(" "));
            }
            if let Some(last) = recs.last() {
                let mut keys = Vec::new();
                key_paths(last, "", &mut keys);
                println!("keys of the last record ({}):", keys.len());
                for k in keys {
                    println!("  {k}");
                }
            }
            Ok(0)
        }
        other => Err(Fail::die(format!(
            "statusline: on [SECS], off or show, not '{other}' ({LAB} help)"
        ))),
    }
}

/// The newest transcript under the scratch home's `projects/`, or `None`.
#[must_use]
pub fn newest_transcript(run: &Run) -> Option<PathBuf> {
    let mut files = Vec::new();
    crate::util::walk_files(&run.w.join("home/.claude/projects"), &mut files);
    // each file's time off its checked descriptor: a link swapped in after the listing is refused there
    files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| {
            let f = crate::sys::scratch_file(&p).ok()??;
            let t = f.metadata().and_then(|m| m.modified()).ok()?;
            Some((t, p))
        })
        .max()
        .map(|(_, p)| p)
}

/// One transcript row as `transcript` prints it: `type[/subtype]`, a
/// `meta` mark, and the text of a system row or of a user row's string
/// content.
#[must_use]
pub fn row_summary(row: &J) -> String {
    let kind = row.get("type").and_then(J::as_str).unwrap_or("?");
    let sub = row.get("subtype").and_then(J::as_str);
    let mut s = match sub {
        Some(st) => format!("{kind}/{st}"),
        None => kind.to_string(),
    };
    if row.get("isMeta").is_some_and(J::truthy) {
        s.push_str(" meta");
    }
    let text = match kind {
        "system" => row.get("content").and_then(J::as_str).map(str::to_string),
        "user" => row
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(J::as_str)
            .map(str::to_string),
        _ => None,
    };
    if let Some(t) = text {
        s.push_str(&format!(" {}", crate::util::py_repr(&one_line(&t, 100))));
    }
    s
}

/// `transcript [tag]`: the rows of the newest transcript in the scratch home,
/// one line each ([`row_summary`]), then how many of each kind.
pub fn cmd_transcript(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    let f = newest_transcript(&run).ok_or_else(|| {
        Fail::die(format!(
            "no transcript under {}/home/.claude/projects yet",
            run.w.display()
        ))
    })?;
    println!("# {}", scrub(&run, &f.to_string_lossy()));
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut n = 0;
    for l in scratch_text(&f).lines().filter(|l| !l.trim().is_empty()) {
        n += 1;
        let row = J::parse(l).unwrap_or(J::Null);
        let s = row_summary(&row);
        println!("{n:3} {}", scrub(&run, &s));
        let kind = s.split(' ').next().unwrap_or("?").to_string();
        match counts.iter_mut().find(|(k, _)| *k == kind) {
            Some(c) => c.1 += 1,
            None => counts.push((kind, 1)),
        }
    }
    let c: Vec<String> = counts.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("rows={n} {}", c.join(" "));
    Ok(0)
}

/// Where `claude TAG --debug-log` has Claude Code write its debug log.
#[must_use]
pub fn debug_log(run: &Run) -> PathBuf {
    run.w.join("claude-debug.log")
}

/// The debug lines `debuglog` shows by default, in any case: every line that
/// names a request or its failure, so a count read off them can be checked
/// against the black hole's CONNECTs — the account reads and their failures,
/// the profile and bootstrap fetches, the limit-reset programs, the other
/// fetches at start, and the telemetry (`$.http.fetch`, `1P event logging`).
pub const DEBUG_DEFAULT: &str = "(?i)fetchUtilization|oauth/usage|usage data|usage:|oauth.profile|claude_cli|bootstrap|juniper|cedar|gateway-usage|failed to fetch|http\\.fetch|event logging";

/// `debuglog [tag] [egrep]`: the lines of Claude Code's debug log (launched
/// `claude TAG --debug-log`) matching the pattern ([`DEBUG_DEFAULT`]).
pub fn cmd_debuglog(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let pat = a.first().map_or(DEBUG_DEFAULT, String::as_str);
    let re = aterm_regex::Regex::new(pat).map_err(|e| Fail::die(format!("debuglog: {e}")))?;
    let text = scratch_text(&debug_log(&run));
    if text.is_empty() {
        return Err(Fail::die(format!(
            "no debug log at {} (launch with {LAB} claude {} --debug-log)",
            debug_log(&run).display(),
            run.tag
        )));
    }
    for l in text.lines().filter(|l| re.is_match(l)) {
        println!("{}", scrub(&run, l));
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A STATS PRINT NAMES EVERY HOME THE LAB GUARDS, NEVER SPELLS IT: under
    /// a scratch `HOME` (a fake account home handed in through
    /// [`crate::paths::Paths::resolve_with`]) the account's home and the
    /// scratch one both become `<home>`, the run's scratch (inside the
    /// scratch home here, so the deeper path wins) `<scratch>`. NEGATIVE
    /// CONTROL: the facts of a run from the account's own `HOME` leave the
    /// scratch home spelled.
    #[test]
    fn the_stats_scrub_names_the_account_home_and_a_scratch_home_both() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-stats-")
            .tempdir()
            .unwrap();
        let real = std::fs::canonicalize(top.path()).unwrap();
        let (account, scratch) = (real.join("account"), real.join("scratch"));
        for d in [&account, &scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (a, s) = (account.display().to_string(), scratch.display().to_string());
        let run = Run {
            w: scratch.join("w"),
            sockdir: "/private/tmp/at.sock".into(),
            ..Run::default()
        };
        let text = format!("{a}/.claude.json {s}/.zshrc {s}/w/x /private/tmp/at.sock/proj");
        let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
        assert_eq!(
            scrub_in(&p, &run, &text),
            "<home>/.claude.json <home>/.zshrc <scratch>/x <sockdir>/proj"
        );
        // NEGATIVE CONTROL
        let own = crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
        assert_eq!(
            scrub_in(&own, &run, &text),
            format!("<home>/.claude.json {s}/.zshrc <scratch>/x <sockdir>/proj")
        );
    }

    #[test]
    fn key_paths_name_every_leaf_and_no_value() {
        let j = J::parse(r#"{"model":{"id":"m","display_name":"M"},"cost":{"total_cost_usd":0.5},"x":[{"y":1}],"e":[]}"#)
            .unwrap();
        let mut out = Vec::new();
        key_paths(&j, "", &mut out);
        // the parser hands objects back in key order
        assert_eq!(
            out,
            [
                "cost.total_cost_usd",
                "e[]",
                "model.display_name",
                "model.id",
                "x[].y"
            ]
        );
        assert_eq!(at_path(&j, "cost.total_cost_usd"), "0.5");
        assert_eq!(at_path(&j, "model.id"), "m");
        assert_eq!(at_path(&j, "rate_limits.five_hour"), "-");
    }

    /// Lines of 2.1.301's debug log (labs `uc0` to `uc3`, 2026-10-16; the
    /// juniper one as fact (4) quotes it).
    #[test]
    fn the_default_debug_lines_are_those_that_name_a_request() {
        let re = aterm_regex::Regex::new(DEBUG_DEFAULT).unwrap();
        for l in [
            "[DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)",
            "[ERROR] Failed to load usage data: proxy refused the connection",
            "[ERROR] Failed to fetch oauth profile from OAuth token: [REDACTED] proxy refused the connection",
            "[DEBUG] [Bootstrap] Fetch failed: ERR_PROXY_TUNNEL",
            "[ERROR] Failed to fetch MCP registry: proxy refused the connection",
            "[DEBUG] $.http.fetch (cc-plugin-telemetry): POST https://api.anthropic.com/api/event_logging/v2/batch",
            "[ERROR] 1P event logging: 141 events failed to export (code=ERR_PROXY_TUNNEL, proxy refused the connection)",
            "[juniper-tide] status fetch failed",
        ] {
            assert!(re.is_match(l), "{l}");
        }
        for l in [
            "[DEBUG] Failed to stat directory <scratch>/home/.claude/agents: ENOENT: no such file or directory",
            "[DEBUG] [engine] turn 1 end (turns=1 usage in=22755 out=12 cost=$0.0456 api=21ms stop=end_turn resultLen=51)",
            "[DEBUG] hooks module cc-plugin-telemetry@builtin telemetry.log settled in 0.1ms (native link, next() included)",
            "[DEBUG] File <scratch>/home/.claude/.claude.json written atomically",
        ] {
            assert!(!re.is_match(l), "{l}");
        }
    }

    #[test]
    fn a_transcript_row_is_its_kind_and_the_text_a_person_typed() {
        let sys = J::parse(r#"{"type":"system","subtype":"local_command","content":"<command-name>/usage</command-name>","isMeta":false}"#).unwrap();
        assert_eq!(
            row_summary(&sys),
            "system/local_command '<command-name>/usage</command-name>'"
        );
        let user = J::parse(r#"{"type":"user","isMeta":true,"message":{"content":"hi"}}"#).unwrap();
        assert_eq!(row_summary(&user), "user meta 'hi'");
        let asst =
            J::parse(r#"{"type":"assistant","message":{"content":[{"type":"text"}]}}"#).unwrap();
        assert_eq!(row_summary(&asst), "assistant");
    }
}
