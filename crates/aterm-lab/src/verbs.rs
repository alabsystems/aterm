// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Small verbs on a running lab.

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
use crate::util::{atomic_write, hms, now_ms, now_secs, scratch_text};
use crate::{Fail, Res};

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

/// Read a regular file in the scratch directory through no symlinks.
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

/// Switch the stub's mode (a `bg` mode also allows `Bash(sleep …)` in the scratch settings).
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

/// `mode [tag] [name ['<spec>']]`: read or switch the stub's mode.
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

/// Write the custom mode `m` (`modes/<m>.json`) and switch the stub to it.
/// # Errors
/// `m` is no custom mode's name ([`custom_mode_name`]), `spec` no JSON object, or the write or
/// switch failed.
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

/// Validate `m` as the name of a custom mode served from `modes/<m>.json`.
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

/// `limit [tag] CLAIM SECONDS [PLAN]`: configure a temporary subscription usage limit.
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
    let answer = set_limit(&run, &claim, reset, &plan)?;
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

/// Set the stub's temporary subscription usage limit: the window `claim` (`five_hour`,
/// `seven_day`, …) resetting at `reset` (epoch seconds), `plan` Codex's `plan_type` or empty.
/// # Errors
/// The stub did not take it.
pub fn set_limit(run: &Run, claim: &str, reset: i64, plan: &str) -> Result<String, Fail> {
    let plan_part = if plan.is_empty() {
        String::new()
    } else {
        format!(":{plan}")
    };
    control(
        &run.port,
        &format!("/__mode?m=limit:{claim}:{reset}{plan_part}"),
    )
    .map_err(|e| {
        Fail::die(format!(
            "limit: the stub of lab '{}' did not take the limit ({e})",
            run.tag
        ))
    })
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

/// Write the tool mode's script and switch the stub to that mode.
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

/// Parse `headers`' argument into `{"on200":{…},"on429":{…}}` blocks.
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

/// Write `new`'s entries into `scratch/headers.json` while keeping the other verb's entries.
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

/// `headers [tag] ['<json>']`: set or clear the stub's extra headers per status on `/v1/messages`.
pub fn cmd_headers(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let j = rate_blocks(a.first().map(String::as_str)).map_err(Fail::die)?;
    write_headers(&run.w, j.clone(), true)?;
    println!("{}", j.py());
    Ok(0)
}

/// The stub's `--auth-modes-file` in the scratch: each credential's mode by its label.
pub const AUTH_MODES: &str = "auth-modes.json";

/// Whether `label` is one the stub names a credential by ([`crate::stub::auth_label`]): 1–40
/// characters of `[a-z0-9_:+()-]`.
fn credential_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 40
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_:+()-".contains(&b))
}

/// Answer the credential `label`'s `/v1/messages` turns in `mode` (`off`: in the stub's own
/// mode again), every other credential's as they were ([`crate::stub::auth_mode`]); the file's
/// new text.
/// # Errors
/// `label` is no credential's label, `mode` is empty, or the file could not be written.
pub fn set_auth_mode(run: &Run, label: &str, mode: &str) -> Result<String, Fail> {
    if !credential_label(label) {
        return Err(Fail::die(format!(
            "mode-for: '{label}' is no credential's label (oat:lab-a, key, …)"
        )));
    }
    let clean = crate::stub::clean_mode(mode);
    if mode.trim().is_empty() {
        return Err(Fail::die("mode-for: need a MODE (or off)"));
    }
    let p = run.w.join(AUTH_MODES);
    let mut modes = match J::parse(&scratch_text(&p)) {
        Ok(j @ J::Obj(_)) => j,
        _ => J::Obj(Vec::new()),
    };
    if mode == "off" {
        modes.remove(label);
    } else {
        modes.set(label, J::s(clean));
    }
    let text = modes.py();
    atomic_write(&p, text.as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    Ok(text)
}

/// `mode-for [tag] LABEL MODE|off`: answer one credential's turns in their own mode.
pub fn cmd_mode_for(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let (Some(label), Some(mode)) = (a.first(), a.get(1)) else {
        return Err(Fail::usage(format!(
            "mode-for: need LABEL and MODE ({LAB} help)"
        )));
    };
    println!("{}", set_auth_mode(&run, label, mode)?);
    Ok(0)
}

/// Set the credential `label`'s own `/v1/messages` header blocks (`{"on200":{…},"on429":{…}}`,
/// `{}` clears them) in `scratch/headers.json`, keeping every other entry
/// ([`crate::stub::headers_file_for`]).
/// # Errors
/// `label` is no credential's label, or the file could not be written.
pub fn write_headers_for(w: &Path, label: &str, blocks: J) -> Result<(), Fail> {
    if !credential_label(label) {
        return Err(Fail::die(format!(
            "headers-for: '{label}' is no credential's label (oat:lab-a, key, …)"
        )));
    }
    let p = w.join("headers.json");
    let h = crate::stub::headers_file_for(&scratch_text(&p), label, blocks);
    let text = h.dump(Style {
        indent: Some(1),
        compact: false,
        ascii: true,
    });
    atomic_write(&p, text.as_bytes()).map_err(|e| Fail::die(e.to_string()))
}

/// `headers-for [tag] LABEL ['<json>']`: one credential's own extra headers on `/v1/messages`.
pub fn cmd_headers_for(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let label = a
        .first()
        .ok_or_else(|| Fail::usage(format!("headers-for: need LABEL ({LAB} help)")))?;
    let j = rate_blocks(a.get(1).map(String::as_str)).map_err(Fail::die)?;
    write_headers_for(&run.w, label, j.clone())?;
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

/// `survey [tag] on|off [--reread]`.
pub fn cmd_survey(args: &[String]) -> Res {
    const KEY: &str = "CLAUDE_FORCE_DISPLAY_SURVEY";
    let (run, a) = lab(args)?;
    let (value, reread) = survey_words(&a)?;
    if !reread {
        return setenv(&run, KEY, value);
    }
    let r = crate::settings_witness::set_env_reread(&run, KEY, value)?;
    println!(
        "survey {}: Claude Code reported a re-read {:.2} s after the first write ({} write(s))",
        if value.is_some() { "on" } else { "off" },
        r.after.as_secs_f64(),
        r.writes
    );
    Ok(0)
}

/// `survey`'s words after the tag: the flag's value (`on`: `1`, `off`: none) and whether the
/// write waits for Claude Code's re-read (`--reread`).
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

/// Set `key = value` lines in the `[harness]` table of an `aterm.toml` text.
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
/// # Errors
/// The file could not be read or written.
pub fn set_harness(run: &Run, kvs: &[(String, String)]) -> Result<String, Fail> {
    let p = run.w.join("cfg/aterm/aterm.toml");
    let text = read_scratch(&p)?;
    let t = harness_set(&text, kvs);
    atomic_write(&p, t.as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    Ok(t)
}

/// Format a `harness` value as a TOML value for the scratch `aterm.toml`.
/// # Errors
/// Value opens as a string, array, or table and is not a valid TOML value.
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

/// `harness [tag] key=value ...`: set `[harness]` keys in the scratch `aterm.toml`.
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

/// `send [tag] text…`.
pub fn cmd_send(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    Ok(Door::new(&run)?.ctl_inherit(&["send", &a.join(" ")])?)
}

/// `enter [tag]`.
pub fn cmd_enter(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    Ok(Door::new(&run)?.ctl_inherit(&["key", "enter"])?)
}

fn standing(door: &Door<'_>, text: &str) -> Option<bool> {
    let r = door.ctl(&["text"]).ok()?;
    if r.code != 0 {
        return None;
    }
    Some(crate::screen::composer_row(door.run().agent(), &r.stdout).as_deref() == Some(text))
}

/// Type `text` and press Enter once `text` stands in the composer.
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

/// `image [tag] [name]`.
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

/// `sid [tag]`.
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

/// `paths`.
pub fn cmd_paths() -> Res {
    let p = paths();
    println!("LAB_STATE={}", p.state.display());
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

/// The tab a verb's words name, `@<sid>`, taken out of `args` (the words after the tag): only
/// their first word names one, so a later word (a capture's name or description) may start
/// with `@`. `None` where the first word names none.
/// # Errors
/// A first word `@…` that is no session id.
pub fn take_tab(args: &mut Vec<String>) -> Result<Option<String>, Fail> {
    if !args.first().is_some_and(|w| w.starts_with('@')) {
        return Ok(None);
    }
    let sid = args.remove(0).split_off(1);
    let shaped = !sid.is_empty()
        && sid.len() <= 64
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if shaped {
        Ok(Some(sid))
    } else {
        Err(Fail::usage(format!("'@{sid}' names no session id")))
    }
}

/// Whether the `ls` reply `ls` lists the tab `sid` (the third word of one of its rows, as
/// [`sid`] reads the first).
#[must_use]
pub fn ls_lists(ls: &str, sid: &str) -> bool {
    ls.lines().any(|l| l.split_whitespace().nth(2) == Some(sid))
}

/// The tab `named` if `door`'s lab lists it, else the first tab ([`sid`]) where none is named.
/// # Errors
/// The lab lists no tab `named`.
fn named_or_first(door: &Door<'_>, named: Option<String>) -> Result<String, Fail> {
    let Some(sid) = named else {
        return Ok(sid(door));
    };
    if ls_lists(&door.out(&["ls"]), &sid) {
        Ok(sid)
    } else {
        Err(Fail::usage(format!(
            "lab '{}' lists no tab @{sid} ({} ls {0})",
            door.run().tag,
            crate::LAB
        )))
    }
}

/// `journal [tag] [@sid]`: the harness's own record of what it decided for the tab (the first
/// tab, or the one named), and its ledger.
pub fn cmd_journal(args: &[String]) -> Res {
    let (run, mut a) = lab(args)?;
    let named = take_tab(&mut a)?;
    let door = Door::new(&run)?;
    let sid = named_or_first(&door, named)?;
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

fn live_pids(run: &Run) -> Vec<String> {
    crate::procs::lab_tree(run)
        .into_iter()
        .chain([run.stub_pid])
        .filter(|&p| alive(p))
        .map(|p| p.to_string())
        .collect()
}

/// `lsof -nP -i` of a lab's own processes: the text, or why there is none. lsof reads every
/// descriptor of each process it lists, and a read of a socket its holder closes meanwhile drains
/// it, so a fork that may be mid-spawn, a process exiting and one gone are left out
/// (`aterm_sysprobe::jobs::sockets_readable`) and named on a last `#` line. The kernel's TCP and
/// UDP tables read no descriptor, but show a reader that is no Apple platform binary its own
/// sockets alone.
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
    let (read, left): (Vec<String>, Vec<String>) = pids.into_iter().partition(|p| {
        p.parse::<u32>()
            .is_ok_and(aterm_sysprobe::jobs::sockets_readable)
    });
    let mut text = if read.is_empty() {
        String::new()
    } else {
        let out = aterm_uds::spawnfd::command("/usr/sbin/lsof")
            .args(["-nP", "-i", "-a", "-p", &read.join(",")])
            .output()
            .map_err(|e| Fail::die(format!("lsof: {e}")))?;
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    if !left.is_empty() {
        text.push_str(&format!(
            "# not read now (a fork under 2 s old, exiting or gone): {}\n",
            left.join(",")
        ));
    }
    Ok(text)
}

/// `lsof [tag]`: list network sockets of this lab's processes only.
pub fn cmd_lsof(args: &[String]) -> Res {
    let (run, _) = lab(args)?;
    print!("{}", lsof_text(&run)?);
    Ok(0)
}

/// `egress [tag] [minutes]`: report network activity and sandbox refusals for this lab's processes.
pub fn cmd_egress(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    print!(
        "{}",
        egress_text(&run, a.first().map_or("30", String::as_str))
    );
    Ok(0)
}

/// Unified log predicate for sandbox refusal reports.
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

/// Extract `<time> <what>` from a `Sandbox: <cmd>(<pid>) deny…` report matching `pids`.
#[must_use]
pub fn refusal_of(line: &str, pids: &[String]) -> Option<String> {
    let i = line.find("Sandbox: ")?;
    let rest = &line[i + 9..];
    let open = rest.find('(')?;
    let close = rest[open..].find(") deny")?;
    if rest[..open].contains(' ') || !pids.iter().any(|p| *p == rest[open + 1..open + close]) {
        return None;
    }
    let time = line
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.split('-').next())
        .unwrap_or("");
    Some(format!("{time} {rest}"))
}

/// Format the `egress` report for `run` over the last `minutes`.
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

/// Format the one-line `# …` fixture header for this lab and the agent its active tab runs.
#[must_use]
pub fn fixture_header(run: &Run, door: &Door<'_>, what: &str) -> String {
    fixture_header_on(run, door, None, what)
}

/// [`fixture_header`] for the tab `tab` (`None`: the active tab): its size, and for Claude
/// Code the launch that ran in it ([`launch_record`]).
#[must_use]
pub fn fixture_header_on(run: &Run, door: &Door<'_>, tab: Option<&str>, what: &str) -> String {
    let at = tab.map(|sid| format!("@{sid}"));
    let on =
        |verb: &'static str| -> Vec<&str> { at.as_deref().into_iter().chain([verb]).collect() };
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
    let size = door
        .out(&on("dims"))
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
    if run.agent() == "agy" {
        return format!(
            "# agy {} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running \
             the real Antigravity CLI, launched by path with a scratch `HOME` whose settings name the Gemini API \
             provider, its Google hosts black-holed, a fake `GEMINI_API_KEY` and \
             `GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:<port>` (a local stub of the Gemini API, crates/aterm-lab) · {what}",
            run.agy_ver
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
    let sid = tab.map_or_else(|| active_sid(door), str::to_string);
    let launcher = launch_record(run, &sid);
    let settings = scratch_text(&run.settings());
    let creds = match claude_route(&launcher) {
        ClaudeRoute::Stub => run.w.join("home/.claude/.credentials.json"),
        ClaudeRoute::Default => {
            launcher_value(&launcher, aterm_agent::harness::accounts::LOGIN_ENV)
                .map_or_else(|| run.w.join("home/.claude"), std::path::PathBuf::from)
                .join(".credentials.json")
        }
    };
    let stored = crate::sys::scratch_read(&creds)
        .ok()
        .flatten()
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    format!(
        "# claude-code {} · MEASURED {day}, `aterm ctl text` of a private headless aterm ({size}, aterm {av}) running the real \
         Claude Code, {} · {what}",
        run.claude_ver,
        claude_launch(&launcher, &settings, stored.as_deref())
    )
}

/// The active tab's session id ([`active_in`] of `ls`), empty when it lists none.
#[must_use]
pub fn active_sid(door: &Door<'_>) -> String {
    active_in(&door.out(&["ls"])).unwrap_or_default()
}

/// The session id of the `ls` row marked `active=1` (its third word), the first.
#[must_use]
pub fn active_in(ls: &str) -> Option<String> {
    ls.lines().find_map(|l| {
        l.split_whitespace()
            .any(|w| w == "active=1")
            .then(|| l.split_whitespace().nth(2).map(str::to_string))
            .flatten()
    })
}

/// Where the launcher of what ran in tab `sid` is kept ([`record_launch`]).
#[must_use]
pub fn launch_path(run: &Run, sid: &str) -> std::path::PathBuf {
    run.run.join(crate::lab::LAUNCHES).join(format!("{sid}.sh"))
}

/// Keep `launcher` as what ran in tab `sid`, for its captures' header ([`launch_record`]).
/// # Errors
/// The record could not be written.
pub fn record_launch(run: &Run, sid: &str, launcher: &str) -> Result<(), Fail> {
    let path = launch_path(run, sid);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| Fail::die(format!("cannot make {}: {e}", dir.display())))?;
    }
    atomic_write(&path, launcher.as_bytes())
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// The launcher of what ran in tab `sid` ([`record_launch`]), else the lab's last Claude Code
/// launch (`claude.sh`), else empty.
#[must_use]
pub fn launch_record(run: &Run, sid: &str) -> String {
    let kept = (!sid.is_empty())
        .then(|| fs::read_to_string(launch_path(run, sid)).ok())
        .flatten();
    kept.unwrap_or_else(|| fs::read_to_string(run.run.join("claude.sh")).unwrap_or_default())
}

/// Which way a Claude Code launch reached a model's API ([`claude_route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeRoute {
    /// The lab's own: `ANTHROPIC_BASE_URL` set to the local stub, `CLAUDE_CONFIG_DIR` scratch
    /// ([`crate::lab::launcher`]). A lab that launched nothing reads so too.
    Stub,
    /// Claude Code's default route: no `ANTHROPIC_BASE_URL` set (the launcher unsets it, and
    /// `CLAUDE_CONFIG_DIR` and the proxies), so it would reach Anthropic but for the jail.
    Default,
}

/// The route of the launch `launcher` (a tab's `sh` script): the stub's where it sets
/// `ANTHROPIC_BASE_URL`, or where there is no launcher; else the default route.
#[must_use]
pub fn claude_route(launcher: &str) -> ClaudeRoute {
    if launcher.trim().is_empty() || launcher.contains("ANTHROPIC_BASE_URL=") {
        ClaudeRoute::Stub
    } else {
        ClaudeRoute::Default
    }
}

/// The value `launcher` gives `key` (`KEY=value` or `KEY='value'`), the first.
#[must_use]
pub fn launcher_value(launcher: &str, key: &str) -> Option<String> {
    let at = format!("{key}=");
    let start = launcher.match_indices(at.as_str()).find_map(|(i, _)| {
        let before = launcher[..i].chars().next_back();
        before
            .is_none_or(|c| c.is_whitespace() || c == '\'')
            .then_some(i + at.len())
    })?;
    let rest = &launcher[start..];
    if let Some(quoted) = rest.strip_prefix('\'') {
        quoted.split_once('\'').map(|(v, _)| v.to_string())
    } else {
        rest.split_whitespace().next().map(str::to_string)
    }
}

/// Whether a `.credentials.json` holds a dead stored login: a `claudeAiOauth` whose tokens
/// are empty and whose `expiresAt` is past (0), as the lab's dead-login world lays it.
#[must_use]
pub fn stored_login_dead(text: &str) -> bool {
    J::parse(text)
        .ok()
        .and_then(|j| j.get("claudeAiOauth").cloned())
        .is_some_and(|o| {
            let empty = |k: &str| o.get(k).and_then(J::as_str).is_none_or(str::is_empty);
            empty("accessToken")
                && empty("refreshToken")
                && o.get("expiresAt")
                    .and_then(J::as_f64)
                    .is_none_or(|t| t <= 0.0)
        })
}

/// Where a default-route launch's stored login lives ([`Launched::stored`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDir {
    /// A scratch login directory the launcher names by `CLAUDE_SECURESTORAGE_CONFIG_DIR`
    /// ([`aterm_agent::harness::accounts::LOGIN_ENV`]).
    SecureStorage,
    /// The scratch HOME's own `.claude`.
    ScratchClaude,
}

/// How Claude Code was launched in a tab, as its fixture header tells it ([`launched`],
/// [`Self::sentence`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launched {
    /// Its route ([`claude_route`]).
    pub route: ClaudeRoute,
    /// Its login ([`claude_login`]).
    pub login: ClaudeLogin,
    /// On the default route with a stored login: where that login lives, and whether it is
    /// dead ([`stored_login_dead`]); `None` otherwise.
    pub stored: Option<(LoginDir, bool)>,
}

/// How Claude Code was launched, from the tab's `launcher`, the scratch `settings` and the
/// `.credentials.json` the launch reads (`stored`, its text where there is one).
#[must_use]
pub fn launched(launcher: &str, settings: &str, stored: Option<&str>) -> Launched {
    let route = claude_route(launcher);
    let login = claude_login(launcher, settings, stored.is_some());
    let stored =
        (route == ClaudeRoute::Default && login == ClaudeLogin::StoredCredentials).then(|| {
            let dir =
                if launcher_value(launcher, aterm_agent::harness::accounts::LOGIN_ENV).is_some() {
                    LoginDir::SecureStorage
                } else {
                    LoginDir::ScratchClaude
                };
            (dir, stored.is_some_and(stored_login_dead))
        });
    Launched {
        route,
        login,
        stored,
    }
}

impl Launched {
    /// The sentence a fixture header tells this launch by.
    #[must_use]
    pub fn sentence(self) -> String {
        match self.route {
            ClaudeRoute::Stub => format!(
                "launched by path with a scratch `CLAUDE_CONFIG_DIR`, {} and \
                 `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` (a local stub of the API, crates/aterm-lab)",
                self.login.phrase()
            ),
            ClaudeRoute::Default => {
                let how = match self.stored {
                    Some((dir, dead)) => {
                        let dir = match dir {
                            LoginDir::SecureStorage => {
                                "`CLAUDE_SECURESTORAGE_CONFIG_DIR` a scratch login dir"
                            }
                            LoginDir::ScratchClaude => "its scratch `.claude`",
                        };
                        let what = if dead {
                            "a dead stored login"
                        } else {
                            "a stored login"
                        };
                        format!(
                            "{dir} whose `.credentials.json` is {what}, no `apiKeyHelper` and no OAuth token"
                        )
                    }
                    None => self.login.phrase().to_string(),
                };
                format!(
                    "launched by path on its default route (no `CLAUDE_CONFIG_DIR`, no \
                     `ANTHROPIC_BASE_URL` and no proxy: the lab's scratch HOME, {how}, every \
                     connection but loopback denied by the lab's jail, crates/aterm-lab)"
                )
            }
        }
    }
}

/// How a fixture header says Claude Code was launched: [`launched`]'s [`Launched::sentence`].
#[must_use]
pub fn claude_launch(launcher: &str, settings: &str, stored: Option<&str>) -> String {
    launched(launcher, settings, stored).sentence()
}

/// How the lab's Claude Code signs in ([`claude_login`]), each named in a fixture header by its
/// [`Self::phrase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeLogin {
    /// The fake claude.ai OAuth token the tab's launcher sets (`claude --subscriber`).
    OAuthToken,
    /// The fake key of the `apiKeyHelper` the scratch settings name.
    ApiKeyHelper,
    /// A scratch `.credentials.json`, with no helper and no token.
    StoredCredentials,
    /// None of them.
    NoLogin,
}

impl ClaudeLogin {
    /// The words a fixture header names this login by.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::OAuthToken => "a fake claude.ai OAuth token (`claude --subscriber`)",
            Self::ApiKeyHelper => "a fake `apiKeyHelper` key",
            Self::StoredCredentials => {
                "no `apiKeyHelper` and no OAuth token but a scratch `.credentials.json`"
            }
            Self::NoLogin => "no login (no `apiKeyHelper`, no OAuth token)",
        }
    }
}

/// How the lab's Claude Code signs in: the fake claude.ai OAuth token `launcher` (the tab's
/// `claude.sh`) sets (`claude --subscriber`), else the fake `apiKeyHelper` key the scratch
/// `settings` name, else a scratch `.credentials.json` (`stored`), else none.
#[must_use]
pub fn claude_login(launcher: &str, settings: &str, stored: bool) -> ClaudeLogin {
    let helper = J::parse(settings)
        .ok()
        .is_some_and(|s| s.get("apiKeyHelper").and_then(J::as_str).is_some());
    if launcher.contains("CLAUDE_CODE_OAUTH_TOKEN") {
        ClaudeLogin::OAuthToken
    } else if helper {
        ClaudeLogin::ApiKeyHelper
    } else if stored {
        ClaudeLogin::StoredCredentials
    } else {
        ClaudeLogin::NoLogin
    }
}

/// `header [tag] [@sid] "<what>"`: the active tab's, or the named one's.
pub fn cmd_header(args: &[String]) -> Res {
    let (run, mut a) = lab(args)?;
    let named = take_tab(&mut a)?;
    let door = Door::new(&run)?;
    let tab = named
        .map(|sid| named_or_first(&door, Some(sid)))
        .transpose()?;
    println!(
        "{}",
        fixture_header_on(
            &run,
            &door,
            tab.as_deref(),
            a.first().map_or("", String::as_str)
        )
    );
    Ok(0)
}

/// Write `$LAB_STATE/captures/claude-<version>-<name>.txt` and its PNG; return their paths.
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

/// Capture the current frame as a PNG next to the fixtures, or `None`.
pub fn png(door: &Door<'_>, dir: &Path, full: &str) -> Option<std::path::PathBuf> {
    png_on(door, None, dir, full)
}

/// [`png`] of tab `tab` (`None`: the active tab).
pub fn png_on(
    door: &Door<'_>,
    tab: Option<&str>,
    dir: &Path,
    full: &str,
) -> Option<std::path::PathBuf> {
    let name = format!("{full}.png");
    let at = tab.map(|sid| format!("@{sid}"));
    let args: Vec<&str> = at
        .as_deref()
        .into_iter()
        .chain(["image", name.as_str()])
        .collect();
    let r = door.out(&args);
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

/// Capture the active tab's screen into `$LAB_STATE/captures/<agent>-<version>-<name>.txt`
/// (and `.png`).
/// # Errors
/// The screen could not be read, or the capture could not be written.
pub fn capture(
    run: &Run,
    name: &str,
    what: &str,
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Fail> {
    capture_on(run, None, name, what)
}

/// [`capture`] of tab `tab` (`None`: the active tab).
/// # Errors
/// As [`capture`].
pub fn capture_on(
    run: &Run,
    tab: Option<&str>,
    name: &str,
    what: &str,
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Fail> {
    let door = Door::new(run)?;
    let at = tab.map(|sid| format!("@{sid}"));
    let text: Vec<&str> = at.as_deref().into_iter().chain(["text"]).collect();
    let body = door.ctl(&text)?;
    if body.code != 0 {
        return Err(Fail::die("ctl text failed"));
    }
    let full = format!("{}-{name}", run.fixture_prefix());
    let out = paths().state.join("captures").join(format!("{full}.txt"));
    fs::create_dir_all(paths().state.join("captures")).map_err(|e| Fail::die(e.to_string()))?;
    let text = format!(
        "{}\n{}\n",
        fixture_header_on(run, &door, tab, what),
        body.stdout.trim_end_matches('\n')
    );
    fs::write(&out, &text).map_err(|e| Fail::die(e.to_string()))?;
    let png = png_on(&door, tab, &paths().state.join("captures"), &full);
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

fn pii_found(p: &crate::paths::Paths, host: &str, text: &str) -> Vec<String> {
    [p.user.clone(), host.to_string()]
        .into_iter()
        .chain(p.guarded_homes())
        .filter(|bad| !bad.is_empty() && text.contains(bad.as_str()))
        .collect()
}

/// `capture [tag] [@sid] <name> "<what>"`: the active tab's screen (or the named tab's) as it
/// is NOW, in the repo's fixture shape.
pub fn cmd_capture(args: &[String]) -> Res {
    let (run, mut a) = lab(args)?;
    let named = take_tab(&mut a)?;
    let tab = match named {
        Some(sid) => Some(named_or_first(&Door::new(&run)?, Some(sid))?),
        None => None,
    };
    let name = a.first().ok_or_else(|| Fail::die("capture: need a name"))?;
    capture_on(
        &run,
        tab.as_deref(),
        name,
        a.get(1).map_or("", String::as_str),
    )?;
    Ok(0)
}

fn semver_like(v: &str) -> bool {
    let mut parts = v.splitn(3, '.');
    let (Some(major), Some(minor), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return false;
    }
    let patch_end = rest
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    if patch_end == 0 {
        return false;
    }
    match &rest[patch_end..] {
        "" => true,
        tail => {
            (tail.starts_with('-') || tail.starts_with('+'))
                && tail.len() > 1
                && tail[1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        }
    }
}

/// The leading numbers of a version, for ordering.
#[must_use]
pub fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .take(3)
        .map_while(|x| {
            let end = x
                .bytes()
                .position(|b| !b.is_ascii_digit())
                .unwrap_or(x.len());
            x[..end].parse().ok()
        })
        .collect()
}

/// `twin [tag] <claude-bin>|--crashing [ver]`.
pub fn cmd_twin(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    twin(&run, &a)
}

/// [`cmd_twin`] on a loaded lab.
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
        // a build directory of its own, its program probed where the instance
        // will start it: it showed it ran before the twin names it, and no
        // later twin puts over it
        let build = format!("lab-{}-{}", std::process::id(), now_ms());
        let dest = pkg.join(format!("store/claude/{build}/bin/claude"));
        crate::lab::hold_scratch(&run.run, &build)
            .map_err(|e| Fail::die(format!("twin: cannot hold the scratch: {e}")))?;
        if let Err(e) = put(&dest, 0o755, &PutSrc::File(&src)) {
            crate::lab::release_scratch(&run.run, &build);
            return Err(Fail::die(format!(
                "twin: cannot copy {} into {}: {e}",
                src.display(),
                dest.display()
            )));
        }
        // the hold's clock starts at its probe's exec, so a `down` the hang
        // after a refusal frees it
        aterm_uds::deadline::stamp_exec(&run.run.join(crate::lab::HELD).join(&build));
        let ver = crate::lab::jailed_version(run, &dest.to_string_lossy(), |l| {
            l.split_whitespace().next()
        });
        if !semver_like(&ver) {
            let kept = dest.parent().and_then(Path::parent).unwrap_or(&dest);
            return Err(Fail::die(format!(
                "twin: {} --version named no version; {}",
                src.display(),
                crate::lab::kept_copies(kept, "down", &run.tag)
            )));
        }
        crate::lab::release_scratch(&run.run, &build);
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

/// Selftest check that [`twin`] refuses a symlinked `home/Library` inside the jail.
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

    #[test]
    fn a_header_names_the_login_the_lab_launched_with() {
        let token = "exec env CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-LAB claude\n";
        let plain = "exec env claude\n";
        let helper = r#"{"apiKeyHelper": "/lab/key.sh", "env": {}}"#;
        let none = r#"{"env": {}}"#;
        for (launcher, settings, stored, want) in [
            (token, helper, false, ClaudeLogin::OAuthToken),
            (token, none, true, ClaudeLogin::OAuthToken),
            (plain, helper, true, ClaudeLogin::ApiKeyHelper),
            (plain, none, true, ClaudeLogin::StoredCredentials),
            (plain, none, false, ClaudeLogin::NoLogin),
            (plain, "", false, ClaudeLogin::NoLogin),
            (plain, "{", true, ClaudeLogin::StoredCredentials),
            (
                plain,
                r#"{"apiKeyHelper": null}"#,
                false,
                ClaudeLogin::NoLogin,
            ),
            (
                plain,
                r#"{"apiKeyHelper": 7}"#,
                true,
                ClaudeLogin::StoredCredentials,
            ),
        ] {
            assert_eq!(
                claude_login(launcher, settings, stored),
                want,
                "{launcher} {settings} {stored}"
            );
        }
        let all = [
            ClaudeLogin::OAuthToken,
            ClaudeLogin::ApiKeyHelper,
            ClaudeLogin::StoredCredentials,
            ClaudeLogin::NoLogin,
        ];
        let phrases: std::collections::BTreeSet<&str> =
            all.iter().map(|login| login.phrase()).collect();
        assert_eq!(phrases.len(), all.len(), "each login a header tells apart");
    }

    /// Two tabs as a lab's `ls` lists them (measured 2026-10-25), the second one active.
    const LS: &str = "0 0 s-514051f1c3c7e2adf6ba - alive /private/tmp/at.GQvN/proj meta=0 window=0 active=0 wfocus=1 program=zsh\n\
                      0 1 s-0f4a5235f1fe789003c4 s-514051f1c3c7e2adf6ba alive /private/tmp meta=0 window=0 active=1 wfocus=1 program=zsh\n";

    #[test]
    fn a_verb_names_its_tab_by_sid_and_reads_the_active_one_otherwise() {
        let words = |w: &[&str]| w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let mut a = words(&["@s-0f4a5235f1fe789003c4", "name", "what"]);
        assert_eq!(
            take_tab(&mut a).unwrap().as_deref(),
            Some("s-0f4a5235f1fe789003c4")
        );
        assert_eq!(a, words(&["name", "what"]), "the tab's word is taken out");
        let mut a = words(&["name", "what"]);
        assert_eq!(take_tab(&mut a).unwrap(), None, "no tab named");
        assert_eq!(a.len(), 2);
        for bad in [&["@"][..], &["@a b"], &["@../x", "name"]] {
            assert!(take_tab(&mut words(bad)).is_err(), "{bad:?}");
        }
        for later in [
            &["mention", "@ file picker"][..],
            &["mention", "@mentions"],
            &["@s-0f4a5235f1fe789003c4", "mention", "@mentions"],
        ] {
            let mut a = words(later);
            let took = take_tab(&mut a).unwrap();
            assert_eq!(
                took.is_some(),
                later[0].starts_with('@'),
                "only the first word names a tab: {later:?}"
            );
            assert_eq!(
                a.last().map(String::as_str),
                later.last().copied(),
                "a later `@` word is the verb's own: {later:?}"
            );
        }
        assert!(ls_lists(LS, "s-0f4a5235f1fe789003c4"));
        assert!(ls_lists(LS, "s-514051f1c3c7e2adf6ba"));
        assert!(
            !ls_lists(LS, "s-0f4a"),
            "a prefix is no tab, nor is a parent column"
        );
        assert_eq!(active_in(LS).as_deref(), Some("s-0f4a5235f1fe789003c4"));
        assert_eq!(
            active_in(&LS.replace("active=1", "active=0")),
            None,
            "none active"
        );
    }

    /// The lab's stub-route launcher ([`crate::lab::launcher`]) and Claude Code's default
    /// route's (`claude-login-proof`), as each writes them.
    const STUB: &str = "#!/bin/sh\nclear\nexec env PATH=\"/r/shim:$PATH\" CLAUDE_CONFIG_DIR=/r/scratch/home/.claude \
                        ANTHROPIC_BASE_URL=http://127.0.0.1:57044 \\\n  HTTP_PROXY=http://127.0.0.1:57045 \\\n  /c/claude\n";
    const DEFAULT: &str = "#!/bin/sh\nclear\nexec env -u CLAUDE_CONFIG_DIR -u ANTHROPIC_BASE_URL -u HTTP_PROXY \\\n  \
                           PATH=\"/r/shim:$PATH\" CLAUDE_SECURESTORAGE_CONFIG_DIR='/r/scratch/state/a b/lab-b' \
                           DISABLE_AUTOUPDATER=1 \\\n  '/c/claude'\n";
    const DEAD: &str =
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":[]}}"#;

    #[test]
    fn a_header_names_the_route_the_tab_was_launched_on() {
        assert_eq!(claude_route(STUB), ClaudeRoute::Stub);
        assert_eq!(claude_route(DEFAULT), ClaudeRoute::Default);
        assert_eq!(claude_route(""), ClaudeRoute::Stub, "nothing launched");
        assert_eq!(
            launcher_value(DEFAULT, "CLAUDE_SECURESTORAGE_CONFIG_DIR").as_deref(),
            Some("/r/scratch/state/a b/lab-b"),
            "a quoted value whole"
        );
        assert_eq!(
            launcher_value(STUB, "CLAUDE_CONFIG_DIR").as_deref(),
            Some("/r/scratch/home/.claude")
        );
        assert_eq!(
            launcher_value(DEFAULT, "CLAUDE_CONFIG_DIR"),
            None,
            "`-u CLAUDE_CONFIG_DIR` gives it no value"
        );
        assert!(stored_login_dead(DEAD));
        assert!(!stored_login_dead(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"r","expiresAt":1999999999999}}"#
        ));
        assert!(!stored_login_dead("{}"), "no stored login at all");
        let none = r#"{"env": {}}"#;
        let live = r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1999999999999}}"#;
        let home = DEFAULT.replace(
            " CLAUDE_SECURESTORAGE_CONFIG_DIR='/r/scratch/state/a b/lab-b'",
            "",
        );
        let of = |route, login, stored| Launched {
            route,
            login,
            stored,
        };
        let (stub, default, stored) = (
            ClaudeRoute::Stub,
            ClaudeRoute::Default,
            ClaudeLogin::StoredCredentials,
        );
        let cases = [
            ("the stub's route", STUB, Some(DEAD), of(stub, stored, None)),
            (
                "the default route, a dead login in a named dir",
                DEFAULT,
                Some(DEAD),
                of(default, stored, Some((LoginDir::SecureStorage, true))),
            ),
            (
                "the default route, a live login",
                DEFAULT,
                Some(live),
                of(default, stored, Some((LoginDir::SecureStorage, false))),
            ),
            (
                "the default route, the scratch HOME's own `.claude`",
                home.as_str(),
                Some(DEAD),
                of(default, stored, Some((LoginDir::ScratchClaude, true))),
            ),
            (
                "the default route, no stored login",
                DEFAULT,
                None,
                of(default, ClaudeLogin::NoLogin, None),
            ),
        ];
        let mut sentences = std::collections::BTreeSet::new();
        for (case, launcher, creds, want) in cases {
            let got = launched(launcher, none, creds);
            assert_eq!(got, want, "{case}");
            let sentence = claude_launch(launcher, none, creds);
            assert_eq!(
                sentence,
                got.sentence(),
                "{case}: the header's is the decision's"
            );
            for key in ["CLAUDE_CONFIG_DIR", "ANTHROPIC_BASE_URL"] {
                assert!(sentence.contains(key), "{case}: names {key}");
            }
            assert_eq!(
                sentence.contains(aterm_agent::harness::accounts::LOGIN_ENV),
                got.stored
                    .is_some_and(|(dir, _)| dir == LoginDir::SecureStorage),
                "{case}: names the login dir's variable where the launch set it"
            );
            sentences.insert(sentence);
        }
        assert_eq!(
            sentences.len(),
            cases.len(),
            "each launch a header tells apart"
        );
    }

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
        assert!(pii_found(&p, host, "cwd ~/x").is_empty());
        let mut own =
            crate::paths::Paths::resolve_with(Some(account.clone()), Some(account.into()));
        own.user = "lab-owner-name".into();
        assert!(pii_found(&own, host, &format!("cwd {s}/x")).is_empty());
    }

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
        for m in ["ok", "limit", "tool", "bg", "slow", "rate-limit"] {
            assert!(refused(m).contains("is a built-in mode"), "{m}");
        }
        assert!(refused("refresh").contains("token refresh"));
        for m in ["a b", "custom:x", "limit:five_hour:1", "x/../y"] {
            assert!(refused(m).contains("letters, digits"), "{m}");
        }
    }

    #[test]
    fn versions_order_by_their_numbers() {
        assert!(version_key("2.1.300") > version_key("2.1.299"));
        assert!(version_key("2.1.300-rc.1") > version_key("2.1.299"));
        assert_eq!(version_key("x.99.0"), Vec::<u64>::new());
        assert!(version_key("99.0.0") > version_key("2.1.300"));
        assert!(
            semver_like("2.1.999")
                && semver_like("2.1.300-rc.1")
                && semver_like("2.1.300+build.5")
                && !semver_like("2.1.3x")
                && !semver_like("2.1.3-")
                && !semver_like("2.1.3+")
                && !semver_like("2.1a.3")
                && !semver_like("x.1.2")
                && !semver_like("2.1")
        );
    }
}
