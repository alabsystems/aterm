// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real Codex lab runner against the stub Responses API ([`safe_args`], [`takes_no_daemon`]).

use std::fs;
use std::path::{Path, PathBuf};

use crate::LAB;
use crate::door::Door;
use crate::pyjson::{COMPACT, J, Style};
use crate::run::{Run, take_tag};
use crate::util::shell_quote;
use crate::{Fail, Res};

/// The lab provider's `env_key` value.
pub use crate::stub::LAB_CODEX_KEY;

fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

/// The lab has a Codex to run.
///
/// # Errors
/// None is managed in this lab.
pub fn need_codex(run: &Run) -> Result<(), Fail> {
    if run.codex_bin.is_empty() || !crate::sys::executable(&run.codex_bin) {
        return Err(Fail::die(format!(
            "no managed Codex in lab '{}' (aterm pkg install codex, or LAB_CODEX=/path/to/codex at up)",
            run.tag
        )));
    }
    Ok(())
}

/// A TOML string.
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `CODEX_HOME/config.toml` for a login (`lab`, `chatgpt`, `signed-out`).
#[must_use]
pub fn config_text(mode: &str, port: &str, proj: &str, proj_spelled: &str, model: &str) -> String {
    let stub = format!("http://127.0.0.1:{port}");
    let mut l: Vec<String> = vec![
        format!(
            "# aterm-lab codex (crates/aterm-lab), mode {mode}: rewritten at each launch; Codex may add to it"
        ),
        "check_for_update_on_startup = false".into(),
        "cli_auth_credentials_store = \"file\"".into(),
        "mcp_oauth_credentials_store = \"file\"".into(),
    ];
    if mode != "signed-out" {
        l.push("model_provider = \"lab\"".into());
    }
    if !model.is_empty() {
        l.push(format!("model = {}", q(model)));
    }
    if mode == "chatgpt" {
        l.extend([
            format!("chatgpt_base_url = {}", q(&format!("{stub}/backend-api/"))),
            String::new(),
            "[features]".into(),
            "apps = false".into(),
        ]);
    }
    l.extend(
        [
            "",
            "[analytics]",
            "enabled = false",
            "",
            "[feedback]",
            "enabled = false",
            "",
            "[otel]",
            "exporter = \"none\"",
            "metrics_exporter = \"none\"",
            "trace_exporter = \"none\"",
            "",
            "[tui]",
            "notifications = false",
        ]
        .map(str::to_string),
    );
    if mode != "signed-out" {
        l.extend([
            String::new(),
            "[model_providers.lab]".into(),
            "name = \"lab stub\"".into(),
            format!("base_url = {}", q(&format!("{stub}/v1"))),
            "wire_api = \"responses\"".into(),
            if mode == "chatgpt" {
                "requires_openai_auth = true".into()
            } else {
                "env_key = \"LAB_CODEX_KEY\"".into()
            },
        ]);
    }
    let mut projs = vec![proj.to_string(), proj_spelled.to_string()];
    projs.sort();
    projs.dedup();
    for d in projs {
        l.extend([
            String::new(),
            format!("[projects.{}]", q(&d)),
            "trust_level = \"trusted\"".into(),
        ]);
    }
    let mut out = l.join("\n");
    out.push('\n');
    out
}

/// Write the scratch `config.toml` for `mode` (through no link).
///
/// # Errors
/// The write was refused.
pub fn write_config(run: &Run, mode: &str) -> Result<(), Fail> {
    let proj =
        fs::canonicalize(&run.proj).map_or(run.proj.clone(), |p| p.to_string_lossy().into_owned());
    let model = std::env::var("LAB_CODEX_MODEL").unwrap_or_default();
    let text = config_text(mode, &run.port, &proj, &run.proj, &model);
    crate::sys::put(
        &run.w.join("codex-home/config.toml"),
        0o600,
        &crate::sys::PutSrc::Bytes(text.as_bytes()),
    )
    .map_err(|e| Fail::die(format!("codex: cannot write config.toml: {e}")))
}

/// A composed, fake ChatGPT `auth.json`.
#[must_use]
pub fn auth_json(label: &str, account: &str, plan: &str, now: i64) -> String {
    let seg = |j: &J| crate::util::b64url(j.dump(COMPACT).as_bytes());
    let jwt = |mut claims: Vec<(String, J)>| {
        claims.push(("lab".into(), J::s(label)));
        claims.push(("iat".into(), J::Int(now)));
        claims.push(("exp".into(), J::Int(now + 30 * 86_400)));
        format!(
            "{}.{}.bGFiLW5vdC1zaWduZWQ",
            seg(&J::obj([("alg", J::s("none")), ("typ", J::s("JWT"))])),
            seg(&J::Obj(claims))
        )
    };
    let auth = J::obj([
        ("chatgpt_plan_type", J::s(plan)),
        ("chatgpt_user_id", J::s("user-lab")),
        ("user_id", J::s("user-lab")),
        ("chatgpt_account_id", J::s(account)),
        (
            "chatgpt_account_user_id",
            J::s(format!("user-lab__{account}")),
        ),
    ]);
    let email = "user@example.com";
    let id = jwt(vec![
        ("https://api.openai.com/auth".into(), auth.clone()),
        ("email".into(), J::s(email)),
        (
            "https://api.openai.com/profile".into(),
            J::obj([("email", J::s(email))]),
        ),
    ]);
    let access = jwt(vec![("https://api.openai.com/auth".into(), auth)]);
    let d = J::obj([
        ("auth_mode", J::s("chatgpt")),
        ("OPENAI_API_KEY", J::Null),
        (
            "tokens",
            J::obj([
                ("id_token", J::s(id)),
                ("access_token", J::s(access)),
                ("refresh_token", J::s(format!("lab-refresh-{label}"))),
                ("account_id", J::s(account)),
            ]),
        ),
        ("last_refresh", J::s(crate::util::utc_stamp(now))),
    ]);
    d.dump(Style {
        indent: Some(2),
        compact: false,
        ascii: true,
    })
}

/// Write [`auth_json`] into the scratch `CODEX_HOME` (mode 0600, through no link).
///
/// # Errors
/// The write was refused.
pub fn write_auth(run: &Run, label: &str, account: &str, plan: &str) -> Result<PathBuf, Fail> {
    let p = run.w.join("codex-home/auth.json");
    let text = auth_json(label, account, plan, crate::util::now_secs());
    crate::sys::put(&p, 0o600, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("codex: cannot write auth.json: {e}")))?;
    Ok(p)
}

/// Write [`auth_json`] into the live `CODEX_HOME` and a note of it into the stub's log.
///
/// # Errors
/// The write was refused, or the stub did not take the note.
pub fn auth_and_note(run: &Run, label: &str, account: &str, plan: &str) -> Result<PathBuf, Fail> {
    let p = write_auth(run, label, account, plan)?;
    let note = format!("codex-auth: auth.json now holds {label} (account {account}, plan {plan})");
    crate::stub::control(
        &run.port,
        &format!("/__note?t={}", crate::stub::query_escape(&note)),
    )
    .map_err(|e| {
        Fail::die(format!(
            "codex-auth: auth.json holds {label}, but the stub of lab '{}' did not take the note ({e})",
            run.tag
        ))
    })?;
    Ok(p)
}

/// Refuse arguments that would start a login, contact a real server, or open the Desktop app.
///
/// # Errors
/// What was refused.
pub fn safe_args(tab: bool, args: &[String]) -> Result<(), String> {
    let joined = args.join(" ");
    for a in args {
        match a.as_str() {
            "login" => {
                if !tab && ["login status", "login --help", "login -h"].contains(&joined.as_str()) {
                    continue;
                }
                return Err("codex: a login never runs in the lab (codex-run takes exactly 'login status', 'login --help' or 'login -h'; the tab no login at all)".into());
            }
            "logout" | "app" | "app-server" | "remote-control" | "agents" | "exec-server"
            | "cloud" | "update" => {
                return Err(format!(
                    "codex: '{a}' is refused in the lab (a login, a server or an app outside the jail's reach)"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether the Codex at `bin` takes `--no-daemon`.
#[must_use]
pub fn takes_no_daemon(bin: &str) -> bool {
    let bin = Path::new(bin);
    aterm_agent::harness::codex_alab::alab_bytes(bin) || names_no_daemon(bin)
}

/// Whether the regular file at `bin` holds the bytes `no-daemon`.
#[must_use]
pub fn names_no_daemon(bin: &Path) -> bool {
    use std::io::Read as _;
    const NEEDLE: &[u8] = b"no-daemon";
    const BLOCK: usize = 4 << 20;
    if !fs::metadata(bin).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(mut file) = fs::File::open(bin) else {
        return false;
    };
    let mut buf = vec![0u8; BLOCK];
    let mut kept = 0;
    loop {
        let n = match file.read(&mut buf[kept..]) {
            Ok(0) => return false,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        let filled = kept + n;
        if buf[..filled].windows(NEEDLE.len()).any(|w| w == NEEDLE) {
            return true;
        }
        kept = filled.min(NEEDLE.len() - 1);
        buf.copy_within(filled - kept..filled, 0);
    }
}

/// The launcher: the line the tab types (`clear`, then `exec`), or `codex-run`'s (`exec`).
#[must_use]
pub fn launcher(run: &Run, tab: bool, mode: &str, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let shim = run.run.join("shim");
    let px = &run.proxy;
    let mut s = String::from("#!/bin/sh\n");
    if tab {
        s.push_str("clear\n");
    }
    s.push_str(&format!(
        "exec env -u OPENAI_API_KEY -u CODEX_API_KEY -u OPENAI_BASE_URL PATH=\"{}:$PATH\" HOME={} CODEX_HOME={} \\\n",
        shim.display(),
        shell_quote(&format!("{w}/home")),
        shell_quote(&format!("{w}/codex-home"))
    ));
    s.push_str(&format!(
        "  HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px} \\\n"
    ));
    s.push_str("  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \\\n");
    s.push_str(&format!(
        "  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n",
        shell_quote(&format!("{w}/security.log")),
        shell_quote(&format!("{w}/open.log")),
        shell_quote(&shim.join("open").to_string_lossy())
    ));
    s.push_str(&format!(
        "  CODEX_REFRESH_TOKEN_URL_OVERRIDE=http://127.0.0.1:{0}/oauth/token CODEX_REVOKE_TOKEN_URL_OVERRIDE=http://127.0.0.1:{0}/oauth/revoke \\\n",
        run.port
    ));
    if mode == "lab" {
        s.push_str(&format!("  LAB_CODEX_KEY={LAB_CODEX_KEY} \\\n"));
    }
    for kv in envs {
        s.push_str(&format!("  {} \\\n", shell_quote(kv)));
    }
    s.push_str(&format!("  {}", shell_quote(&run.codex_bin)));
    if takes_no_daemon(&run.codex_bin) {
        s.push_str(" --no-daemon");
    }
    for a in args {
        s.push(' ');
        s.push_str(&shell_quote(a));
    }
    s.push('\n');
    s
}

fn write_launcher(path: &Path, text: &str) -> Result<(), Fail> {
    crate::sys::put(path, 0o755, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// How `codex` launches.
#[derive(Debug, Default)]
pub struct Launch {
    /// `lab`, `chatgpt` or `signed-out`.
    pub mode: String,
    /// The plan of a `--chatgpt` login.
    pub plan: String,
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `-- <codex args>`.
    pub args: Vec<String>,
}

/// Parse `codex`'s options.
///
/// # Errors
/// An unknown option, `-e` with no value, or a plan that is no `plan_type`.
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch {
        mode: "lab".into(),
        plan: "plus".into(),
        ..Launch::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => l
                .envs
                .push(it.next().cloned().ok_or("codex: -e needs K=V")?),
            "--no-wait" => l.no_wait = true,
            "--signed-out" => l.mode = "signed-out".into(),
            "--chatgpt" => l.mode = "chatgpt".into(),
            "--" => l.args.extend(it.by_ref().cloned()),
            other => {
                if let Some(plan) = other.strip_prefix("--chatgpt=") {
                    l.mode = "chatgpt".into();
                    l.plan = plan.to_string();
                } else {
                    return Err(format!(
                        "codex: unknown option '{other}' (use --signed-out, --chatgpt[=PLAN], -e K=V, --no-wait, -- <codex args>)"
                    ));
                }
            }
        }
    }
    if l.plan.is_empty()
        || !l
            .plan
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("codex: --chatgpt=PLAN is a plan_type (plus, pro, free, team, ...)".into());
    }
    Ok(l)
}

/// Launch the managed Codex in the lab's first tab.
///
/// # Errors
/// No Codex, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, l: &Launch) -> Result<String, Fail> {
    need_codex(run)?;
    crate::procs::need_witness(run)?;
    safe_args(true, &l.args)?;
    if l.mode != "lab" {
        crate::verbs::set_harness(run, &[("enabled".into(), "false".into())])?;
    }
    let mut run = run.clone();
    run.codex_mode.clone_from(&l.mode);
    run.agent = "codex".into();
    run.save();
    write_config(&run, &l.mode)?;
    let _ = fs::remove_file(run.w.join("codex-home/auth.json"));
    if l.mode == "chatgpt" {
        write_auth(&run, "A", "acct-lab-1", &l.plan)?;
    }
    let script = run.run.join("codex.sh");
    write_launcher(&script, &launcher(&run, true, &l.mode, &l.envs, &l.args))?;
    let door = Door::new(&run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    if door.ctl(&["send", &line])?.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}', login: {}{}  (launcher: {})",
        run.codex_bin,
        run.codex_ver,
        run.tag,
        l.mode,
        if l.mode == "chatgpt" {
            format!(" (plan {}, user@example.com)", l.plan)
        } else {
            String::new()
        },
        script.display()
    ))
}

/// `codex [tag] [--signed-out|--chatgpt[=PLAN]] [-e K=V]... [--no-wait] [-- codex args]`.
pub fn cmd_codex(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let l = parse_launch(&a)?;
    println!("{}", launch(&run, &l)?);
    let run = Run::load(&run.tag)?;
    if !l.no_wait
        && Door::new(&run)?
            .await_match("OpenAI Codex|Sign in|Welcome to Codex|›", 60)
            .unwrap_or(false)
    {
        println!("the TUI is drawing ({LAB} text {})", run.tag);
    }
    Ok(0)
}

/// `codex-run [tag] [--lab|--chatgpt|--signed-out] -- <codex args>`.
pub fn cmd_codex_run(args: &[String]) -> Res {
    let (run, mut a) = lab(args)?;
    codex_run(&run, &mut a, crate::procs::JailOut::Inherit).map(|(rc, _)| rc)
}

/// [`cmd_codex_run`] on a loaded lab.
///
/// # Errors
/// No Codex, a refused argument, or the jail could not start it.
pub fn codex_run(
    run: &Run,
    a: &mut Vec<String>,
    out: crate::procs::JailOut,
) -> Result<(i32, String), Fail> {
    need_codex(run)?;
    let mut mode = if run.codex_mode.is_empty() {
        "lab".to_string()
    } else {
        run.codex_mode.clone()
    };
    match a.first().map(String::as_str) {
        Some("--lab") => {
            mode = "lab".into();
            a.remove(0);
        }
        Some("--chatgpt") => {
            mode = "chatgpt".into();
            a.remove(0);
        }
        Some("--signed-out") => {
            mode = "signed-out".into();
            a.remove(0);
        }
        _ => {}
    }
    if a.first().map(String::as_str) == Some("--") {
        a.remove(0);
    }
    safe_args(false, a)?;
    let current = crate::util::scratch_text(&run.w.join("codex-home/config.toml"));
    if !current
        .lines()
        .next()
        .is_some_and(|l| l.contains(&format!("mode {mode}:")))
    {
        write_config(run, &mode)?;
    }
    let script = run.run.join("codex-run.sh");
    write_launcher(&script, &launcher(run, false, &mode, &[], a))?;
    let user = crate::paths::paths().user.clone();
    let script_s = script.to_string_lossy().into_owned();
    crate::procs::jailed(
        run,
        Path::new(&run.proj),
        &[
            ("LANG", "en_US.UTF-8"),
            ("TERM", "dumb"),
            ("USER", &user),
            ("LOGNAME", &user),
        ],
        &["/bin/sh", &script_s],
        out,
    )
    .map_err(Fail::die)
}

/// `codex-auth [tag] [LABEL [ACCOUNT [PLAN]]]`.
pub fn cmd_codex_auth(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let get = |i: usize, d: &str| a.get(i).cloned().unwrap_or_else(|| d.to_string());
    let (label, account, plan) = (get(0, "B"), get(1, "acct-lab-1"), get(2, "plus"));
    let ok = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    if !(ok(&label) && ok(&account) && ok(&plan)) {
        return Err(Fail::die(
            "codex-auth: LABEL, ACCOUNT and PLAN are letters, digits, _ and -",
        ));
    }
    let p = auth_and_note(&run, &label, &account, &plan)?;
    println!(
        "codex-auth: {} now holds the lab's fake ChatGPT login '{label}' (account {account}, plan {plan}, user@example.com)",
        p.display()
    );
    Ok(0)
}

/// The `x-codex-*` headers of `ratelimits`' arguments.
///
/// # Errors
/// A missing window or an unknown key.
pub fn ratelimit_headers(args: &[String]) -> Result<J, String> {
    let mut h: Vec<(String, J)> = Vec::new();
    if args.first().is_none_or(|a| a == "off") {
        return Ok(J::Obj(h));
    }
    let (Some(prim), Some(sec)) = (args.first(), args.get(1)) else {
        return Err(
            "ratelimits: PRIMARY SECONDARY (used percent of the 5h and the weekly window), or off"
                .into(),
        );
    };
    for (slot, used, window, reset) in [
        ("primary", prim, 300, 3600),
        ("secondary", sec, 10080, 5 * 86_400),
    ] {
        if used == "-" {
            continue;
        }
        let v: f64 = used
            .parse()
            .map_err(|_| format!("ratelimits: {used} is not a percent"))?;
        h.push((
            format!("x-codex-{slot}-used-percent"),
            J::s(crate::pyjson::py_float(v)),
        ));
        h.push((
            format!("x-codex-{slot}-window-minutes"),
            J::s(window.to_string()),
        ));
        h.push((
            format!("x-codex-{slot}-reset-at"),
            J::s(format!("@NOW+{reset}@")),
        ));
    }
    for kv in &args[2..] {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        match k {
            "credits" => {
                let has = matches!(v, "yes" | "unlimited");
                h.push((
                    "x-codex-credits-has-credits".into(),
                    J::s(if has { "true" } else { "false" }),
                ));
                h.push((
                    "x-codex-credits-unlimited".into(),
                    J::s(if v == "unlimited" { "true" } else { "false" }),
                ));
            }
            "balance" => h.push(("x-codex-credits-balance".into(), J::s(v))),
            "reached" => h.push(("x-codex-rate-limit-reached-type".into(), J::s(v))),
            "promo" => h.push(("x-codex-promo-message".into(), J::s(v))),
            "active" => h.push(("x-codex-active-limit".into(), J::s(v))),
            _ => {
                return Err(format!(
                    "ratelimits: unknown {} (credits= balance= reached= promo= active=)",
                    crate::util::py_repr(kv)
                ));
            }
        }
    }
    Ok(J::Obj(h))
}

/// `ratelimits [tag] off | PRIMARY SECONDARY [credits=yes|no|unlimited]
/// [balance=N] [reached=TYPE] [promo=TEXT] [active=ID]`: the `x-codex-*`
/// headers every stub answer carries, and the account usage a `--chatgpt` Codex
/// polls; the `on200`/`on429` blocks `headers` wrote stay
/// ([`crate::verbs::write_headers`]).
pub fn cmd_ratelimits(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let h = ratelimit_headers(&a).map_err(Fail::die)?;
    crate::verbs::write_headers(&run.w, h.clone(), false)?;
    println!("{}", h.py());
    Ok(0)
}

/// The newest rollout of the tab's Codex under the scratch `CODEX_HOME`, or `None`.
#[must_use]
pub fn tab_rollout(run: &Run) -> Option<PathBuf> {
    let mut files = Vec::new();
    crate::util::walk_files(&run.w.join("codex-home/sessions"), &mut files);
    let mut files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    files.into_iter().rev().find(|f| {
        !crate::util::scratch_text(f)
            .lines()
            .next()
            .unwrap_or("")
            .contains("\"originator\":\"codex_exec\"")
    })
}

/// `rollout [tag] [all]`: the `token_count` lines (all: every line) of the tab's newest rollout.
pub fn cmd_rollout(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let f = tab_rollout(&run).ok_or_else(|| {
        Fail::die(format!(
            "no rollout of the tab's Codex under {}/codex-home/sessions yet",
            run.w.display()
        ))
    })?;
    println!("# {}", f.display());
    let all = a.first().map(String::as_str) == Some("all");
    for l in crate::util::scratch_text(&f).lines() {
        if all || l.contains("\"token_count\"") {
            println!("{l}");
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_or_a_server_never_runs_in_the_lab() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(safe_args(false, &v(&["login", "status"])).is_ok());
        assert!(safe_args(false, &v(&["login", "--help"])).is_ok());
        assert!(safe_args(true, &v(&["login", "status"])).is_err());
        assert!(safe_args(false, &v(&["login", "-c", "status"])).is_err());
        assert!(safe_args(false, &v(&["mcp", "login", "x"])).is_err());
        for bad in [
            "logout",
            "app",
            "app-server",
            "remote-control",
            "agents",
            "exec-server",
            "cloud",
            "update",
        ] {
            assert!(safe_args(false, &v(&["exec", bad])).is_err(), "{bad}");
        }
        assert!(safe_args(false, &v(&["exec", "--skip-git-repo-check", "hi"])).is_ok());
    }

    #[test]
    fn the_config_names_its_login_and_trusts_the_project_both_ways() {
        let c = config_text(
            "chatgpt",
            "5",
            "/private/tmp/at.X/proj",
            "/tmp/at.X/proj",
            "",
        );
        assert!(c.starts_with("# aterm-lab codex (crates/aterm-lab), mode chatgpt:"));
        assert!(c.contains(
            "chatgpt_base_url = \"http://127.0.0.1:5/backend-api/\"\n\n[features]\napps = false\n"
        ));
        assert!(c.contains("base_url = \"http://127.0.0.1:5/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n"));
        assert!(
            c.contains("[projects.\"/private/tmp/at.X/proj\"]")
                && c.contains("[projects.\"/tmp/at.X/proj\"]")
        );
        let lab = config_text("lab", "5", "/p", "/p", "gpt-x");
        assert!(
            lab.contains("model_provider = \"lab\"\nmodel = \"gpt-x\"\n")
                && lab.contains("env_key = \"LAB_CODEX_KEY\"")
        );
        assert_eq!(lab.matches("[projects.").count(), 1);
        let out = config_text("signed-out", "5", "/p", "/p", "");
        assert!(!out.contains("model_provider") && !out.contains("[model_providers.lab]"));
    }

    #[test]
    fn the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only() {
        let j = J::parse(&auth_json("B", "acct-lab-1", "plus", 1_792_091_823)).unwrap();
        let tokens = j.get("tokens").unwrap();
        let access = tokens.get("access_token").and_then(J::as_str).unwrap();
        assert!(access.ends_with(".bGFiLW5vdC1zaWduZWQ"));
        assert_eq!(
            crate::stub::auth_label(Some(&format!("Bearer {access}"))),
            "jwt:B"
        );
        let claims = crate::stub::lab_claims(&format!("Bearer {access}")).unwrap();
        let auth = claims.get("https://api.openai.com/auth").unwrap();
        assert_eq!(
            auth.get("chatgpt_plan_type").and_then(J::as_str),
            Some("plus")
        );
        assert_eq!(
            auth.get("chatgpt_account_id").and_then(J::as_str),
            Some("acct-lab-1")
        );
        assert_eq!(
            j.get("last_refresh").and_then(J::as_str),
            Some("2026-10-15T19:17:03.000000Z")
        );
        assert_eq!(
            tokens.get("refresh_token").and_then(J::as_str),
            Some("lab-refresh-B")
        );
        assert_eq!(crate::stub::auth_label(Some("Bearer lab-not-a-key")), "key");
        assert_eq!(crate::stub::auth_label(None), "none");
        assert_eq!(
            crate::stub::auth_label(Some("Bearer sk-x")),
            "other(4 chars)"
        );
    }

    #[test]
    fn ratelimits_name_each_window_and_the_credits() {
        let a: Vec<String> = ["76", "-", "credits=unlimited", "balance=12.5"]
            .map(String::from)
            .to_vec();
        let h = ratelimit_headers(&a).unwrap();
        assert_eq!(
            h.get("x-codex-primary-used-percent").and_then(J::as_str),
            Some("76.0")
        );
        assert_eq!(
            h.get("x-codex-primary-reset-at").and_then(J::as_str),
            Some("@NOW+3600@")
        );
        assert!(h.get("x-codex-secondary-used-percent").is_none());
        assert_eq!(
            h.get("x-codex-credits-unlimited").and_then(J::as_str),
            Some("true")
        );
        assert_eq!(
            ratelimit_headers(&["off".to_string()]).unwrap(),
            J::Obj(Vec::new())
        );
        assert!(ratelimit_headers(&["5".to_string()]).is_err());
        assert!(ratelimit_headers(&["1".into(), "2".into(), "bogus=1".into()]).is_err());
    }

    /// The bytes of a stand-in codex-alab binary: clap's literal for the
    /// flag among other text, as the store's build holds it.
    const NAMES_IT: &[u8] = b"\x00\xcf\xfa\xed\xfeno-daemon cannot be used with --remote\x00";

    /// The bytes of a stand-in binary that takes no `--no-daemon`: the
    /// literal's pieces, never the literal (vendor 0.151.0's case).
    const NAMES_NONE: &[u8] = b"\x00\xcf\xfa\xed\xfeno-daemo\x00n-daemon\x00no-daemoN\x00";

    /// A Codex package at `root` (layout 1, `bin/codex` its entrypoint,
    /// holding `exe`), its manifest's version `version`, ALab's when `alab`
    /// (its `alab-build.json` beside the manifest): the binary's path.
    fn package_at(root: &Path, version: &str, alab: bool, exe: &[u8]) -> String {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/codex"), exe).unwrap();
        fs::write(
            root.join(aterm_agent::harness::codex_alab::PACKAGE_JSON),
            format!(r#"{{"layoutVersion":1,"version":"{version}","entrypoint":"bin/codex"}}"#),
        )
        .unwrap();
        let mark = root.join(aterm_agent::harness::codex_alab::ALAB_BUILD_JSON);
        if alab {
            fs::write(&mark, "{}").unwrap();
        } else {
            let _ = fs::remove_file(&mark);
        }
        root.join("bin/codex").to_string_lossy().into_owned()
    }

    /// THE LAUNCHER EXECS CODEX BY PATH, `--no-daemon` ONLY WHERE IT IS
    /// TAKEN: codex-alab's package (its ALab build mark beside the manifest)
    /// is launched with `--no-daemon` before the caller's arguments, in the
    /// tab and in `codex-run`, its bytes unread.
    #[test]
    fn the_launcher_execs_codex_by_path_with_no_daemon_where_it_is_taken() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive package fixture");
        let alab = package_at(
            &scratch.path().join("alab b"),
            "0.164.0-dev.1+gabc",
            true,
            b"",
        );
        let vendor = package_at(&scratch.path().join("vendor"), "0.151.0", false, NAMES_NONE);
        let vendor_163 = package_at(
            &scratch.path().join("vendor 163"),
            "0.163.0",
            false,
            NAMES_IT,
        );
        let dev = scratch.path().join("codex-rs/target/release/codex");
        fs::create_dir_all(dev.parent().unwrap()).unwrap();
        fs::write(&dev, NAMES_IT).unwrap();
        let dev = dev.to_string_lossy().into_owned();
        let other = scratch.path().join("other/codex");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, NAMES_NONE).unwrap();
        let other = other.to_string_lossy().into_owned();
        let neither = package_at(
            &scratch.path().join("neither"),
            "0.152.0-alpha.1",
            false,
            NAMES_IT,
        );
        let run = |codex_bin: &str| Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            codex_bin: codex_bin.into(),
            ..Run::default()
        };
        let args = ["-C".to_string(), "/x y".to_string()];
        let s = launcher(&run(&alab), true, "lab", &["K=v 1".into()], &args);
        assert!(s.starts_with("#!/bin/sh\nclear\nexec env -u OPENAI_API_KEY -u CODEX_API_KEY -u OPENAI_BASE_URL PATH=\"/s/runs/t/shim:$PATH\""), "{s}");
        assert!(
            s.contains("  LAB_CODEX_KEY=lab-not-a-key \\\n  'K=v 1' \\\n"),
            "{s}"
        );
        let takes = |bin: &str| {
            [(true, "lab"), (false, "chatgpt")].map(|(tab, mode)| {
                let v = launcher(&run(bin), tab, mode, &[], &args);
                let quoted = shell_quote(bin);
                if v.ends_with(&format!("  {quoted} --no-daemon -C '/x y'\n")) {
                    Some(true)
                } else if v.ends_with(&format!("  {quoted} -C '/x y'\n"))
                    && !v.contains("--no-daemon")
                {
                    Some(false)
                } else {
                    None
                }
            })
        };
        assert_eq!(
            takes(&alab),
            [Some(true); 2],
            "codex-alab, tab and codex-run"
        );
        let r = launcher(&run(&alab), false, "chatgpt", &[], &[]);
        assert!(!r.contains("clear") && !r.contains("LAB_CODEX_KEY="), "{r}");
        assert!(r.ends_with(" --no-daemon\n"), "codex-run's too: {r}");
        assert_eq!(
            takes(&vendor),
            [Some(false); 2],
            "vendor 0.151.0: its bytes name no flag"
        );
        assert_eq!(
            takes(&vendor_163),
            [Some(true); 2],
            "vendor 0.163.0: a release whose bytes name the flag"
        );
        assert_eq!(takes("/a b/codex"), [Some(false); 2], "no file");
        assert_eq!(takes(&dev), [Some(true); 2], "a codex-alab dev build");
        assert_eq!(takes(&other), [Some(false); 2], "a binary naming no flag");
        assert_eq!(
            takes(&neither),
            [Some(true); 2],
            "a package that says neither"
        );
        let marked = package_at(&scratch.path().join("vendor"), "0.151.0", true, NAMES_NONE);
        assert_eq!(marked, vendor);
        assert_eq!(takes(&marked), [Some(true); 2], "CONTROL: the mark");
        fs::write(&vendor_163, NAMES_NONE).unwrap();
        assert_eq!(
            takes(&vendor_163),
            [Some(false); 2],
            "CONTROL: 0.163.0's literal"
        );
        fs::write(&dev, NAMES_NONE).unwrap();
        assert_eq!(takes(&dev), [Some(false); 2], "CONTROL: the literal");
    }

    /// The literal is found wherever it lies, across the seam of two read
    /// blocks too, and nowhere it is not: a file whose only `no-daemon`
    /// straddles the first 4 MiB block's end is read as naming it, the same
    /// file with one byte of it changed is not, and a directory is never
    /// opened.
    #[test]
    fn a_binary_names_the_flag_across_a_block_seam() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive binary fixture");
        let bin = scratch.path().join("codex");
        let mut bytes = vec![0u8; (4 << 20) - 4];
        bytes.extend_from_slice(b"no-daemon");
        bytes.extend_from_slice(&[0u8; 64]);
        fs::write(&bin, &bytes).unwrap();
        assert!(names_no_daemon(&bin), "across the seam");
        let at = (4 << 20) - 4 + 4;
        assert_eq!(bytes[at], b'a');
        bytes[at] = b'A';
        fs::write(&bin, &bytes).unwrap();
        assert!(!names_no_daemon(&bin), "CONTROL: one byte changed");
        assert!(!names_no_daemon(scratch.path()), "a directory");
    }
}
