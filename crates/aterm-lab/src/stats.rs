// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account-stats lab verbs (`account`, `claude-json`, `statusline`, `transcript`, `debuglog`).

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

/// Replace lab scratch, home, and binary paths in `text` with symbolic placeholders.
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

/// `account [tag] composed|off|show|'<json>'`: inspect or configure the stub's usage responses.
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

/// Set `key` in the scratch `.claude.json` to `value`, or remove it when `value` is null.
/// # Errors
/// The file is unreadable, not JSON, or not written.
pub(crate) fn set_claude_json(run: &Run, key: &str, value: J) -> Result<(), Fail> {
    let mut j = claude_json(run)?;
    if matches!(value, J::Null) {
        j.remove(key);
    } else {
        j.set(key, value);
    }
    atomic_write(
        &run.w.join("home/.claude/.claude.json"),
        j.dump(PRETTY).as_bytes(),
    )
    .map_err(|e| Fail::die(e.to_string()))
}

/// `claude-json [tag] [KEY ...]`: inspect keys in the scratch `.claude.json`.
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
        set_claude_json(&run, key, value)?;
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

/// Shell command for the recording status line in `run`.
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

/// `statusline [tag] on [SECS]|off|show`: configure or inspect the recording status line.
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

/// Newest transcript path under the scratch home's `projects/`, if any.
#[must_use]
pub fn newest_transcript(run: &Run) -> Option<PathBuf> {
    let mut files = Vec::new();
    crate::util::walk_files(&run.w.join("home/.claude/projects"), &mut files);
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

/// Summarize one transcript JSONL row for `transcript`.
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

/// `transcript [tag]`: print summarized rows of the newest scratch transcript.
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

/// Path where `claude TAG --debug-log` writes Claude Code's debug log.
#[must_use]
pub fn debug_log(run: &Run) -> PathBuf {
    run.w.join("claude-debug.log")
}

/// Default egrep pattern for `debuglog`.
pub const DEBUG_DEFAULT: &str = "(?i)fetchUtilization|oauth/usage|usage data|usage:|oauth.profile|claude_cli|bootstrap|juniper|cedar|gateway-usage|failed to fetch|http\\.fetch|event logging";

/// `debuglog [tag] [egrep]`: filter lines from Claude Code's debug log.
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
    /// scratch home here, so the deeper path wins) `<scratch>`.
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

    /// Lines of 2.1.301's debug log (labs `uc0` to `uc3`, and the juniper line a
    /// five-hour wall's grant read logs).
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
