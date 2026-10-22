// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The small verbs on a lab that is up: the stub's mode and scripts, the
//! scratch settings and `aterm.toml`, typing into the tab, reading it, the
//! witnesses, fixtures in the repo's shape, and a newer managed build.

use std::fs;

use std::path::Path;

use crate::LAB;
use crate::door::Door;
use crate::lab::{clean_ver, mask_account};
use crate::paths::paths;
use crate::procs::alive;
use crate::pyjson::{J, Style};
use crate::run::{Run, take_tag};
use crate::stub::{control, query_escape};
use crate::util::{atomic_write, hms, now_secs, scratch_text};
use crate::{Fail, Res};

/// The lab a verb acts on, and its remaining arguments.
fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

const SETTINGS_STYLE: Style = Style {
    indent: Some(2),
    compact: false,
    ascii: true,
};

/// Deep-merge `patch` into the scratch `settings.json` (a running Claude re-reads it); the result.
///
/// # Errors
/// The settings could not be read, parsed or written.
pub fn settings_merge(run: &Run, patch: &J) -> Result<J, Fail> {
    let p = run.settings();
    let text = read_scratch(&p)?;
    let mut d = J::parse(&text).map_err(|e| Fail::die(format!("{}: {e}", p.display())))?;
    crate::pyjson::merge(&mut d, patch);
    atomic_write(&p, d.dump(SETTINGS_STYLE).as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    Ok(d)
}

/// A file of the scratch, read through no link on its whole path; the refusal
/// when it is missing or is not this user's regular file with one name.
///
/// # Errors
/// It is missing, or refused.
pub fn read_scratch(p: &Path) -> Result<String, Fail> {
    match crate::sys::scratch_read(p) {
        Ok(Some(b)) => Ok(String::from_utf8_lossy(&b).into_owned()),
        Ok(None) => Err(Fail::die(format!(
            "cannot read {}: no such file",
            p.display()
        ))),
        Err(e) => Err(Fail::die(e.to_string())),
    }
}

/// Switch the stub's mode (a `bg` mode also allows `Bash(sleep …)` in the scratch settings); its answer.
///
/// # Errors
/// The stub did not answer, or the settings could not be edited.
pub fn set_mode(run: &Run, m: &str) -> Result<String, Fail> {
    let answer = control(&run.port, &format!("/__mode?m={}", query_escape(m))).map_err(|e| {
        Fail::die(format!(
            "mode: the stub of lab '{}' did not take mode {m} ({e})",
            run.tag
        ))
    })?;
    if m == "bg" || m.starts_with("bg:") {
        allow_sleep(run)?;
    }
    Ok(answer)
}

/// `mode [tag] [name ['<spec>']]`: read or switch the stub's mode; with a
/// spec, the custom mode `name` is written first (`modes/<name>.json`: the
/// stub module's `{"status", "headers", "json"|"body", "stream", …}`).
pub fn cmd_mode(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    match (a.first(), a.get(1)) {
        (None, _) => println!("{}", control(&run.port, "/__mode")?),
        (Some(m), None) => println!("{}", set_mode(&run, m)?),
        (Some(m), Some(spec)) => {
            let j = J::parse(spec).map_err(|e| Fail::die(format!("mode {m}: {e}")))?;
            println!("{}", set_custom_mode(&run, m, &j)?);
        }
    }
    Ok(0)
}

/// Write the custom mode `m` (`modes/<m>.json`, the stub module's `{"status",
/// "headers", "json"|"body", "stream", …}`) and switch the stub to it; the
/// stub's answer.
///
/// # Errors
/// `m` is no custom mode's name ([`custom_mode_name`]), `spec` no JSON
/// object, or the write or the switch failed.
pub fn set_custom_mode(run: &Run, m: &str, spec: &J) -> Result<String, Fail> {
    let name = custom_mode_name(m)?;
    if !matches!(spec, J::Obj(_)) {
        return Err(Fail::die(format!("mode {m}: the spec is a JSON object")));
    }
    atomic_write(
        &run.w.join(format!("modes/{name}.json")),
        spec.py().as_bytes(),
    )
    .map_err(|e| Fail::die(e.to_string()))?;
    set_mode(run, &name)
}

/// `m` as the name of a custom mode the stub will serve from
/// `modes/<m>.json`; refused when the stub would answer it otherwise (a
/// built-in mode's name, Codex's `refresh` answer: [`crate::stub::not_custom`])
/// or would read another name (what [`crate::stub::clean_mode`] changes, a `:`).
///
/// # Errors
/// `m` is not such a name.
pub fn custom_mode_name(m: &str) -> Result<String, Fail> {
    let name = crate::stub::clean_mode(m);
    if name != m || name.contains(':') {
        return Err(Fail::die(format!(
            "mode: a custom mode's name is letters, digits, '.', '_' and '-' ('{m}')"
        )));
    }
    if let Some(why) = crate::stub::not_custom(&name) {
        return Err(Fail::die(format!(
            "mode: '{name}' is {why}, never served as a custom mode: name the spec otherwise"
        )));
    }
    Ok(name)
}

/// The `bg` mode's tool use runs `sleep N` in the background: let Claude run it
/// without asking (a prompt would stall it).
fn allow_sleep(run: &Run) -> Result<(), Fail> {
    let p = run.settings();
    let text = read_scratch(&p)?;
    let mut d = J::parse(&text).map_err(Fail::die)?;
    let perms = d.entry_obj("permissions");
    let mut allow: Vec<J> = perms
        .get("allow")
        .and_then(J::as_arr)
        .map(<[J]>::to_vec)
        .unwrap_or_default();
    for rule in ["Bash(sleep:*)", "Bash(sleep *)"] {
        if !allow.iter().any(|r| r.as_str() == Some(rule)) {
            allow.push(J::s(rule));
        }
    }
    perms.set("allow", J::Arr(allow));
    atomic_write(&p, d.dump(SETTINGS_STYLE).as_bytes()).map_err(|e| Fail::die(e.to_string()))
}

/// `limit [tag] CLAIM SECONDS [PLAN]`: a subscription usage limit that resets
/// SECONDS from now, then lifts; Codex gets its own `usage_limit_reached` body
/// (PLAN: its `plan_type`) with the claim's window at 100% in its `x-codex-*` headers.
pub fn cmd_limit(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let claim = a.first().cloned().unwrap_or_default();
    let secs = a.get(1).cloned().unwrap_or_default();
    let plan = a.get(2).cloned().unwrap_or_default();
    if !plan
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(Fail::die(
            "limit: PLAN is Codex's plan_type (plus, pro, free, team, ...)",
        ));
    }
    if claim.is_empty()
        || !claim
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(Fail::die(format!(
            "limit: CLAIM is five_hour, seven_day, ... ({LAB} help)"
        )));
    }
    let Ok(n) = secs.parse::<i64>().map_err(drop).and_then(|n| {
        if secs.chars().all(|c| c.is_ascii_digit()) {
            Ok(n)
        } else {
            Err(())
        }
    }) else {
        return Err(Fail::die(
            "limit: SECONDS is a whole number of seconds from now",
        ));
    };
    let reset = now_secs() + n;
    let plan_part = if plan.is_empty() {
        String::new()
    } else {
        format!(":{plan}")
    };
    let answer = control(
        &run.port,
        &format!("/__mode?m=limit:{claim}:{reset}{plan_part}"),
    )
    .map_err(|e| {
        Fail::die(format!(
            "limit: the stub of lab '{}' did not take the limit ({e})",
            run.tag
        ))
    })?;
    println!("{answer}");
    println!(
        "limit: {claim}, resets at {} ({reset}){}; the stub answers ok from then on",
        hms(reset),
        if plan.is_empty() {
            String::new()
        } else {
            format!(", plan_type {plan}")
        }
    );
    Ok(0)
}

/// `tool [tag] '<json>'`: the tool mode's script, and switch the stub to that mode.
pub fn cmd_tool(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let text = a.first().ok_or_else(|| {
        Fail::die(format!(
            "tool: need the JSON of a call or a list of them ({LAB} help)"
        ))
    })?;
    let (j, answer) = set_tool(&run, text)?;
    println!("{}", j.py());
    println!("{answer}");
    Ok(0)
}

/// Write the tool mode's script (`@PROJ@` the project dir, `@PORT@` the
/// stub's port) and switch the stub to that mode: the script and the stub's answer.
///
/// # Errors
/// Not JSON, the script could not be written, or the stub did not take the mode.
pub fn set_tool(run: &Run, text: &str) -> Result<(J, String), Fail> {
    let proj =
        fs::canonicalize(&run.proj).map_or(run.proj.clone(), |p| p.to_string_lossy().into_owned());
    let j = J::parse(&text.replace("@PROJ@", &proj).replace("@PORT@", &run.port))
        .map_err(|e| Fail::die(format!("tool: {e}")))?;
    atomic_write(&run.w.join("tool.json"), j.py().as_bytes())
        .map_err(|e| Fail::die(e.to_string()))?;
    let answer = control(&run.port, "/__mode?m=tool").map_err(|e| {
        Fail::die(format!(
            "tool: the stub of lab '{}' did not take mode tool ({e})",
            run.tag
        ))
    })?;
    Ok((j, answer))
}

/// `headers`' argument (none reads as `{}`, which clears them): the
/// `{"on200":{…},"on429":{…}}` blocks, an object of objects under those keys
/// alone ([`crate::stub::RATE_BLOCKS`]).
///
/// # Errors
/// Not JSON, or not that shape.
pub fn rate_blocks(text: Option<&str>) -> Result<J, String> {
    let j = J::parse(text.unwrap_or("{}")).map_err(|e| format!("headers: {e}"))?;
    match &j {
        J::Obj(m)
            if m.iter().all(|(k, v)| {
                crate::stub::RATE_BLOCKS.contains(&k.as_str()) && matches!(v, J::Obj(_))
            }) =>
        {
            Ok(j)
        }
        _ => {
            Err(r#"headers: expected {"on200": {header: value}, "on429": {header: value}}"#.into())
        }
    }
}

/// Write `new`'s own entries into the lab's `--headers-file`
/// (`scratch/headers.json`) and keep the other verb's
/// ([`crate::stub::headers_file_with`]): the `on200`/`on429` blocks when
/// `blocks` (`headers`), else the flat entries (`ratelimits`).
///
/// # Errors
/// The file could not be written.
pub fn write_headers(w: &Path, new: J, blocks: bool) -> Result<(), Fail> {
    let p = w.join("headers.json");
    let h = crate::stub::headers_file_with(&scratch_text(&p), new, blocks);
    let text = h.dump(Style {
        indent: Some(1),
        compact: false,
        ascii: true,
    });
    atomic_write(&p, text.as_bytes()).map_err(|e| Fail::die(e.to_string()))
}

/// `headers [tag] ['<json>']`: the stub's extra headers per answer status on
/// `/v1/messages` (`{"on200":{…},"on429":{…}}`, re-read per request;
/// [`crate::stub::rate_headers`]); `{}` or nothing clears them, and the flat
/// entries `ratelimits` wrote stay ([`write_headers`]).
pub fn cmd_headers(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let j = rate_blocks(a.first().map(String::as_str)).map_err(Fail::die)?;
    write_headers(&run.w, j.clone(), true)?;
    println!("{}", j.py());
    Ok(0)
}

/// `settings [tag] '<json>'`: deep-merge into the scratch `settings.json`.
pub fn cmd_settings(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let text = a
        .first()
        .ok_or_else(|| Fail::die(format!("settings: need a JSON object ({LAB} help)")))?;
    let patch = J::parse(text).map_err(|e| Fail::die(format!("settings: {e}")))?;
    let mut d = settings_merge(&run, &patch)?;
    d.remove("env");
    d.remove("apiKeyHelper");
    println!("{}", d.py());
    Ok(0)
}

/// `setenv [tag] KEY [VALUE]`: edit the scratch settings' env block (no VALUE removes it).
pub fn cmd_setenv(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let key = a.first().ok_or_else(|| Fail::die("setenv: need KEY"))?;
    setenv(&run, key, a.get(1).map(String::as_str))
}

/// Set (or, with no value, remove) `key` in the scratch settings' env block; the block.
///
/// # Errors
/// The settings could not be read, parsed or written.
pub fn set_env(run: &Run, key: &str, value: Option<&str>) -> Result<String, Fail> {
    let p = run.settings();
    let text = read_scratch(&p)?;
    let mut d = J::parse(&text).map_err(Fail::die)?;
    let env = d.entry_obj("env");
    match value {
        Some(v) => env.set(key, J::s(v)),
        None => env.remove(key),
    }
    let shown = env.py();
    atomic_write(&p, d.dump(SETTINGS_STYLE).as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    Ok(shown)
}

fn setenv(run: &Run, key: &str, value: Option<&str>) -> Res {
    println!("{}", set_env(run, key, value)?);
    Ok(0)
}

/// `survey [tag] on|off [--reread]`: `CLAUDE_FORCE_DISPLAY_SURVEY` in the running Claude's settings env. With
/// `--reread` the write is made as `selftest` arms the flag: again every 3 s until Claude Code reports re-reading
/// the file ([`crate::settings_witness`]), then the writes and the seconds to that report are printed.
pub fn cmd_survey(args: &[String]) -> Res {
    const KEY: &str = "CLAUDE_FORCE_DISPLAY_SURVEY";
    let (run, a) = lab(args)?;
    let (value, reread) = survey_words(&a)?;
    if !reread {
        return setenv(&run, KEY, value);
    }
    let r = crate::settings_witness::set_env_reread(
        &run,
        KEY,
        value,
        (
            crate::settings_witness::REWRITE_EVERY,
            crate::settings_witness::HANG,
        ),
    )?;
    println!(
        "survey {}: Claude Code reported a re-read {:.2} s after the first write ({} write(s))",
        if value.is_some() { "on" } else { "off" },
        r.after.as_secs_f64(),
        r.writes
    );
    Ok(0)
}

/// `survey`'s words after the tag: the flag's value (`on`: `1`, `off`: none)
/// and whether the write waits for Claude Code's re-read (`--reread`).
///
/// # Errors
/// Anything else.
fn survey_words(a: &[String]) -> Result<(Option<&'static str>, bool), Fail> {
    let words: Vec<&str> = a.iter().map(String::as_str).collect();
    let (value, rest) = match words.split_first() {
        Some((&"on", rest)) => (Some("1"), rest),
        Some((&"off", rest)) => (None, rest),
        _ => return Err(Fail::die("survey: on|off [--reread]")),
    };
    match rest {
        [] => Ok((value, false)),
        ["--reread"] => Ok((value, true)),
        _ => Err(Fail::die("survey: on|off [--reread]")),
    }
}

/// Set `key = value` lines in the `[harness]` table of an `aterm.toml` text
/// (the table is added when there is none; a key already there is replaced).
#[must_use]
pub fn harness_set(text: &str, kvs: &[(String, String)]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let trailing_nl = text.ends_with('\n') || text.is_empty();
    if !lines.iter().any(|l| l.trim_end() == "[harness]") {
        lines.push(String::new());
        lines.push("[harness]".into());
    }
    for (k, v) in kvs {
        let head = lines
            .iter()
            .position(|l| l.trim_end() == "[harness]")
            .unwrap_or(0);
        let end = lines[head + 1..]
            .iter()
            .position(|l| l.starts_with('['))
            .map_or(lines.len(), |i| head + 1 + i);
        let is_key = |l: &str| {
            l.strip_prefix(k.as_str())
                .is_some_and(|r| r.trim_start().starts_with('='))
        };
        let mut found = false;
        for l in &mut lines[head + 1..end] {
            if is_key(l) {
                *l = format!("{k} = {v}");
                found = true;
            }
        }
        if !found {
            // after the table's last non-blank line
            let mut at = end;
            while at > head + 1 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            lines.insert(at, format!("{k} = {v}"));
        }
    }
    let mut out = lines.join("\n");
    if trailing_nl {
        out.push('\n');
    }
    out
}

/// Set `[harness]` keys in the scratch `aterm.toml` (live); the new text.
///
/// # Errors
/// The file could not be read or written.
pub fn set_harness(run: &Run, kvs: &[(String, String)]) -> Result<String, Fail> {
    let p = run.w.join("cfg/aterm/aterm.toml");
    let text = read_scratch(&p)?;
    let t = harness_set(&text, kvs);
    atomic_write(&p, t.as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    Ok(t)
}

/// A `harness` value as the scratch `aterm.toml` holds it: as typed when it
/// is a TOML value (`false`, `3`, `"none"`, `["a"]`), else a word, written as
/// a TOML string (`none` → `"none"`). Unquoted, such a word makes the file
/// invalid TOML, and aterm reads none of it ("Couldn't read your settings").
/// A value over more than one line is never taken as typed (`true` and a
/// second line `[update]` parse as a document, not one value): it is a word.
///
/// # Errors
/// A value that opens as a string, an array or a table (`"`, `'`, `[`, `{`)
/// and is no TOML value: quoting it would change what was meant.
pub fn toml_value(v: &str) -> Result<String, String> {
    if !v.contains(['\n', '\r'])
        && aterm_toml::from_str::<aterm_toml::Value>(&format!("k = {v}\n")).is_ok()
    {
        return Ok(v.to_string());
    }
    if v.starts_with(['"', '\'', '[', '{']) {
        return Err(format!(
            "harness: '{v}' opens a TOML string, array or table but is none ({LAB} help)"
        ));
    }
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if c.is_control() => s.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => s.push(c),
        }
    }
    s.push('"');
    Ok(s)
}

/// `harness [tag] key=value ...`: set `[harness]` keys in the scratch
/// `aterm.toml` (live), each value a TOML value ([`toml_value`]).
pub fn cmd_harness(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let kvs = a
        .iter()
        .map(|kv| {
            let (k, v) = kv.split_once('=').ok_or_else(|| {
                Fail::die(format!("harness: '{kv}' is not key=value ({LAB} help)"))
            })?;
            Ok((
                k.trim().to_string(),
                toml_value(v.trim()).map_err(Fail::die)?,
            ))
        })
        .collect::<Result<Vec<_>, Fail>>()?;
    println!("{}", set_harness(&run, &kvs)?);
    Ok(0)
}

/// `send [tag] text…`
pub fn cmd_send(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    Ok(Door::new(&run)?.ctl_inherit(&["send", &a.join(" ")])?)
}

/// `enter [tag]`
pub fn cmd_enter(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    Ok(Door::new(&run)?.ctl_inherit(&["key", "enter"])?)
}

/// Whether `text` stands in the composer of the agent in the tab now;
/// `None` when the screen could not be read (never taken for "sent").
fn standing(door: &Door<'_>, text: &str) -> Option<bool> {
    let r = door.ctl(&["text"]).ok()?;
    if r.code != 0 {
        return None;
    }
    Some(crate::screen::composer_row(door.run().agent(), &r.stdout).as_deref() == Some(text))
}

/// A turn: TEXT typed, then Enter once TEXT stands in the composer (0.3 s
/// after the typing at least, so the agent reads a typed prompt and an Enter,
/// not one pasted burst whose Enter is a newline), and it is taken once the
/// composer has let it go. An Enter lost at an agent's start leaves the text
/// standing (codex-cli 0.163.0 drew its composer and kept `› hi` with no
/// request made, in 1 of 5 runs): Enter again, once, and only while the text
/// stands there, where Enter sends it. Whether it was taken: never when the
/// text did not stand there (no Enter pressed), still stands after the second
/// Enter, or the screen could not be read, so a caller never goes on as if a
/// turn was taken.
///
/// # Errors
/// A door rule refused it, or the run was interrupted.
pub fn turn(door: &Door<'_>, text: &str) -> Result<bool, Fail> {
    use std::time::Duration;
    let tag = door.run().tag.clone();
    if door.ctl(&["send", text])?.code != 0 {
        return Ok(false);
    }
    crate::stop::sleep(Duration::from_millis(300))?;
    let mut stood = false;
    for _ in 0..20 {
        if standing(door, text) == Some(true) {
            stood = true;
            break;
        }
        crate::stop::sleep(Duration::from_millis(250))?;
    }
    if !stood {
        eprintln!(
            "aterm-lab: turn: '{text}' does not stand in the composer of lab '{tag}' ({LAB} text {tag}): Enter not pressed"
        );
        return Ok(false);
    }
    for attempt in 1..=2 {
        if door.ctl(&["key", "enter"])?.code != 0 {
            return Ok(false);
        }
        let mut now = Some(true);
        for _ in 0..20 {
            crate::stop::sleep(Duration::from_millis(250))?;
            now = standing(door, text);
            if now == Some(false) {
                return Ok(true);
            }
        }
        if now.is_none() {
            eprintln!(
                "aterm-lab: turn: the screen of lab '{tag}' could not be read after Enter: '{text}' is not known to be sent"
            );
            return Ok(false);
        }
        if attempt == 1 {
            eprintln!(
                "aterm-lab: turn: '{text}' still stands in the composer of lab '{tag}' 5 s after Enter: Enter again"
            );
        }
    }
    eprintln!(
        "aterm-lab: turn: '{text}' still stands in the composer of lab '{tag}' after two Enters ({LAB} text {tag})"
    );
    Ok(false)
}

/// `turn [tag] TEXT` (one row): exit 1 unless the composer took the prompt.
pub fn cmd_turn(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    Ok(if turn(&Door::new(&run)?, &a.join(" "))? {
        0
    } else {
        1
    })
}

/// `key [tag] <key>…`, `text [tag] [args]`, `ctl [tag] <verb>…`: the client, on this terminal.
pub fn cmd_passthrough(verb: Option<&str>, args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let mut v: Vec<&str> = verb.into_iter().collect();
    v.extend(a.iter().map(String::as_str));
    Ok(Door::new(&run)?.ctl_inherit(&v)?)
}

/// `image [tag] [name]`
pub fn cmd_image(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let name = a
        .first()
        .cloned()
        .unwrap_or_else(|| format!("lab-{}.png", hms(now_secs()).replace(':', "")));
    Ok(Door::new(&run)?.ctl_inherit(&["image", &name])?)
}

/// `status [tag]`: `ls` and `status`.
pub fn cmd_status(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    let d = Door::new(&run)?;
    d.ctl_inherit(&["ls"])?;
    Ok(d.ctl_inherit(&["status"])?)
}

/// The first tab's session id (`ls`, third word of the first row).
#[must_use]
pub fn sid(door: &Door<'_>) -> String {
    door.out(&["ls"])
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(2))
        .unwrap_or("")
        .to_string()
}

/// `sid [tag]`
pub fn cmd_sid(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    println!("{}", sid(&Door::new(&run)?));
    Ok(0)
}

/// `phase [tag]`: what the hosted loop's reader says of the tab.
pub fn cmd_phase(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    let r = Door::new(&run)?.drive(&["phase"])?;
    print!("{}", r.stdout);
    eprint!("{}", r.stderr);
    Ok(r.code)
}

/// `stublog [tag] [n]`: the stub's last n log lines (40).
pub fn cmd_stublog(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let n: usize = a.first().and_then(|v| v.parse().ok()).unwrap_or(40);
    let text = scratch_text(&run.w.join("stub.log"));
    let lines: Vec<&str> = text.lines().collect();
    for l in &lines[lines.len().saturating_sub(n)..] {
        println!("{l}");
    }
    Ok(0)
}

/// `security [tag]`: the keychain tripwire's log.
pub fn cmd_security(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    let p = run.w.join("security.log");
    println!(
        "# {} (the keychain tripwire: reads and deletes are answered 'not found', writes 'ok' and dropped)",
        p.display()
    );
    print!("{}", scratch_text(&p));
    Ok(0)
}

/// `paths`
pub fn cmd_paths() -> Res {
    let p = paths();
    println!("LAB_STATE={}", p.state.display());
    // the aterm a NEW lab tests; a lab that is up runs the one its env.sh recorded at up
    println!(
        "LAB_ATERM={}",
        p.aterm
            .as_ref()
            .map(|a| a.display().to_string())
            .unwrap_or_default()
    );
    Ok(0)
}

/// `wait [tag] <egrep> [secs]`: until a row of the screen matches (60 s); 1 when none did.
pub fn cmd_wait(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let pat = a.first().ok_or_else(|| Fail::die("wait: need a pattern"))?;
    let secs: u64 = a.get(1).and_then(|v| v.parse().ok()).unwrap_or(60);
    if Door::new(&run)?.await_match(pat, secs)? {
        Ok(0)
    } else {
        eprintln!("wait: '{pat}' did not show within {secs}s");
        Ok(1)
    }
}

/// One harness journal record as `journal` prints it.
#[must_use]
pub fn journal_line(line: &str) -> String {
    let Ok(r) = J::parse(line) else {
        return line.trim_end().to_string();
    };
    let t = r.get("t").and_then(J::as_f64).unwrap_or(0.0) / 1000.0;
    let s = |k: &str, d: &str| r.get(k).and_then(J::as_str).unwrap_or(d).to_string();
    // whole milliseconds of the stamp, as the journal's own clock writes them
    #[allow(clippy::cast_possible_truncation)]
    let secs = t.floor() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let ms = ((t * 1000.0) as i64).rem_euclid(1000);
    format!(
        "{}.{ms:03} {:<10} {:<12} {}",
        hms(secs),
        s("kind", "?"),
        s("phase", "-"),
        s("line", "")
    )
}

/// `journal [tag]`: the harness's own record of what it decided for the tab, and its ledger.
pub fn cmd_journal(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    let door = Door::new(&run)?;
    let sid = sid(&door);
    let f = run
        .state()
        .join("drive")
        .join(format!("{sid}.journal.jsonl"));
    println!("# journal {}", f.display());
    match crate::sys::scratch_read(&f) {
        Ok(Some(b)) => {
            for l in String::from_utf8_lossy(&b).lines() {
                println!("{}", journal_line(l));
            }
        }
        Ok(None) => println!(
            "(no journal yet: the harness writes it once it decides something for the tab)"
        ),
        Err(e) => println!("({e})"),
    }
    println!("# harness ledger");
    let ledger = door.harness_ledger(&sid)?;
    for l in ledger.lines().take(60) {
        println!("{l}");
    }
    Ok(0)
}

/// The pids of a lab's processes that live: everything in its jail (the
/// instance's tree, the agents, codex-runs) and the stub.
fn live_pids(run: &Run) -> Vec<String> {
    crate::procs::lab_tree(run)
        .into_iter()
        .chain([run.stub_pid])
        .filter(|&p| alive(p))
        .map(|p| p.to_string())
        .collect()
}

/// `lsof -nP -i` of a lab's own processes: the text, or why there is none.
///
/// # Errors
/// No process of the lab lives.
pub fn lsof_text(run: &Run) -> Result<String, Fail> {
    let pids = live_pids(run);
    if pids.is_empty() {
        return Err(Fail::die(format!(
            "no process of lab '{}' is alive",
            run.tag
        )));
    }
    let out = aterm_uds::spawnfd::command("/usr/sbin/lsof")
        .args(["-nP", "-i", "-a", "-p", &pids.join(",")])
        .output()
        .map_err(|e| Fail::die(format!("lsof: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `lsof [tag]`: the network sockets of THIS lab's processes only (instance,
/// shells, agents, codex-runs, stub); never a machine-wide list.
pub fn cmd_lsof(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    print!("{}", lsof_text(&run)?);
    Ok(0)
}

/// `egress [tag] [minutes]`: what the lab's processes did on the network, and
/// what the unified log kept of the jail's refusals.
pub fn cmd_egress(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    print!(
        "{}",
        egress_text(&run, a.first().map_or("30", String::as_str))
    );
    Ok(0)
}

/// The unified log's sandbox refusal reports: the kernel's (sender
/// `Sandbox`) and sandboxd's, which on macOS 27 carries them (`Sandbox:
/// codex(<pid>) deny(1) mach-lookup …`).
pub const SANDBOX_LOG: &str =
    "sender == \"Sandbox\" OR subsystem == \"com.apple.sandbox.reporting\"";

/// The unified log's sandbox reports of the last `minutes`.
#[must_use]
pub fn sandbox_log(minutes: &str) -> String {
    aterm_uds::spawnfd::command("/usr/bin/log")
        .args([
            "show",
            "--last",
            &format!("{minutes}m"),
            "--info",
            "--predicate",
            SANDBOX_LOG,
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// The `<what>` of a `Sandbox: <cmd>(<pid>) deny…` report whose pid is one of
/// `pids`, as `<time> <what>`; `None` for any other line.
#[must_use]
pub fn refusal_of(line: &str, pids: &[String]) -> Option<String> {
    let i = line.find("Sandbox: ")?;
    let rest = &line[i + 9..];
    let open = rest.find('(')?;
    let close = rest[open..].find(") deny")?;
    if rest[..open].contains(' ') || !pids.iter().any(|p| *p == rest[open + 1..open + close]) {
        return None;
    }
    // `<date> <time>-<zone> … Sandbox: <what>` → `<time> <what>`
    let time = line
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.split('-').next())
        .unwrap_or("");
    Some(format!("{time} {rest}"))
}

/// The `egress` report: the witness's sockets, the external ones, the proxy's
/// refusals and what the unified log kept of the jail's refusals naming a
/// process of this lab. On macOS 27.0.1 the log kept mach-lookup,
/// process-exec and system-info refusals and none of a refused connect or of
/// a file refusal: no witness of network attempts, and its silence shows nothing.
#[must_use]
pub fn egress_text(run: &Run, minutes: &str) -> String {
    let mut s = String::from(
        "# sockets seen (sampled every second by the egress witness; cmd[pid] proto kind endpoint):\n",
    );
    let read = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    for l in read(&run.run.join("sockets.log")).lines() {
        s.push_str(&format!("  {l}\n"));
    }
    let egress = read(&run.run.join("egress.log"));
    s.push_str(&format!(
        "# EXTERNAL endpoints seen: {}\n",
        egress.lines().filter(|l| l.contains("EXTERNAL")).count()
    ));
    for l in egress.lines() {
        s.push_str(&format!("  {l}\n"));
    }
    s.push_str("# the stub's black-hole proxy refused:\n");
    for l in scratch_text(&run.w.join("stub.log"))
        .lines()
        .filter(|l| l.contains("PROXY refused"))
    {
        s.push_str(&format!("  {l}\n"));
    }
    s.push_str(&format!(
        "# kernel sandbox denials (last {minutes}m) naming a process of this lab, codex-run's among them (what the unified log \
         kept: on macOS 27 never a refused connect or file access, so empty shows nothing):\n"
    ));
    let pids: Vec<String> = crate::procs::lab_pids_ever(run)
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for l in sandbox_log(minutes).lines() {
        let Some(what) = refusal_of(l, &pids) else {
            continue;
        };
        let line: String = format!("  {what}").chars().take(220).collect();
        match counts.iter_mut().find(|(k, _)| *k == line) {
            Some(c) => c.1 += 1,
            None => counts.push((line, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (line, n) in counts.into_iter().take(40) {
        s.push_str(&format!("{n:>7} {line}\n"));
    }
    s
}

/// The one-line `# …` header of a fixture, in the repo's shape, for this lab
/// and the agent its tab runs.
#[must_use]
pub fn fixture_header(run: &Run, door: &Door<'_>, what: &str) -> String {
    let av = run
        .need_aterm()
        .ok()
        .map(|a| {
            aterm_uds::spawnfd::command(a)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("xcrun_nocache", "1")
                .arg("--version")
                .output()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("?")
                        .to_string()
                })
                .unwrap_or_else(|_| "?".into())
        })
        .unwrap_or_else(|| "?".into());
    // the grid as it is now (a `ctl resize` changes it), and the login the launcher gave the agent
    let size = door
        .out(&["dims"])
        .lines()
        .find_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            (w.first() == Some(&"OK") && w.len() >= 3).then(|| format!("{}x{}", w[2], w[1]))
        })
        .unwrap_or_else(|| format!("{}x{}", run.cols, run.rows));
    let day = crate::util::ymd(now_secs());
    if run.agent() == "muse" {
        return format!(
            "# muse {} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running the \
             real Muse Code, launched by path with a scratch `HOME`, a fake `META_API_KEY` and `--base-url \
             http://127.0.0.1:<port>` (a local stub of Meta's model API, crates/aterm-lab) · {what}",
            run.muse_ver
        );
    }
    if matches!(run.agent(), "gemini" | "agy") {
        let (name, ver, how) = if run.agent() == "gemini" {
            (
                "gemini-cli",
                &run.gemini_ver,
                "the real Gemini CLI, launched by path with a scratch `HOME` whose settings select the API-key auth,",
            )
        } else {
            (
                "agy",
                &run.agy_ver,
                "the real Antigravity CLI, launched by path with a scratch `HOME` whose settings name the Gemini API \
                 provider, its Google hosts black-holed,",
            )
        };
        return format!(
            "# {name} {ver} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running \
             {how} a fake `GEMINI_API_KEY` and `GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:<port>` (a local stub of the \
             Gemini API, crates/aterm-lab) · {what}"
        );
    }
    if run.agent() == "codex" {
        let how = match run.codex_mode.as_str() {
            "signed-out" => "the default OpenAI provider, no auth, egress black-holed",
            "chatgpt" => {
                "a composed fake ChatGPT `auth.json` (user@example.com) and the lab provider \
                 `base_url=http://127.0.0.1:<port>/v1` with `requires_openai_auth`"
            }
            _ => {
                "the lab provider `base_url=http://127.0.0.1:<port>/v1`, `wire_api = \"responses\"`, a fake `env_key`"
            }
        };
        return format!(
            "# codex-cli {} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running the \
             real Codex, launched by path with a scratch `CODEX_HOME`, {how} (a local stub of the Responses API, \
             crates/aterm-lab) · {what}",
            run.codex_ver
        );
    }
    let launcher = fs::read_to_string(run.run.join("claude.sh")).unwrap_or_default();
    let login = if launcher.contains("CLAUDE_CODE_OAUTH_TOKEN") {
        "a fake claude.ai OAuth token (`claude --subscriber`)"
    } else {
        "a fake `apiKeyHelper` key"
    };
    format!(
        "# claude-code {} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running the real \
         Claude Code, launched by path with a scratch `CLAUDE_CONFIG_DIR`, {login} and \
         `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` (a local stub of the API, crates/aterm-lab) · {what}",
        run.claude_ver
    )
}

/// `header [tag] "<what>"`
pub fn cmd_header(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    println!(
        "{}",
        fixture_header(
            &run,
            &Door::new(&run)?,
            a.first().map_or("", String::as_str)
        )
    );
    Ok(0)
}

/// Write `$LAB_STATE/captures/claude-<version>-<name>.txt` (header, then the
/// rows without trailing blank ones) and its PNG; their paths.
///
/// # Errors
/// The captures directory could not be made or written.
pub fn write_fixture_rows(
    run: &Run,
    door: &Door<'_>,
    name: &str,
    what: &str,
    rows: &[String],
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Fail> {
    write_fixture_rows_in(run, door, &paths().state.join("captures"), name, what, rows)
}

/// [`write_fixture_rows`] into `dir`.
///
/// # Errors
/// The directory could not be made or written.
pub fn write_fixture_rows_in(
    run: &Run,
    door: &Door<'_>,
    dir: &Path,
    name: &str,
    what: &str,
    rows: &[String],
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Fail> {
    fs::create_dir_all(dir)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", dir.display())))?;
    let full = format!("{}-{name}", run.fixture_prefix());
    let mut rows = rows.to_vec();
    while rows.last().is_some_and(|r| r.trim().is_empty()) {
        rows.pop();
    }
    let out = dir.join(format!("{full}.txt"));
    let body = format!("{}\n{}\n", fixture_header(run, door, what), rows.join("\n"));
    fs::write(&out, body).map_err(|e| Fail::die(format!("cannot write {}: {e}", out.display())))?;
    Ok((out, png(door, dir, &full)))
}

/// The frame as a PNG next to the fixtures, or `None`. The instance writes it
/// into the socket dir's `images/`, which jailed code may write: a link renamed
/// over it the moment it shows would make a plain copy carry what the link
/// names (`~/.codex/auth.json`, say) into the fixtures. It is taken only from
/// this lab's `images/`, read through no link, only as a regular file, and
/// kept by a rename ([`crate::sys::put`]).
pub fn png(door: &Door<'_>, dir: &Path, full: &str) -> Option<std::path::PathBuf> {
    let name = format!("{full}.png");
    let r = door.out(&["image", &name]);
    let w: Vec<&str> = r.split_whitespace().collect();
    let src = (w.len() >= 4 && w[0] == "OK").then(|| w[3])?;
    let images = crate::paths::physical(Path::new(&door.run().sockdir)).join("images");
    if Path::new(src) != images.join(&name) {
        return None;
    }
    let dst = crate::paths::physical(dir).join(&name);
    crate::sys::put(&dst, 0o644, &crate::sys::PutSrc::File(Path::new(src)))
        .ok()
        .map(|()| dst)
}

/// The screen as it is NOW, in the repo's fixture shape, as
/// `$LAB_STATE/captures/<agent>-<version>-<name>.txt` (+ `.png`): the text's and the PNG's paths.
///
/// # Errors
/// The screen could not be read, or the capture could not be written.
pub fn capture(
    run: &Run,
    name: &str,
    what: &str,
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Fail> {
    let door = Door::new(run)?;
    let body = door.ctl(&["text"])?;
    if body.code != 0 {
        return Err(Fail::die("ctl text failed"));
    }
    // the rows exactly as `ctl text` printed them (blank tail rows included, as the screen holds them)
    let full = format!("{}-{name}", run.fixture_prefix());
    let out = paths().state.join("captures").join(format!("{full}.txt"));
    fs::create_dir_all(paths().state.join("captures")).map_err(|e| Fail::die(e.to_string()))?;
    let text = format!(
        "{}\n{}\n",
        fixture_header(run, &door, what),
        body.stdout.trim_end_matches('\n')
    );
    fs::write(&out, &text).map_err(|e| Fail::die(e.to_string()))?;
    let png = png(&door, &paths().state.join("captures"), &full);
    // PII guard: the fixture must not carry the owner's name, host or home
    let host = aterm_uds::spawnfd::command("/bin/hostname")
        .arg("-s")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    for bad in pii_found(paths(), &host, &text) {
        eprintln!(
            "aterm-lab: WARNING: {} contains '{bad}' -- redact before it goes near the repo",
            out.display()
        );
    }
    println!(
        "{} ({} lines){}",
        out.display(),
        text.lines().count(),
        png.as_ref()
            .map(|p| format!("  +  {}", p.display()))
            .unwrap_or_default()
    );
    Ok((out, png))
}

/// What of the owner's a capture's `text` carries, by the PII guard: the
/// login name, the host's short name, and every home the lab guards (the
/// account's, whatever `HOME` says, and the caller's `HOME` when it names
/// another: [`crate::paths::Paths::guarded_homes`]).
fn pii_found(p: &crate::paths::Paths, host: &str, text: &str) -> Vec<String> {
    [p.user.clone(), host.to_string()]
        .into_iter()
        .chain(p.guarded_homes())
        .filter(|bad| !bad.is_empty() && text.contains(bad.as_str()))
        .collect()
}

/// `capture [tag] <name> "<what>"`: the screen as it is NOW, in the repo's fixture shape.
pub fn cmd_capture(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let name = a.first().ok_or_else(|| Fail::die("capture: need a name"))?;
    capture(&run, name, a.get(1).map_or("", String::as_str))?;
    Ok(0)
}

/// Whether `v` is a three-number version (`2.1.300`).
fn semver_like(v: &str) -> bool {
    let p: Vec<&str> = v.split('.').collect();
    p.len() >= 3
        && p[..3]
            .iter()
            .all(|x| !x.is_empty() && x.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

/// The leading numbers of a version, for ordering.
#[must_use]
pub fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .take(3)
        .filter_map(|x| x.parse().ok())
        .collect()
}

/// `twin [tag] <claude-bin>|--crashing [ver]`: present a build as a NEWER managed
/// Claude Code the way atpkg lays one (the build at
/// `store/claude/<build>/bin/claude` under the scratch HOME's package prefix,
/// the `agents/claude` twin whose `exec '<path>' "$@"` line names it, and the
/// activation notice renamed into place, which wakes the instance's live
/// upgrade). The build is a clone made from OUTSIDE the jail (a build in the
/// owner's store is denied to the jail). Every file is written by
/// [`crate::sys::put`], which follows no link on its path: the package store
/// sits under `home/Library`, which the instance makes and a jailed process
/// can replace with a link (to the owner's `~/Library`, say, whose store and
/// activation notice the owner's live aterm acts on). The build's `--version`
/// runs only inside the jail, from a clone in the run directory (which the
/// jail reads and cannot write). `--crashing` presents a stand-in that names
/// its version and exits 1 at any other launch.
pub fn cmd_twin(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    twin(&run, &a)
}

/// [`cmd_twin`] on a loaded lab.
///
/// # Errors
/// A bad argument, a build that names no version, or a write refused (a link on its way).
pub fn twin(run: &Run, a: &[String]) -> Res {
    use crate::sys::{PutSrc, put};
    let src = a.first().ok_or_else(|| {
        Fail::die(format!("twin: need the Claude Code binary to present as the newer managed build, or --crashing ({LAB} help)"))
    })?;
    let pkg = run.w.join("home/Library/Application Support/aterm/pkg");
    let (ver, dest) = if src == "--crashing" {
        let given = a.get(1).cloned().unwrap_or_else(|| "99.0.0".into());
        if clean_ver(&given) != given || !semver_like(&given) {
            return Err(Fail::die(format!(
                "twin: --crashing takes a version (99.0.0), not '{given}'"
            )));
        }
        let ver = given;
        let dest = pkg.join(format!("store/claude/lab-{ver}-crashing/bin/claude"));
        let script = format!(
            "#!/bin/sh\n# aterm-lab twin --crashing: a stand-in build that exits at once (crates/aterm-lab)\ncase \"$1\" in --version) echo \"{ver} (Claude Code)\"; exit 0 ;; esac\necho \"lab stand-in {ver}: this build exits at once\" >&2\nexit 1\n"
        );
        put(&dest, 0o755, &PutSrc::Bytes(script.as_bytes()))
            .map_err(|e| Fail::die(format!("twin: cannot write {}: {e}", dest.display())))?;
        (ver, dest)
    } else {
        let src = crate::paths::from_caller(src);
        if !crate::sys::executable(&src) || !Path::new(&src).is_file() {
            return Err(Fail::die(format!("twin: cannot execute {src}")));
        }
        let src = fs::canonicalize(&src).map_err(|e| Fail::die(e.to_string()))?;
        let probe = run.run.join("twin-src/claude");
        let _ = fs::remove_dir_all(run.run.join("twin-src"));
        put(&probe, 0o755, &PutSrc::File(&src))
            .map_err(|e| Fail::die(format!("twin: cannot clone {}: {e}", src.display())))?;
        let ver = crate::lab::jailed_version(run, &probe.to_string_lossy(), |l| {
            l.split_whitespace().next()
        });
        let _ = fs::remove_dir_all(run.run.join("twin-src"));
        if !semver_like(&ver) {
            return Err(Fail::die(format!(
                "twin: {} --version named no version",
                src.display()
            )));
        }
        let dest = pkg.join(format!("store/claude/lab-{ver}/bin/claude"));
        put(&dest, 0o755, &PutSrc::File(&src)).map_err(|e| {
            Fail::die(format!(
                "twin: cannot copy {} into {}: {e}",
                src.display(),
                dest.display()
            ))
        })?;
        (ver, dest)
    };
    let q = dest.to_string_lossy().replace('\'', "'\\''");
    let shim = format!(
        "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\n# aterm-lab twin: a newer managed build presented the way atpkg lays one (crates/aterm-lab)\nexport DISABLE_AUTOUPDATER='1'\nexec '{q}' \"$@\"\n"
    );
    let twin = pkg.join("agents/claude");
    put(&twin, 0o755, &PutSrc::Bytes(shim.as_bytes()))
        .map_err(|e| Fail::die(format!("twin: cannot write the twin: {e}")))?;
    put(
        &pkg.join("activation-notices/claude"),
        0o644,
        &PutSrc::Bytes(b"1\n"),
    )
    .map_err(|e| Fail::die(format!("twin: cannot publish the activation notice: {e}")))?;
    println!(
        "twin: managed Claude Code {ver} presented in lab '{}' (the tab runs {}): {} -> {}",
        run.tag,
        run.claude_ver,
        twin.display(),
        dest.display()
    );
    if version_key(&ver) <= version_key(&run.claude_ver) {
        eprintln!(
            "aterm-lab: twin: {ver} is not newer than the build the lab runs ({}): nothing is behind it, and the upgrade moves nothing",
            run.claude_ver
        );
    }
    Ok(0)
}

/// The selftests' check of [`twin`]: from INSIDE the jail, `home/Library` is
/// moved aside and made a link to a decoy directory (as a jailed process could
/// make it one to the owner's `~/Library`: a link needs no access to its
/// target); `twin --crashing` must refuse and the decoy stay empty. The decoy
/// is in the run directory, which the jail cannot write, so only the lab could
/// fill it. The link and the move are undone from inside the jail as well:
/// the instance and the agent still run there and may write `home/`, so a link
/// one of them put at `home/Library` between the lab's removal and its move
/// would take the move, unjailed, into whatever it names. Whether it held.
#[must_use]
pub fn twin_link_refused(run: &Run) -> bool {
    let lib = run.w.join("home/Library");
    let lib_s = lib.to_string_lossy().into_owned();
    let Ok(decoy) = crate::sys::make_temp_dir(&format!("{}/twin-decoy.XXXX", run.run.display()))
    else {
        return false;
    };
    let sh = |script: &str, args: &[&str]| {
        let mut argv = vec!["/bin/sh", "-c", script, "sh"];
        argv.extend_from_slice(args);
        let _ = crate::procs::jailed(run, &run.w, &[], &argv, crate::procs::JailOut::Capture);
    };
    sh(
        "[ ! -e \"$1\" ] || mv \"$1\" \"$1.lab-moved\"; ln -s \"$2\" \"$1\"",
        &[&lib_s, &decoy],
    );
    let held = fs::symlink_metadata(&lib).is_ok_and(|m| m.file_type().is_symlink())
        && twin(run, &["--crashing".to_string()]).is_err()
        && fs::read_dir(&decoy).is_ok_and(|mut rd| rd.next().is_none());
    sh(
        "[ ! -L \"$1\" ] || rm -f \"$1\"; [ ! -e \"$1.lab-moved\" ] || mv \"$1.lab-moved\" \"$1\"",
        &[&lib_s],
    );
    let _ = fs::remove_dir_all(&decoy);
    held
}

/// The tripwire log of a lab, masked, one line per entry (selftest's report).
#[must_use]
pub fn security_masked(run: &Run) -> String {
    scratch_text(&run.w.join("security.log"))
        .lines()
        .map(|l| format!("  {}\n", mask_account(l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE CAPTURE'S PII GUARD LOOKS FOR EVERY HOME THE LAB GUARDS: under a
    /// scratch `HOME` (a fake account home handed in through
    /// [`crate::paths::Paths::resolve_with`]) a capture naming the account's
    /// home or the scratch one is flagged, as is the login name. NEGATIVE
    /// CONTROLS: a capture naming none is clean, and the facts of a run from
    /// the account's own `HOME` do not take the scratch home for the owner's.
    #[test]
    fn the_capture_pii_guard_looks_for_the_account_home_and_a_scratch_home() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-pii-")
            .tempdir()
            .unwrap();
        let real = fs::canonicalize(top.path()).unwrap();
        let (account, scratch) = (real.join("account"), real.join("scratch"));
        for d in [&account, &scratch] {
            fs::create_dir_all(d).unwrap();
        }
        let (a, s) = (account.display().to_string(), scratch.display().to_string());
        let mut p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.into()));
        p.user = "lab-owner-name".into();
        let host = "lab-host-name";
        assert_eq!(
            pii_found(&p, host, &format!("cwd {a}/x")),
            std::slice::from_ref(&a)
        );
        assert_eq!(
            pii_found(&p, host, &format!("cwd {s}/x")),
            std::slice::from_ref(&s)
        );
        assert_eq!(
            pii_found(&p, host, "lab-owner-name@lab-host-name"),
            ["lab-owner-name", "lab-host-name"]
        );
        // NEGATIVE CONTROLS
        assert!(pii_found(&p, host, "cwd ~/x").is_empty());
        let mut own =
            crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
        own.user = "lab-owner-name".into();
        assert!(pii_found(&own, host, &format!("cwd {s}/x")).is_empty());
    }

    /// `survey on|off [--reread]`: `on` sets the flag, `off` removes it, and
    /// `--reread` after either waits for Claude Code's re-read. NEGATIVE
    /// CONTROLS: no word, another word, `--reread` alone or first, and a word
    /// past the flag are refused.
    #[test]
    fn survey_takes_on_or_off_and_reread_after_it() {
        let w = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
        for (words, want) in [
            ("on", (Some("1"), false)),
            ("off", (None, false)),
            ("on --reread", (Some("1"), true)),
            ("off --reread", (None, true)),
        ] {
            assert_eq!(survey_words(&w(words)).ok(), Some(want), "{words}");
        }
        for bad in [
            "",
            "yes",
            "--reread",
            "--reread on",
            "on --reread x",
            "on off",
        ] {
            let e = survey_words(&w(bad)).expect_err(bad);
            assert_eq!(e.msg.as_deref(), Some("survey: on|off [--reread]"), "{bad}");
        }
    }

    #[test]
    fn harness_keys_are_set_in_their_table_and_replaced_when_there() {
        let base = "[update]\nenabled = false\n[harness]\nprobe_api = false\n\n[render]\nx = 1\n";
        let kv = |k: &str, v: &str| vec![(k.to_string(), v.to_string())];
        let t = harness_set(base, &kv("continue", "false"));
        assert_eq!(
            t,
            "[update]\nenabled = false\n[harness]\nprobe_api = false\ncontinue = false\n\n[render]\nx = 1\n"
        );
        let t = harness_set(&t, &kv("probe_api", "true"));
        assert!(
            t.contains("[harness]\nprobe_api = true\ncontinue = false\n"),
            "{t}"
        );
        assert_eq!(
            harness_set("a = 1\n", &kv("k", "\"v\"")),
            "a = 1\n\n[harness]\nk = \"v\"\n"
        );
    }

    /// `harness TAG approve=none` writes `approve = "none"`, a file aterm can
    /// read; a TOML value is written as typed, and a word with a quote or a
    /// backslash is escaped. NEGATIVE CONTROLS: the unquoted line (what the
    /// verb wrote before) is no TOML, and a value that opens an array or a
    /// string but is none is refused rather than quoted into a string.
    #[test]
    fn a_harness_value_is_written_as_toml() {
        let parses = |t: &str| aterm_toml::from_str::<aterm_toml::Value>(t).is_ok();
        let base = "[update]\nenabled = false\n";
        let v = toml_value("none").unwrap();
        assert_eq!(v, "\"none\"");
        let t = harness_set(base, &[("approve".to_string(), v)]);
        assert!(t.ends_with("[harness]\napprove = \"none\"\n"), "{t}");
        assert!(parses(&t), "{t}");
        let raw = harness_set(base, &[("approve".to_string(), "none".to_string())]);
        assert!(!parses(&raw), "the unquoted word is no TOML: {raw}");
        for typed in ["false", "3", "1.5", "\"safe\"", "'safe'", "[\"a\", \"b\"]"] {
            assert_eq!(toml_value(typed).as_deref(), Ok(typed), "{typed}");
        }
        assert_eq!(toml_value("a\"b\\c").unwrap(), r#""a\"b\\c""#);
        assert!(parses(&format!("k = {}\n", toml_value("a\"b\\c").unwrap())));
        for bad in ["[1,", "\"open", "{a = }"] {
            assert!(toml_value(bad).is_err(), "{bad}");
        }
        // A second line is never written as typed: one key, its value a string.
        let two = toml_value("true\n[update]\nenabled = true").unwrap();
        assert_eq!(two, r#""true\u000A[update]\u000Aenabled = true""#);
        let t = harness_set(base, &[("approve".to_string(), two)]);
        assert!(parses(&t), "{t}");
        assert!(t.contains("[update]\nenabled = false\n"), "{t}");
        assert!(!t.contains("\nenabled = true"), "{t}");
    }

    #[test]
    fn a_journal_record_prints_as_one_aligned_line() {
        let l = journal_line(
            r#"{"t":1792091823464,"kind":"dismissed","phase":"survey","line":"DISMISSED survey seq=1460"}"#,
        );
        assert!(
            l.ends_with(".464 dismissed  survey       DISMISSED survey seq=1460"),
            "{l}"
        );
        assert_eq!(journal_line("not json"), "not json");
    }

    /// `headers` takes the two blocks alone, objects each; nothing is `{}`.
    #[test]
    fn headers_takes_the_on200_and_on429_blocks_alone() {
        assert_eq!(rate_blocks(None).unwrap().py(), "{}");
        assert_eq!(
            rate_blocks(Some(r#"{"on200":{"a":"1"},"on429":{}}"#))
                .unwrap()
                .py(),
            r#"{"on200": {"a": "1"}, "on429": {}}"#
        );
        for bad in [r#"{"x-codex-a":"1"}"#, r#"{"on200":"1"}"#, "[1]", "nope"] {
            assert!(rate_blocks(Some(bad)).is_err(), "{bad}");
        }
    }

    /// Both verbs write the one `--headers-file` through [`write_headers`],
    /// and each leaves the other's entries: `headers` then `ratelimits` on
    /// one file reads back the block and the flat entry. NEGATIVE CONTROL:
    /// the second verb's entries written over the whole file, as each verb
    /// wrote it before, read back without the first's.
    #[test]
    fn both_headers_verbs_writes_stand_in_the_one_file() {
        let d = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-verbs-headers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("headers.json");
        let read = || {
            J::parse(&std::fs::read_to_string(&p).unwrap())
                .unwrap()
                .py()
        };
        let flat = r#"{"x-codex-primary-used-percent":"76.0"}"#;
        write_headers(&d, J::parse(r#"{"on429":{"a":"1"}}"#).unwrap(), true).unwrap();
        write_headers(&d, J::parse(flat).unwrap(), false).unwrap();
        let both = read();
        atomic_write(&p, flat.as_bytes()).unwrap();
        let over = read();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(
            both,
            r#"{"on429": {"a": "1"}, "x-codex-primary-used-percent": "76.0"}"#
        );
        assert_eq!(over, r#"{"x-codex-primary-used-percent": "76.0"}"#);
    }

    #[test]
    fn a_custom_mode_is_named_what_the_stub_serves_from_its_file() {
        assert_eq!(custom_mode_name("fable-noreset").unwrap(), "fable-noreset");
        let refused = |m: &str| custom_mode_name(m).unwrap_err().msg.unwrap_or_default();
        // a built-in mode's name is answered by that mode, whatever its file holds
        for m in ["ok", "limit", "tool", "bg", "slow", "rate-limit"] {
            assert!(refused(m).contains("is a built-in mode"), "{m}");
        }
        assert!(refused("refresh").contains("token refresh"));
        // a name the stub would read as another
        for m in ["a b", "custom:x", "limit:five_hour:1", "x/../y"] {
            assert!(refused(m).contains("letters, digits"), "{m}");
        }
    }

    #[test]
    fn versions_order_by_their_numbers() {
        assert!(version_key("2.1.300") > version_key("2.1.299"));
        assert!(version_key("99.0.0") > version_key("2.1.300"));
        assert!(semver_like("2.1.999") && !semver_like("x.1.2") && !semver_like("2.1"));
    }
}
