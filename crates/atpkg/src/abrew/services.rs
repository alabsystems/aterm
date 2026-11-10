// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Brew's `services list` and `services info` over launchd,
//! read-only: service-bearing formulae, their launchctl status
//! and registered plist. Service execution (`start`, `stop`,
//! `run`, `restart`, `kill`, `cleanup`) is not native.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_json::Value;

use super::api::Api;
use super::fetch::run_capture;
use super::prefix::{Kind, Prefix};
use super::tap;
use super::tty;

/// The evaluated service fields brew appends to `to_hash`
/// when the formula's service carries a run command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceDetails {
    pub command: String,
    pub working_dir: Option<String>,
    pub root_dir: Option<String>,
    pub log_path: Option<String>,
    pub error_log_path: Option<String>,
    pub interval: Option<i64>,
    pub cron: Option<String>,
}

/// One service entry (brew `FormulaWrapper#to_hash`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRow {
    pub name: String,
    pub service_name: String,
    pub running: bool,
    pub schedulable: bool,
    pub pid: Option<i64>,
    pub status: &'static str,
    pub user: Option<String>,
    pub file: PathBuf,
    pub exit_code: Option<i64>,
    pub loaded: bool,
    pub registered: bool,
    pub loaded_file: Option<String>,
    pub details: Option<ServiceDetails>,
}

/// The `$HOMEBREW_PREFIX`/`$HOMEBREW_CELLAR`/`/$HOME`
/// placeholder substitution brew applies to API service data.
fn substitute(text: &str, prefix: &Path, cellar: &Path, home: &Path) -> String {
    text.replace("$HOMEBREW_PREFIX", &prefix.display().to_string())
        .replace("$HOMEBREW_CELLAR", &cellar.display().to_string())
        .replace("/$HOME", &home.display().to_string())
}

/// Ruby `Utils::Shell.sh_quote`, closely enough for service
/// argv display.
fn sh_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%_+=:,./-".contains(c))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Brew `Service#manual_command`: non-PATH env assignments
/// followed by the quoted run argv. User env overrides from
/// `<config>/services/<formula>.env` apply as in brew.
fn manual_command(
    name: &str,
    run: &[String],
    raw_service: &Value,
    subst: &dyn Fn(&str) -> String,
) -> String {
    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(Value::Object(map)) = raw_service.get("environment_variables") {
        for (k, v) in map {
            if let Some(v) = v.as_str() {
                env.push((k.clone(), subst(v)));
            }
        }
    }
    let config_home = std::env::var_os("HOMEBREW_USER_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".homebrew"));
    if let Ok(text) =
        std::fs::read_to_string(config_home.join("services").join(format!("{name}.env")))
    {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                match env.iter_mut().find(|(ek, _)| ek == k.trim()) {
                    Some(slot) => slot.1 = v.trim().to_string(),
                    None => env.push((k.trim().to_string(), v.trim().to_string())),
                }
            }
        }
    }
    let mut parts: Vec<String> = env
        .iter()
        .filter(|(k, _)| k != "PATH")
        .map(|(k, v)| format!("{k}=\"{v}\""))
        .collect();
    parts.extend(run.iter().map(|a| sh_quote(a)));
    parts.join(" ")
}

/// The details brew appends when the service has a run command.
fn service_details(
    name: &str,
    raw_service: Option<&Value>,
    prefix: &Prefix,
    home: &Path,
) -> Option<ServiceDetails> {
    let svc = raw_service?;
    let cellar = prefix.cellar();
    let subst = |t: &str| substitute(t, &prefix.root, &cellar, home);
    let run: Vec<String> = match svc.get("run") {
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_str().map(subst)).collect(),
        Some(Value::String(one)) => vec![subst(one)],
        _ => return None,
    };
    if run.is_empty() || run.iter().all(|a| a.is_empty()) {
        return None;
    }
    let field = |key: &str| {
        svc.get(key)
            .and_then(Value::as_str)
            .map(subst)
            .filter(|v| !v.is_empty())
    };
    Some(ServiceDetails {
        command: manual_command(name, &run, svc, &subst),
        working_dir: field("working_dir"),
        root_dir: field("root_dir"),
        log_path: field("log_path"),
        error_log_path: field("error_log_path"),
        interval: svc.get("interval").and_then(Value::as_i64),
        cron: svc
            .get("cron")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|c| !c.trim().is_empty()),
    })
}

/// The loaded path from `launchctl print` output
/// (`launchctl list` output carries none).
pub fn loaded_file_of(output: &str, print: bool) -> Option<String> {
    if !print {
        return None;
    }
    output
        .lines()
        .filter_map(|l| l.trim().strip_prefix("path = "))
        .next()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// The plist names brew probes for a formula: an explicit
/// service `name macos:` stands alone; otherwise the canonical
/// `sh.brew.<name>` plus the legacy `homebrew.mxcl.<name>`.
pub fn plist_names(name: &str, raw_service: Option<&Value>) -> Vec<String> {
    if let Some(explicit) = raw_service.and_then(|s| s.get("name")) {
        let macos = match explicit {
            Value::String(one) => Some(one.clone()),
            Value::Object(_) => explicit
                .get("macos")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        };
        if let Some(macos) = macos.filter(|m| !m.is_empty()) {
            return vec![macos];
        }
    }
    vec![format!("sh.brew.{name}"), format!("homebrew.mxcl.{name}")]
}

/// A string value from an XML plist by key (`Label`, `UserName`).
pub fn plist_string(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let needle = format!("<key>{key}</key>");
    let after = &text[text.find(&needle)? + needle.len()..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    let value = after[start..end]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    (!value.is_empty()).then_some(value)
}

/// Brew's pid / last-exit-code extraction from launchctl
/// status output (`print` domain form vs `list` label form).
pub fn status_numbers(output: &str, print: bool) -> (Option<i64>, Option<i64>) {
    fn grab(output: &str, marker: &str) -> Option<i64> {
        let at = output.find(marker)? + marker.len();
        let digits: String = output[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    }
    if print {
        (grab(output, "pid = "), grab(output, "last exit code = "))
    } else {
        (
            grab(output, "\"PID\" = "),
            grab(output, "\"LastExitStatus\" = "),
        )
    }
}

/// Brew `FormulaWrapper#status_symbol`.
pub fn status_symbol(
    pid: Option<i64>,
    loaded: bool,
    exit_code: Option<i64>,
    timed: bool,
    output_blank: bool,
) -> &'static str {
    let pid = pid.filter(|p| *p > 0);
    if pid.is_some() {
        "started"
    } else if !loaded {
        "none"
    } else if exit_code == Some(0) {
        if timed { "scheduled" } else { "stopped" }
    } else if exit_code.is_some_and(|c| c != 0) {
        "error"
    } else if output_blank {
        "unknown"
    } else {
        "other"
    }
}

fn launchctl(args: &[String]) -> (String, bool) {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_capture("/bin/launchctl", &refs) {
        Ok(out) => (
            String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
            out.status.success(),
        ),
        Err(_) => (String::new(), false),
    }
}

fn uid() -> Option<u32> {
    let out = run_capture("id", &["-u"]).ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn current_user() -> Option<String> {
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
    {
        return Some(user);
    }
    let out = run_capture("id", &["-un"]).ok()?;
    let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!user.is_empty()).then_some(user)
}

/// The /dev/console owner (brew's SSH-domain test).
fn console_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata("/dev/console").ok().map(|m| m.uid())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Brew `System.candidate_domain_targets` (non-root): the
/// gui domain, or the user domain first under sudo/SSH.
fn candidate_domains(uid: u32) -> Vec<String> {
    let user_first = super::env_set("HOMEBREW_SUDO_USER")
        || (super::env_set("HOMEBREW_SSH_TTY") && console_uid().is_some_and(|c| c != uid));
    let (first, second) = if user_first {
        ("user", "gui")
    } else {
        ("gui", "user")
    };
    vec![format!("{first}/{uid}"), format!("{second}/{uid}")]
}

/// Brew `System.launchctl_find_service`: (output, success, print?).
fn find_service(label: &str, domains: &[String]) -> (String, bool, bool) {
    for domain in domains {
        let (output, success) = launchctl(&["print".to_string(), format!("{domain}/{label}")]);
        if success && !output.is_empty() {
            return (output, true, true);
        }
    }
    let (output, success) = launchctl(&["list".to_string(), label.to_string()]);
    let success = success && !output.is_empty();
    (output, success, false)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("~"), PathBuf::from)
}

/// Build one row for an installed service-bearing formula.
#[allow(clippy::too_many_arguments)]
fn service_row(
    prefix: &Prefix,
    name: &str,
    keg_base: &Path,
    raw_service: Option<&Value>,
    domains: &[String],
    user_path: &Path,
    boot_path: &Path,
    home: &Path,
    current_user: &Option<String>,
    root: bool,
) -> ServiceRow {
    let name = name.to_string();
    let names = plist_names(&name, raw_service);
    let keg_base = keg_base.to_path_buf();
    let keg_files: Vec<PathBuf> = names
        .iter()
        .map(|n| keg_base.join(format!("{n}.plist")))
        .collect();
    let dest_dir = if root { boot_path } else { user_path };
    let destinations: Vec<PathBuf> = names
        .iter()
        .map(|n| dest_dir.join(format!("{n}.plist")))
        .collect();

    // Label candidates: the names plus labels of existing files.
    let mut label_files: Vec<PathBuf> = keg_files.clone();
    label_files.extend(destinations.iter().cloned());
    if keg_files.iter().all(|f| !f.exists())
        && let Ok(entries) = std::fs::read_dir(&keg_base)
    {
        let mut plists: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "plist"))
            .collect();
        plists.sort();
        if let Some(first) = plists.into_iter().next() {
            label_files.push(first);
        }
    }
    let mut candidates: Vec<String> = names.clone();
    for file in &label_files {
        if let Some(label) = plist_string(file, "Label")
            && !candidates.contains(&label)
        {
            candidates.push(label);
        }
    }

    // Probe: first success wins; a running pid wins outright.
    let mut chosen: Option<(String, bool, String)> = None; // label, print?, output
    for label in &candidates {
        let (output, success, print) = find_service(label, domains);
        if !success {
            continue;
        }
        let (pid, _) = status_numbers(&output, print);
        let hit = (label.clone(), print, output);
        if pid.is_some_and(|p| p > 0) {
            chosen = Some(hit);
            break;
        }
        chosen.get_or_insert(hit);
    }
    let (active, print, output, loaded) = match &chosen {
        Some((label, print, output)) => (label.clone(), *print, output.clone(), true),
        None => (names[0].clone(), false, String::new(), false),
    };
    let (pid, exit_code) = status_numbers(&output, print);
    let timed = raw_service
        .and_then(|s| s.get("run_type"))
        .and_then(Value::as_str)
        .is_some_and(|t| matches!(t, "cron" | "interval"));
    let status = status_symbol(pid, loaded, exit_code, timed, output.is_empty());

    let present_in = |dir: &Path| {
        names
            .iter()
            .any(|n| dir.join(format!("{n}.plist")).exists())
    };
    let present = present_in(boot_path) || present_in(user_path);
    let active_dest = dest_dir.join(format!("{active}.plist"));
    let registered = if active_dest.exists() {
        active_dest
    } else {
        destinations
            .iter()
            .find(|d| d.exists())
            .cloned()
            .unwrap_or(active_dest)
    };
    let user = if registered.exists() {
        plist_string(&registered, "UserName").or_else(|| {
            if registered.parent() == Some(boot_path) {
                Some("root".to_string())
            } else if registered.parent() == Some(user_path) {
                current_user.clone()
            } else {
                None
            }
        })
    } else if present_in(boot_path) {
        Some("root".to_string())
    } else if present_in(user_path) {
        current_user.clone()
    } else {
        None
    };
    let file = if present {
        registered
    } else {
        keg_files
            .iter()
            .find(|f| f.exists())
            .cloned()
            .unwrap_or_else(|| keg_files[0].clone())
    };
    ServiceRow {
        service_name: active,
        running: pid.is_some_and(|p| p > 0),
        schedulable: timed,
        pid,
        registered: present,
        loaded_file: loaded_file_of(&output, print),
        details: service_details(&name, raw_service, prefix, home),
        name,
        status,
        user,
        file,
        exit_code,
        loaded,
    }
}

/// Brew's status cell text (colour per status, exit code
/// appended after an `error`).
fn status_cell(row: &ServiceRow) -> String {
    let mut cell = match row.status {
        "started" | "scheduled" => tty::success(row.status),
        "error" => tty::error("error  "),
        "unknown" | "other" => tty::warning(row.status),
        _ => tty::service_default(row.status),
    };
    if row.status == "error"
        && let Some(code) = row.exit_code
    {
        cell.push_str(&code.to_string());
    }
    cell
}

/// Ruby `%-w.ws` field: truncate then pad (byte-unsafe inputs
/// don't occur — names/statuses are ASCII plus escapes).
fn field(text: &str, width: usize) -> String {
    let truncated: String = text.chars().take(width).collect();
    let len = truncated.chars().count();
    if len >= width {
        truncated
    } else {
        format!("{truncated}{}", " ".repeat(width - len))
    }
}

/// Brew `ListSubcommand.print_table`.
pub fn print_table(rows: &[ServiceRow], home: &Path) -> String {
    let cells: Vec<String> = rows.iter().map(status_cell).collect();
    let files: Vec<String> = rows
        .iter()
        .map(|r| {
            if r.loaded {
                r.file
                    .display()
                    .to_string()
                    .replace(&home.display().to_string(), "~")
            } else {
                String::new()
            }
        })
        .collect();
    let longest_name = rows
        .iter()
        .map(|r| r.name.chars().count())
        .chain([4])
        .max()
        .unwrap();
    let longest_status = cells
        .iter()
        .map(|c| c.chars().count())
        .chain([15])
        .max()
        .unwrap();
    let longest_user = rows
        .iter()
        .filter_map(|r| r.user.as_ref().map(|u| u.chars().count()))
        .chain([4])
        .max()
        .unwrap();
    let header = format!(
        "{} {} {} {}",
        field("Name", longest_name),
        field("Status", longest_status - 9),
        field("User", longest_user),
        "File"
    );
    let mut out = format!("{}\n", tty::bold_out(&header));
    for (row, (cell, file)) in rows.iter().zip(cells.iter().zip(files.iter())) {
        out.push_str(&format!(
            "{} {} {} {}\n",
            field(&row.name, longest_name),
            field(cell, longest_status),
            field(row.user.as_deref().unwrap_or(""), longest_user),
            file
        ));
    }
    out
}

/// Brew `ListSubcommand.print_json` (`JSON.pretty_generate`
/// over the name/status/user/file/exit_code slice).
pub fn to_json(rows: &[ServiceRow]) -> String {
    fn quoted(text: &str) -> String {
        aterm_json::to_string(&Value::String(text.to_string())).unwrap_or_default()
    }
    if rows.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[\n");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        let user = r.user.as_deref().map_or_else(|| "null".to_string(), quoted);
        let exit = r
            .exit_code
            .map_or_else(|| "null".to_string(), |c| c.to_string());
        out.push_str(&format!(
            "  {{\n    \"name\": {},\n    \"status\": {},\n    \"user\": {},\n    \"file\": {},\n    \"exit_code\": {}\n  }}",
            quoted(&r.name),
            quoted(r.status),
            user,
            quoted(&r.file.display().to_string()),
            exit
        ));
    }
    out.push_str("\n]");
    out
}

/// Shared launchd context for list/info.
struct Ctx {
    domains: Vec<String>,
    user_path: PathBuf,
    boot_path: PathBuf,
    home: PathBuf,
    user: Option<String>,
    root: bool,
}

impl Ctx {
    fn row(
        &self,
        prefix: &Prefix,
        name: &str,
        keg_base: &Path,
        raw_service: Option<&Value>,
    ) -> ServiceRow {
        service_row(
            prefix,
            name,
            keg_base,
            raw_service,
            &self.domains,
            &self.user_path,
            &self.boot_path,
            &self.home,
            &self.user,
            self.root,
        )
    }
}

fn raw_service_of(api: &Api, name: &str) -> Option<Value> {
    api.formula_value(name)
        .ok()
        .and_then(|v| v.get("service").cloned())
        .filter(|v| !v.is_null())
}

fn keg_base_for(prefix: &Prefix, name: &str) -> PathBuf {
    if let Some(installed) = prefix.installed_one(Kind::Formula, name) {
        let keg = prefix.cellar().join(name).join(&installed.version);
        if keg.exists() {
            return keg;
        }
    }
    prefix.opt(name)
}

/// Brew `InfoSubcommand.output` for one entry.
pub fn info_output(row: &ServiceRow, verbose: bool) -> String {
    fn pretty_bool(value: bool) -> String {
        if tty::stdout_tty() && !super::env_set("HOMEBREW_NO_EMOJI") {
            let glyph = if value {
                tty::success("\u{2714}")
            } else {
                tty::error("\u{2718}")
            };
            tty::bold_out(&glyph)
        } else {
            value.to_string()
        }
    }
    let mut out = format!("{} ({})\n", tty::bold_out(&row.name), row.service_name);
    out.push_str(&format!("Running: {}\n", pretty_bool(row.running)));
    out.push_str(&format!("Loaded: {}\n", pretty_bool(row.loaded)));
    out.push_str(&format!("Schedulable: {}\n", pretty_bool(row.schedulable)));
    if let Some(pid) = row.pid {
        out.push_str(&format!("User: {}\n", row.user.as_deref().unwrap_or("")));
        out.push_str(&format!("PID: {pid}\n"));
    }
    if !verbose {
        return out;
    }
    out.push_str(&format!(
        "File: {} {}\n",
        row.file.display(),
        pretty_bool(true)
    ));
    out.push_str(&format!(
        "Registered at login: {}\n",
        pretty_bool(row.registered)
    ));
    if let Some(d) = &row.details {
        out.push_str(&format!("Command: {}\n", d.command));
        if let Some(v) = &d.working_dir {
            out.push_str(&format!("Working directory: {v}\n"));
        }
        if let Some(v) = &d.root_dir {
            out.push_str(&format!("Root directory: {v}\n"));
        }
        if let Some(v) = &d.log_path {
            out.push_str(&format!("Log: {v}\n"));
        }
        if let Some(v) = &d.error_log_path {
            out.push_str(&format!("Error log: {v}\n"));
        }
        if let Some(v) = d.interval {
            out.push_str(&format!("Interval: {v}s\n"));
        }
        if let Some(v) = &d.cron {
            out.push_str(&format!("Cron: {v}\n"));
        }
    }
    out
}

/// Brew `services info --json`: the full `to_hash` pretty JSON.
pub fn to_info_json(rows: &[ServiceRow]) -> String {
    fn quoted(text: &str) -> String {
        aterm_json::to_string(&Value::String(text.to_string())).unwrap_or_default()
    }
    fn opt_str(value: &Option<String>) -> String {
        value.as_deref().map_or_else(|| "null".to_string(), quoted)
    }
    fn opt_num(value: Option<i64>) -> String {
        value.map_or_else(|| "null".to_string(), |n| n.to_string())
    }
    if rows.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[\n");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  {\n");
        let mut fields: Vec<String> = vec![
            format!("\"name\": {}", quoted(&r.name)),
            format!("\"service_name\": {}", quoted(&r.service_name)),
            format!("\"running\": {}", r.running),
            format!("\"loaded\": {}", r.loaded),
            format!("\"schedulable\": {}", r.schedulable),
            format!("\"pid\": {}", opt_num(r.pid)),
            format!("\"exit_code\": {}", opt_num(r.exit_code)),
            format!("\"user\": {}", opt_str(&r.user)),
            format!("\"status\": {}", quoted(r.status)),
            format!("\"file\": {}", quoted(&r.file.display().to_string())),
            format!("\"registered\": {}", r.registered),
            format!("\"loaded_file\": {}", opt_str(&r.loaded_file)),
        ];
        if let Some(d) = &r.details {
            fields.push(format!("\"command\": {}", quoted(&d.command)));
            fields.push(format!("\"working_dir\": {}", opt_str(&d.working_dir)));
            fields.push(format!("\"root_dir\": {}", opt_str(&d.root_dir)));
            fields.push(format!("\"log_path\": {}", opt_str(&d.log_path)));
            fields.push(format!(
                "\"error_log_path\": {}",
                opt_str(&d.error_log_path)
            ));
            fields.push(format!("\"interval\": {}", opt_num(d.interval)));
            fields.push(format!("\"cron\": {}", opt_str(&d.cron)));
        }
        for (j, f) in fields.iter().enumerate() {
            out.push_str(&format!(
                "    {f}{}\n",
                if j + 1 < fields.len() { "," } else { "" }
            ));
        }
        out.push_str("  }");
    }
    out.push_str("\n]");
    out
}

/// `brew services [list|info] [--json]`; service control refuses.
pub fn cmd_services(prefix: &Prefix, rest: &[String]) -> ExitCode {
    // Split subcommand, named formulae and flags (admission has
    // already validated the shapes).
    let mut sub = "list";
    let mut named: Vec<String> = Vec::new();
    {
        let mut seen_sub = false;
        let mut literal = false;
        let mut args = rest.iter();
        while let Some(arg) = args.next() {
            if literal {
                if seen_sub {
                    named.push(arg.clone());
                } else {
                    seen_sub = true;
                    sub = normalize_sub(arg);
                }
            } else if arg == "--" {
                literal = true;
            } else if matches!(
                arg.as_str(),
                "--file" | "--max-wait" | "--sudo-service-user"
            ) {
                args.next();
            } else if !arg.starts_with('-') {
                if seen_sub {
                    named.push(arg.clone());
                } else {
                    seen_sub = true;
                    sub = normalize_sub(arg);
                }
            }
        }
    }
    if !matches!(sub, "list" | "info") {
        return super::unsupported("'services' is not native");
    }
    let json = rest.iter().any(|a| a == "--json");
    let verbose = rest
        .iter()
        .any(|a| matches!(a.as_str(), "--verbose" | "-v"));
    let all = rest.iter().any(|a| a == "--all");
    if !Path::new("/bin/launchctl").exists() {
        return super::failed(
            "Error: `brew services` is supported only on macOS or Linux (with systemd)!",
        );
    }
    let Some(uid) = uid() else {
        return super::failed("Error: could not determine the current user id");
    };
    let root = uid == 0;
    let home = home_dir();
    let ctx = Ctx {
        domains: if root {
            vec!["system".to_string()]
        } else {
            candidate_domains(uid)
        },
        user_path: home.join("Library/LaunchAgents"),
        boot_path: PathBuf::from("/Library/LaunchDaemons"),
        home,
        user: current_user(),
        root,
    };
    let api = Api::new(prefix.clone());
    let service_installed: Vec<(String, Option<Value>)> = prefix
        .installed(Kind::Formula)
        .iter()
        .filter_map(|installed| {
            let raw = raw_service_of(&api, &installed.name);
            let record_service = api
                .formula(&installed.name)
                .ok()
                .or_else(|| tap::lookup(prefix, &installed.name))
                .is_some_and(|f| f.service.is_some());
            (record_service || raw.is_some()).then(|| (installed.name.clone(), raw))
        })
        .collect();

    if sub == "list" {
        let mut rows: Vec<ServiceRow> = service_installed
            .iter()
            .map(|(name, raw)| ctx.row(prefix, name, &keg_base_for(prefix, name), raw.as_ref()))
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        if rows.is_empty() {
            if json {
                println!("[]");
            } else if tty::stderr_is_tty() {
                tty::opoo!("No services available to control with `brew services`");
            }
            return ExitCode::SUCCESS;
        }
        if json {
            println!("{}", to_json(&rows));
        } else {
            print!("{}", print_table(&rows, &ctx.home));
        }
        return ExitCode::SUCCESS;
    }

    // info: targets are --all (every available service) or the
    // named formulae, installed or not.
    let targets: Vec<(String, Option<Value>)> = if all {
        let mut all_targets = service_installed;
        all_targets.sort_by(|a, b| a.0.cmp(&b.0));
        all_targets
    } else {
        let mut targets = Vec::new();
        for name in &named {
            let known = api.formula(name).is_ok() || tap::lookup(prefix, name).is_some();
            if !known {
                return super::failed(&format!(
                    "Error: {}",
                    super::commands::formula_unavailable(&api, name)
                ));
            }
            targets.push((name.clone(), raw_service_of(&api, name)));
        }
        targets
    };
    let rows: Vec<ServiceRow> = targets
        .iter()
        .map(|(name, raw)| ctx.row(prefix, name, &keg_base_for(prefix, name), raw.as_ref()))
        .collect();
    if json {
        println!("{}", to_info_json(&rows));
    } else {
        for row in &rows {
            print!("{}", info_output(row, verbose));
        }
    }
    ExitCode::SUCCESS
}

fn normalize_sub(arg: &str) -> &'static str {
    match arg {
        "ls" | "list" => "list",
        "i" | "info" => "info",
        "cleanup" => "cleanup",
        "kill" => "kill",
        "restart" => "restart",
        "run" => "run",
        "start" => "start",
        "stop" => "stop",
        _ => "list",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, status: &'static str, user: Option<&str>, loaded: bool) -> ServiceRow {
        ServiceRow {
            name: name.to_string(),
            service_name: format!("sh.brew.{name}"),
            running: status == "started",
            schedulable: false,
            pid: None,
            status,
            user: user.map(str::to_string),
            file: PathBuf::from(format!(
                "/Users//x/Library/LaunchAgents/sh.brew.{name}.plist"
            )),
            exit_code: None,
            loaded,
            registered: loaded,
            loaded_file: None,
            details: None,
        }
    }

    #[test]
    fn table_matches_brew_column_math() {
        let rows = vec![
            row("colima", "started", Some("tester"), true),
            row("emacs", "started", Some("tester"), true),
        ];
        let table = print_table(&rows, Path::new("/Users//x"));
        assert_eq!(
            table,
            "Name   Status User   File\n\
             colima started         tester ~/Library/LaunchAgents/sh.brew.colima.plist\n\
             emacs  started         tester ~/Library/LaunchAgents/sh.brew.emacs.plist\n"
        );
    }

    #[test]
    fn unloaded_rows_hide_the_file() {
        let rows = vec![row("mysql", "none", None, false)];
        let table = print_table(&rows, Path::new("/Users//x"));
        assert!(table.contains("mysql none            "), "{table}");
        assert!(!table.contains("plist"), "{table}");
    }

    #[test]
    fn json_shape_and_key_order() {
        let rows = vec![row("colima", "started", Some("tester"), true)];
        let json = to_json(&rows);
        assert_eq!(
            json,
            "[\n  {\n    \"name\": \"colima\",\n    \"status\": \"started\",\n    \
             \"user\": \"tester\",\n    \"file\": \"/Users//x/Library/LaunchAgents/sh.brew.colima.plist\",\n    \
             \"exit_code\": null\n  }\n]"
        );
    }

    #[test]
    fn status_numbers_both_launchctl_forms() {
        assert_eq!(
            status_numbers("state = running\n\tpid = 8123\n", true),
            (Some(8123), None)
        );
        assert_eq!(
            status_numbers("\"PID\" = 42;\n\"LastExitStatus\" = 0;", false),
            (Some(42), Some(0))
        );
        assert_eq!(status_numbers("last exit code = 2", true), (None, Some(2)));
    }

    #[test]
    fn status_symbol_precedence() {
        assert_eq!(
            status_symbol(Some(9), true, Some(1), false, false),
            "started"
        );
        assert_eq!(status_symbol(None, false, None, false, true), "none");
        assert_eq!(status_symbol(None, true, Some(0), true, false), "scheduled");
        assert_eq!(status_symbol(None, true, Some(0), false, false), "stopped");
        assert_eq!(status_symbol(None, true, Some(3), false, false), "error");
        assert_eq!(status_symbol(None, true, None, false, true), "unknown");
        assert_eq!(status_symbol(Some(0), true, None, false, false), "other");
    }

    #[test]
    fn plist_names_default_and_explicit() {
        assert_eq!(
            plist_names("mysql", None),
            vec![
                "sh.brew.mysql".to_string(),
                "homebrew.mxcl.mysql".to_string()
            ]
        );
        let raw = aterm_json::json!({"name": {"macos": "com.example.d", "linux": "d"}});
        assert_eq!(
            plist_names("d", Some(&raw)),
            vec!["com.example.d".to_string()]
        );
    }

    #[test]
    fn info_text_and_json_render() {
        let mut r = row("colima", "started", Some("tester"), true);
        r.pid = Some(79468);
        r.registered = true;
        r.loaded_file = Some(r.file.display().to_string());
        r.details = Some(ServiceDetails {
            command: "/opt/homebrew/opt/colima/bin/colima start -f".to_string(),
            working_dir: Some("/Users//x".to_string()),
            log_path: Some("/opt/homebrew/var/log/colima.log".to_string()),
            error_log_path: Some("/opt/homebrew/var/log/colima.log".to_string()),
            ..ServiceDetails::default()
        });
        assert_eq!(
            info_output(&r, false),
            "colima (sh.brew.colima)\nRunning: true\nLoaded: true\nSchedulable: false\nUser: tester\nPID: 79468\n"
        );
        let v = info_output(&r, true);
        assert!(
            v.contains("File: /Users//x/Library/LaunchAgents/sh.brew.colima.plist true\n"),
            "{v}"
        );
        assert!(v.contains("Registered at login: true\n"), "{v}");
        assert!(
            v.contains("Command: /opt/homebrew/opt/colima/bin/colima start -f\n"),
            "{v}"
        );
        let plain = row("hello", "none", None, false);
        assert_eq!(
            info_output(&plain, false),
            "hello (sh.brew.hello)\nRunning: false\nLoaded: false\nSchedulable: false\n"
        );
        let json = to_info_json(&[r]);
        assert!(
            json.contains("\"service_name\": \"sh.brew.colima\""),
            "{json}"
        );
        assert!(json.contains("\"pid\": 79468"), "{json}");
        assert!(json.contains("\"root_dir\": null"), "{json}");
        assert!(json.contains("\"interval\": null"), "{json}");
    }

    #[test]
    fn info_sh_quote_and_manual_command() {
        assert_eq!(sh_quote("/opt/x/bin/tool"), "/opt/x/bin/tool");
        assert_eq!(sh_quote("with space"), "'with space'");
        assert_eq!(sh_quote(""), "''");
        let raw = aterm_json::json!({
            "run": ["/bin/d", "--flag"],
            "environment_variables": {"PATH": "/bin", "TOKEN": "a b"}
        });
        let cmd = manual_command(
            "d",
            &["/bin/d".into(), "--flag".into()],
            &raw,
            &|t: &str| t.to_string(),
        );
        assert_eq!(cmd, "TOKEN=\"a b\" /bin/d --flag");
    }

    #[test]
    fn info_loaded_file_from_print_output() {
        assert_eq!(
            loaded_file_of("x = {\n\tpath = /Users//x/a.plist\n", true).as_deref(),
            Some("/Users//x/a.plist")
        );
        assert_eq!(loaded_file_of("anything", false), None);
    }

    #[test]
    fn plist_string_reads_label_and_user() {
        let dir = std::env::temp_dir().join(format!("abrew-svc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.plist");
        std::fs::write(
            &file,
            "<plist><dict><key>Label</key>\n<string>sh.brew.x</string>\
             <key>UserName</key><string>root</string></dict></plist>",
        )
        .unwrap();
        assert_eq!(plist_string(&file, "Label").as_deref(), Some("sh.brew.x"));
        assert_eq!(plist_string(&file, "UserName").as_deref(), Some("root"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
