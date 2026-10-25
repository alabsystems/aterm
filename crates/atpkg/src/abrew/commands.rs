// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use std::cmp::Ordering;
use std::process::ExitCode;

use super::api::Api;
use super::formula::{Formula, version_cmp};
use super::install::{self, InstallOpts};
use super::prefix::{Installed, Kind, Prefix};
use super::{bundle, cask, fetch, from_install, has_flag, not_native, operands, stub, tap};

/// Return true when no local survey found `name` and the API confirmed it does not exist.
pub(super) fn package_definitively_unknown(
    prefix: &Prefix,
    api: &Api,
    name: &str,
    formula: bool,
    cask: bool,
) -> bool {
    if formula {
        if prefix.installed_one(Kind::Formula, name).is_some()
            || tap::lookup(prefix, name).is_some()
        {
            return false;
        }
        match api.formula(name) {
            Ok(_) => return false,
            Err(e) if e.starts_with("abrew: no such formula:") => {}
            Err(_) => return false,
        }
    }
    if cask {
        if prefix
            .installed_one(Kind::Cask, super::api::canonical_cask_token(name))
            .is_some()
        {
            return false;
        }
        match api.cask(name) {
            Ok(_) => return false,
            Err(e) if e.starts_with("abrew: no such cask:") => {}
            Err(_) => return false,
        }
    }
    formula || cask
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn suggestion(api: &Api, name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let mut best: Option<(usize, String)> = None;
    for casks in [false, true] {
        let Ok(rows) = api.index(casks) else {
            continue;
        };
        for (candidate, _) in rows {
            if candidate == name {
                continue;
            }
            let d = edit_distance(&lower, &candidate.to_lowercase());
            if d <= 2
                && best
                    .as_ref()
                    .is_none_or(|(bd, bn)| d < *bd || (d == *bd && candidate < *bn))
            {
                best = Some((d, candidate));
            }
        }
    }
    best.map(|(_, n)| n)
}

fn no_such(api: &Api, name: &str, kind: Option<&str>) -> String {
    let mut msg = match kind {
        Some(k) => format!("abrew: no such {k}: {name}"),
        None => format!("abrew: no formula or cask named \"{name}\""),
    };
    if let Some(s) = suggestion(api, name) {
        msg += &format!("\nDid you mean \"{s}\"?");
    }
    msg
}

fn failed(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("abrew: {msg}");
    ExitCode::from(2)
}

pub(super) fn cmd_on(prefix: &Prefix) -> ExitCode {
    if let Err(e) = prefix.ensure() {
        return failed(&e);
    }
    match std::fs::remove_file(prefix.disabled_path()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return failed(&format!("abrew: cannot remove off marker: {error}")),
    }
    if let Some(layout) = crate::store::resolve_from(crate::config::cached())
        && let Err(e) = stub::lay(&layout)
    {
        eprintln!("abrew: stub not laid: {e}");
    }
    if crate::config::cached_abrew().enabled() {
        println!("abrew: on — `brew` in aterm sessions uses atpkg's native compatibility");
    } else {
        println!("abrew: on for this prefix, but [abrew] enabled=false still disables native brew");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_off(prefix: &Prefix) -> ExitCode {
    if let Err(e) = prefix.ensure() {
        return failed(&e);
    }
    if let Err(e) = super::prefix::write_metadata(&prefix.disabled_path(), b"off\n") {
        return failed(&format!("abrew: {e}"));
    }
    if let Some(layout) = crate::store::resolve_from(crate::config::cached())
        && let Err(error) = stub::lay(&layout)
    {
        return failed(&format!(
            "abrew: native brew is off, but its managed wrapper could not be refreshed: {error}"
        ));
    }
    println!("abrew: off — native brew commands are disabled until `abrew on`");
    ExitCode::SUCCESS
}

pub(super) fn cmd_status(prefix: &Prefix) -> ExitCode {
    println!("prefix: {}", prefix.root.display());
    println!(
        "reroute: {}",
        if !crate::config::cached_abrew().enabled() {
            "off ([abrew] enabled=false)"
        } else if prefix.is_disabled() {
            "off"
        } else {
            "on"
        }
    );
    println!("implementation: atpkg native (no Homebrew fallback)");
    bundle::bundle_status(prefix);
    println!(
        "installed: {} formulae, {} casks",
        prefix.installed(Kind::Formula).len(),
        prefix.installed(Kind::Cask).len()
    );
    ExitCode::SUCCESS
}

/// Extract the inner pattern from brew's `/regex/` search syntax.
pub(super) fn search_regex_pattern(joined: &str) -> Option<&str> {
    if joined.contains('\n') {
        return None;
    }
    joined.strip_prefix('/')?.strip_suffix('/')
}

/// Compile a `/regex/` search pattern under bounded regex limits.
pub(super) fn compile_search_regex(
    pattern: &str,
) -> Result<aterm_regex::Regex, aterm_regex::Error> {
    aterm_regex::RegexBuilder::new(pattern)
        .size_limit(128 * 1024)
        .step_limit(1 << 22)
        .build()
}

enum SearchQuery {
    Text(String),
    Regex(aterm_regex::Regex),
}

fn simplify_search_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '@' || *c == '+')
        .collect()
}

impl SearchQuery {
    fn text(raw: &str) -> Self {
        SearchQuery::Text(simplify_search_text(raw))
    }

    fn hits_any(&self, fields: &[&str]) -> Result<bool, aterm_regex::StepLimitExceeded> {
        match self {
            SearchQuery::Text(q) => Ok(fields
                .iter()
                .any(|f| simplify_search_text(f).contains(q.as_str()))),
            SearchQuery::Regex(re) => {
                for f in fields {
                    if re.try_is_match(f)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    fn hits(
        &self,
        name: &str,
        desc: &str,
        want_desc: bool,
    ) -> Result<bool, aterm_regex::StepLimitExceeded> {
        if want_desc {
            self.hits_any(&[name, desc])
        } else {
            self.hits_any(&[name])
        }
    }
}

pub(super) fn cmd_search(api: &Api, rest: &[String]) -> ExitCode {
    let joined = operands(rest).join(" ");
    if joined.is_empty() {
        return usage("search needs a term");
    }
    let desc = has_flag(rest, "--desc", "");
    let (query, display) = match search_regex_pattern(&joined) {
        Some(pattern) => match compile_search_regex(pattern) {
            Ok(re) => (SearchQuery::Regex(re), joined.clone()),
            Err(_) => return failed(&format!("abrew: {joined} is not a valid regex.")),
        },
        None => {
            let lowered = joined.to_ascii_lowercase();
            let canonical = super::api::canonical_cask_token(&joined);
            (SearchQuery::text(canonical), lowered)
        }
    };
    let mut shown = 0usize;
    for casks in [false, true] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        match api.index(casks) {
            Ok(rows) => {
                for (name, d) in rows {
                    match query.hits(&name, &d, desc) {
                        Ok(true) => {
                            if casks {
                                println!("{name} (cask)");
                            } else {
                                println!("{name}");
                            }
                            shown += 1;
                            if shown >= 200 {
                                println!("… (first 200 matches)");
                                return ExitCode::SUCCESS;
                            }
                        }
                        Ok(false) => {}
                        Err(_) => {
                            return failed(
                                "abrew: native regex search ran out of its work budget; simplify the pattern",
                            );
                        }
                    }
                }
            }
            Err(e) => return failed(&e),
        }
    }
    if shown == 0 {
        if desc {
            println!("No formulae or casks matching \"{display}\".");
        } else {
            eprintln!("Error: No formulae or casks found for \"{display}\".");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

fn keg_abv(dir: &std::path::Path) -> String {
    let mut count = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            count += 1;
            bytes = bytes.saturating_add(meta.len());
        }
    }
    format!(
        "{count} file{}, {}",
        if count == 1 { "" } else { "s" },
        disk_usage_readable(bytes)
    )
}

fn formula_source(prefix: &Prefix, f: &Formula) -> (String, Option<String>) {
    let parts: Vec<&str> = f.full_name.split('/').collect();
    if parts.len() == 3 {
        let (user, repo) = (parts[0], parts[1]);
        let official = user == "homebrew" && matches!(repo, "core" | "cask");
        if !official {
            if let Some(t) = tap::list(prefix)
                .into_iter()
                .find(|t| t.user == user && t.repo == repo)
                && let Some(file) = tap::formula_file(&t, &f.name)
            {
                return (file.display().to_string(), Some(t.name()));
            }
            return (
                format!(
                    "https://github.com/{user}/homebrew-{repo}/blob/HEAD/Formula/{}.rb",
                    f.name
                ),
                Some(format!("{user}/{repo}")),
            );
        }
    }
    (
        format!(
            "https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/{}/{}.rb",
            core_shard(&f.name),
            f.name
        ),
        None,
    )
}

fn print_formula_info(prefix: &Prefix, api: &Api, f: &Formula) {
    let inst = prefix.installed_one(Kind::Formula, &f.name);
    let outdated = inst
        .as_ref()
        .is_some_and(|i| version_cmp(&i.version, &f.version) == Ordering::Less);
    let bottled = !f.bottle_files.is_empty();
    let stable = if bottled {
        format!("stable {} (bottled)", f.version)
    } else {
        format!("stable {}", f.version)
    };
    let spec = if outdated {
        let linked = inst
            .as_ref()
            .and_then(|i| linked_keg_version(prefix, i))
            .unwrap_or_else(|| inst.as_ref().map(|i| i.version.clone()).unwrap_or_default());
        format!("{linked} → {stable}")
    } else {
        stable
    };
    println!(
        "==> {}: {spec}{}",
        f.full_name,
        if f.keg_only { " [keg-only]" } else { "" }
    );
    if !f.desc.is_empty() {
        println!("{}", f.desc);
    }
    if !f.homepage.is_empty() {
        println!("{}", f.homepage);
    }
    if !f.aliases.is_empty() {
        println!("Aliases: {}", f.aliases.join(", "));
    }
    if !f.oldnames.is_empty() {
        println!("Old Names: {}", f.oldnames.join(", "));
    }
    if f.deprecate {
        println!("Deprecated!");
    } else if f.disable {
        println!("Disabled!");
    }
    if !f.conflicts.is_empty() {
        let mut names = f.conflicts.clone();
        names.sort();
        println!("Conflicts with:");
        for name in names {
            println!("  {name}");
        }
    }
    match &inst {
        Some(i) => {
            let keg = prefix.keg(&f.name, &i.version);
            let on_request = super::prefix::receipt_word(&keg, "installed_on_request").as_deref()
                == Some("true");
            println!(
                "Installed ({})",
                if on_request {
                    "on request"
                } else {
                    "as dependency"
                }
            );
        }
        None => println!("Not installed"),
    }
    let (from, tap_name) = formula_source(prefix, f);
    println!("From: {from}");
    if let Some(tap_name) = tap_name {
        println!("Tap: {tap_name}");
    }
    if !f.license.is_empty() {
        println!("License: {}", f.license);
    }
    if let Some(i) = &inst {
        let linked_version = linked_keg_version(prefix, i);
        let mut rows: Vec<(String, String, bool)> = i
            .versions
            .iter()
            .rev()
            .enumerate()
            .filter(|(idx, v)| *idx == 0 || linked_version.as_deref() == Some(v.as_str()))
            .map(|(_, v)| {
                let shown = if outdated && v == &i.version {
                    format!("{v} → {}", f.version)
                } else {
                    v.clone()
                };
                (
                    shown,
                    keg_abv(&prefix.keg(&f.name, v)),
                    linked_version.as_deref() == Some(v.as_str()),
                )
            })
            .collect();
        if !rows.is_empty() {
            println!("==> Installed Versions");
            let vw = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
            let sw = rows.iter().map(|r| r.1.len() + 2).max().unwrap_or(0);
            for (version, size, linked) in rows.drain(..) {
                let size = format!("({size})");
                if linked {
                    println!(
                        "{} {:<vw$} {:<sw$} [Linked]",
                        f.full_name,
                        version,
                        size,
                        vw = vw,
                        sw = sw
                    );
                } else {
                    println!("{} {:<vw$} {}", f.full_name, version, size, vw = vw);
                }
            }
        }
    }
    let mut dep_lines = Vec::new();
    if !bottled && !f.build_dependencies.is_empty() {
        dep_lines.push(format!(
            "Build ({}): {}",
            f.build_dependencies.len(),
            f.build_dependencies.join(", ")
        ));
    }
    if !f.dependencies.is_empty() {
        dep_lines.push(format!(
            "Required ({}): {}",
            f.dependencies.len(),
            f.dependencies.join(", ")
        ));
    }
    let closure = if inst.is_some() {
        flat_dependencies(f, false, |n| api.formula(n).ok())
    } else {
        std::collections::BTreeSet::new()
    };
    if !dep_lines.is_empty() || !closure.is_empty() {
        println!("==> Dependencies");
        for line in dep_lines {
            println!("{line}");
        }
        if !closure.is_empty() {
            let installed_count = closure
                .iter()
                .filter(|d| prefix.installed_one(Kind::Formula, d).is_some())
                .count();
            let counts = if installed_count == closure.len() {
                "all installed ✔".to_string()
            } else {
                format!(
                    "{installed_count} installed ✔, {} missing ✘",
                    closure.len() - installed_count
                )
            };
            println!("Recursive Runtime ({}): {counts}", closure.len());
        }
    }
    if !f.caveats.is_empty() {
        println!("==> Caveats");
        println!("{}", f.caveats.trim());
    }
}

fn keg_time(dir: &std::path::Path) -> Option<u64> {
    let meta = std::fs::metadata(dir.join("INSTALL_RECEIPT.json"))
        .or_else(|_| std::fs::metadata(dir))
        .ok()?;
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn enrich_formula_json(prefix: &Prefix, api: &Api, value: &mut aterm_json::Value) {
    let aterm_json::Value::Object(map) = value else {
        return;
    };
    let Some(name) = map
        .get("name")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let parsed = api.formula(&name).ok();
    let runtime: Vec<aterm_json::Value> = match &parsed {
        Some(f) => {
            let closure = flat_dependencies(f, false, |n| api.formula(n).ok());
            closure
                .iter()
                .filter_map(|dep| {
                    let inst = prefix.installed_one(Kind::Formula, dep)?;
                    let mut row = aterm_json::Map::new();
                    row.insert(
                        "full_name".to_string(),
                        aterm_json::Value::String(prefix.full_name(&inst)),
                    );
                    row.insert(
                        "pkg_version".to_string(),
                        aterm_json::Value::String(inst.version.clone()),
                    );
                    row.insert(
                        "declared_directly".to_string(),
                        aterm_json::Value::Bool(f.dependencies.iter().any(|d| d == dep)),
                    );
                    Some(aterm_json::Value::Object(row))
                })
                .collect()
        }
        None => Vec::new(),
    };
    let mut installed_rows = Vec::new();
    let mut linked = aterm_json::Value::Null;
    let mut outdated = false;
    if let Some(inst) = prefix.installed_one(Kind::Formula, &name) {
        for version in &inst.versions {
            let keg = prefix.keg(&name, version);
            let sbom = keg.join("sbom.spdx.json");
            let mut row = aterm_json::Map::new();
            row.insert(
                "sbom".to_string(),
                if sbom.is_file() {
                    aterm_json::Value::String(sbom.to_string_lossy().into_owned())
                } else {
                    aterm_json::Value::Null
                },
            );
            row.insert(
                "version".to_string(),
                aterm_json::Value::String(version.clone()),
            );
            row.insert(
                "used_options".to_string(),
                aterm_json::Value::Array(Vec::new()),
            );
            row.insert("built_as_bottle".to_string(), aterm_json::Value::Bool(true));
            row.insert(
                "poured_from_bottle".to_string(),
                aterm_json::Value::Bool(true),
            );
            row.insert(
                "time".to_string(),
                keg_time(&keg).map_or(aterm_json::Value::Null, aterm_json::Value::from),
            );
            row.insert(
                "runtime_dependencies".to_string(),
                aterm_json::Value::Array(runtime.clone()),
            );
            row.insert(
                "installed_on_request".to_string(),
                aterm_json::Value::Bool(
                    super::prefix::receipt_word(&keg, "installed_on_request").as_deref()
                        == Some("true"),
                ),
            );
            installed_rows.push(aterm_json::Value::Object(row));
        }
        if let Some(v) = linked_keg_version(prefix, &inst) {
            linked = aterm_json::Value::String(v);
        }
        if let Some(f) = &parsed {
            outdated = version_cmp(&inst.version, &f.version) == Ordering::Less;
        }
    }
    map.insert(
        "installed".to_string(),
        aterm_json::Value::Array(installed_rows),
    );
    map.insert("linked_keg".to_string(), linked);
    map.insert(
        "pinned".to_string(),
        aterm_json::Value::Bool(prefix.is_pinned(&name)),
    );
    map.insert("outdated".to_string(), aterm_json::Value::Bool(outdated));
}

fn enrich_cask_json(prefix: &Prefix, value: &mut aterm_json::Value) {
    let aterm_json::Value::Object(map) = value else {
        return;
    };
    let Some(token) = map
        .get("token")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let latest = map
        .get("version")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string);
    let auto_updates = map
        .get("auto_updates")
        .and_then(aterm_json::Value::as_bool)
        .unwrap_or(false);
    let (installed, installed_time, outdated) = match prefix.installed_one(Kind::Cask, &token) {
        Some(inst) => {
            let dir = prefix.cask_dir(&token, &inst.version);
            let outdated = !auto_updates
                && latest
                    .as_deref()
                    .is_some_and(|l| version_cmp(&inst.version, l) == Ordering::Less);
            (
                aterm_json::Value::String(inst.version.clone()),
                keg_time(&dir).map_or(aterm_json::Value::Null, aterm_json::Value::from),
                outdated,
            )
        }
        None => (aterm_json::Value::Null, aterm_json::Value::Null, false),
    };
    map.insert("installed".to_string(), installed);
    map.insert("installed_time".to_string(), installed_time);
    map.insert("pinned".to_string(), aterm_json::Value::Bool(false));
    map.insert("pinned_version".to_string(), aterm_json::Value::Null);
    map.insert("outdated".to_string(), aterm_json::Value::Bool(outdated));
}

fn info_json(
    prefix: &Prefix,
    api: &Api,
    rest: &[String],
    names: &[String],
) -> Result<String, String> {
    let v2 = has_flag(rest, "--json=v2", "");
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let installed = has_flag(rest, "--installed", "");
    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    let mut fname: Vec<String> = names.to_vec();
    let mut cname: Vec<String> = names.to_vec();
    if installed {
        fname = prefix
            .installed(Kind::Formula)
            .into_iter()
            .map(|i| i.name)
            .collect();
        cname = prefix
            .installed(Kind::Cask)
            .into_iter()
            .map(|i| i.name)
            .collect();
    }
    let mut resolved = std::collections::BTreeSet::new();
    if !want_cask {
        for n in &fname {
            match api.formula_value(n) {
                Ok(v) => {
                    resolved.insert(Formula::short_name(n).to_string());
                    formulae.push(v);
                }
                Err(_) if want_formula || installed => {
                    return Err(format!("abrew: no formula {n}"));
                }
                Err(_) => {}
            }
        }
    }
    if !want_formula {
        for n in &cname {
            let short = Formula::short_name(n);
            if !installed
                && !want_cask
                && formulae
                    .iter()
                    .any(|v| v.get("name").and_then(aterm_json::Value::as_str) == Some(short))
            {
                continue;
            }
            match api.cask_value(n) {
                Ok(v) => {
                    resolved.insert(short.to_string());
                    casks.push(v);
                }
                Err(e) => {
                    if want_cask || installed {
                        return Err(e);
                    }
                }
            }
        }
    }
    if !installed {
        for n in names {
            if !resolved.contains(Formula::short_name(n)) {
                return Err(no_such(api, n, None));
            }
        }
    }
    for v in &mut formulae {
        enrich_formula_json(prefix, api, v);
    }
    for v in &mut casks {
        enrich_cask_json(prefix, v);
    }
    let value = if v2 {
        let mut map = aterm_json::Map::new();
        map.insert("formulae".to_string(), aterm_json::Value::Array(formulae));
        map.insert("casks".to_string(), aterm_json::Value::Array(casks));
        aterm_json::Value::Object(map)
    } else {
        aterm_json::Value::Array(formulae.into_iter().chain(casks).collect())
    };
    aterm_json::to_string(&value).map_err(|e| format!("abrew: {e}"))
}

/// Read one unambiguous `--flag value` or `--flag=value` option.
pub(super) fn flag_value(rest: &[String], flag: &str) -> Result<Option<String>, ()> {
    let mut found = None;
    let mut it = rest.iter();
    while let Some(s) = it.next() {
        let value = if s == flag {
            it.next().map(String::as_str)
        } else if let Some(v) = s.strip_prefix(&format!("{flag}=")) {
            Some(v)
        } else {
            continue;
        };
        let value = value
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or(())?;
        if found.replace(value.to_string()).is_some() {
            return Err(());
        }
    }
    Ok(found)
}

/// Return positional operands with valued flags' arguments removed.
pub(super) fn operands_without_flag_values(rest: &[String], valued: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = rest.iter();
    while let Some(s) = it.next() {
        if valued.contains(&s.as_str()) {
            let _ = it.next();
        } else if !s.starts_with('-') {
            out.push(s.clone());
        }
    }
    out
}

fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn analytics_table(category: &str, days: &str, rows: &[(String, u64)], width: usize) -> String {
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let total_count = format_count(total);
    let index_width = [5, rows.len().to_string().len()]
        .into_iter()
        .max()
        .unwrap_or(5);
    let count_width = [5, total_count.len()].into_iter().max().unwrap_or(5);
    let percent_width = 7usize;
    let name_width = width
        .saturating_sub(index_width + count_width + percent_width + 10)
        .max(1);
    let trunc = |s: &str| -> String { s.chars().take(name_width).collect() };
    let mut out = String::new();
    out += &format!("==> {category} ({days} days)\n");
    out += &format!(
        "{:>index_width$} | {:<name_width$} | {:>count_width$} |  {:>percent_width$}\n",
        "Index",
        trunc("Name (with options)"),
        "Count",
        "Percent",
    );
    out += &format!(
        "{}:|-{}-|-{}:|-{}:\n",
        "-".repeat(index_width),
        "-".repeat(name_width),
        "-".repeat(count_width),
        "-".repeat(percent_width),
    );
    let max_index = rows.len().to_string().len();
    for (i, (name, count)) in rows.iter().enumerate() {
        let percent = if total == 0 {
            0.0
        } else {
            (*count as f64 * 100.0) / total as f64
        };
        out += &format!(
            "{:<index_width$} | {:<name_width$} | {:>count_width$} | {:>percent_width$}%\n",
            format!("{:0max_index$}", i + 1),
            trunc(name),
            format_count(*count),
            format!("{percent:.2}"),
        );
    }
    if rows.len() > 1 {
        out += &format!(
            "{:<index_width$} | {:<name_width$} | {:>count_width$} | {:>percent_width$}%\n",
            "Total", "", total_count, "100.00",
        );
    }
    out
}

fn info_analytics(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands_without_flag_values(rest, &["--days", "--category"]);
    if names.is_empty() {
        return not_native(prefix, args, "info --analytics (global form)");
    }
    let (Ok(days_filter), Ok(category_filter)) =
        (flag_value(rest, "--days"), flag_value(rest, "--category"))
    else {
        return not_native(prefix, args, "info --analytics (invalid options)");
    };
    let mut docs: Vec<Option<aterm_json::Value>> = Vec::new();
    for name in &names {
        let short = Formula::short_name(name);
        let doc = if has_flag(rest, "--cask", "") {
            api.cask_value(short).ok()
        } else if has_flag(rest, "--formula", "") {
            match api.formula_value(name) {
                Ok(v) => Some(v),
                Err(_) if tap::lookup(prefix, name).is_some() => None,
                Err(_) => return not_native(prefix, args, "info --analytics (unresolved name)"),
            }
        } else {
            match api.formula_value(name) {
                Ok(v) => Some(v),
                Err(_) => match api.cask_value(short) {
                    Ok(v) => Some(v),
                    Err(_) if tap::lookup(prefix, name).is_some() => None,
                    Err(_) => {
                        return not_native(prefix, args, "info --analytics (unresolved name)");
                    }
                },
            }
        };
        if has_flag(rest, "--cask", "") && doc.is_none() {
            return not_native(prefix, args, "info --analytics (unresolved name)");
        }
        docs.push(doc);
    }
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w > 0)
        .unwrap_or(80);
    for (i, doc) in docs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let Some(doc) = doc else { continue };
        let Some(analytics) = doc.get("analytics").and_then(|a| a.as_object()) else {
            continue;
        };
        if analytics.is_empty() {
            continue;
        }
        println!("==> Analytics");
        let mut categories: Vec<&String> = ["install", "install_on_request", "build_error"]
            .iter()
            .filter_map(|c| analytics.get_key_value(*c).map(|(k, _)| k))
            .collect();
        categories.extend(
            analytics.keys().filter(|k| {
                !["install", "install_on_request", "build_error"].contains(&k.as_str())
            }),
        );
        for category in categories {
            let shown_category = category.replace('_', "-");
            if category_filter
                .as_deref()
                .is_some_and(|f| f != shown_category)
            {
                continue;
            }
            let Some(by_days) = analytics.get(category).and_then(|v| v.as_object()) else {
                continue;
            };
            let mut day_keys: Vec<(&String, u64)> = by_days
                .keys()
                .map(|k| {
                    (
                        k,
                        k.trim_end_matches('d').parse::<u64>().unwrap_or(u64::MAX),
                    )
                })
                .collect();
            day_keys.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
            for (day_key, days) in day_keys {
                if days_filter
                    .as_deref()
                    .is_some_and(|f| f.parse::<u64>().ok() != Some(days))
                {
                    continue;
                }
                let Some(results) = by_days.get(day_key).and_then(|v| v.as_object()) else {
                    continue;
                };
                let mut rows: Vec<(String, u64)> = results
                    .iter()
                    .map(|(n, c)| (n.clone(), c.as_u64().unwrap_or(0)))
                    .collect();
                rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                print!(
                    "{}",
                    analytics_table(&shown_category, &days.to_string(), &rows, width)
                );
            }
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_info(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    if has_flag(rest, "--analytics", "") {
        return info_analytics(prefix, api, args, rest);
    }
    let names = operands(rest);
    if rest
        .iter()
        .any(|a| matches!(a.as_str(), "--json" | "--json=v1" | "--json=v2"))
    {
        if names.is_empty() && !has_flag(rest, "--installed", "") {
            return usage("info --json needs a name or --installed");
        }
        return match info_json(prefix, api, rest, &names) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => failed(&e),
        };
    }
    let _ = args;
    if names.is_empty() {
        println!(
            "{} formulae, {} casks installed under {}",
            prefix.installed(Kind::Formula).len(),
            prefix.installed(Kind::Cask).len(),
            prefix.root.display()
        );
        return ExitCode::SUCCESS;
    }
    let mut failures = 0;
    let mut printed = false;
    for (idx, name) in names.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        if has_flag(rest, "--cask", "") {
            match api.cask(name) {
                Ok(c) => {
                    print_cask_info(prefix, api, &c);
                    printed = true;
                }
                Err(_) => {
                    eprintln!(
                        "{}",
                        if api.formula(name).is_ok() {
                            format!(
                                "abrew: no such cask: {} (it is a formula; drop --cask)",
                                Formula::short_name(name)
                            )
                        } else {
                            no_such(api, name, Some("cask"))
                        }
                    );
                    failures += 1;
                }
            }
            continue;
        }
        match api.formula(name) {
            Ok(f) => {
                print_formula_info(prefix, api, &f);
                printed = true;
            }
            Err(_) => match tap::lookup(prefix, name) {
                Some(f) => {
                    print_formula_info(prefix, api, &f);
                    printed = true;
                }
                None => {
                    if has_flag(rest, "--formula", "") {
                        eprintln!(
                            "{}",
                            if api.cask(Formula::short_name(name)).is_ok() {
                                format!(
                                    "abrew: no such formula: {} (it is a cask; drop --formula)",
                                    Formula::short_name(name)
                                )
                            } else {
                                no_such(api, name, Some("formula"))
                            }
                        );
                        failures += 1;
                        continue;
                    }
                    if name.contains('/') {
                        if !printed {
                            return not_native(prefix, args, "info (unresolved name)");
                        }
                        eprintln!("{}", no_such(api, name, None));
                        failures += 1;
                        continue;
                    }
                    match api.cask(name) {
                        Ok(c) => {
                            print_cask_info(prefix, api, &c);
                            printed = true;
                        }
                        Err(_) => {
                            eprintln!("{}", no_such(api, name, None));
                            failures += 1;
                        }
                    }
                }
            },
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn artifact_line(a: &super::formula::Artifact) -> Option<String> {
    use super::formula::Artifact;
    let (source, target, label): (&str, Option<&str>, &str) = match a {
        Artifact::App(s, t) => (s, t.as_deref(), "App"),
        Artifact::Binary(s, t) => (s, t.as_deref(), "Binary"),
        Artifact::Font(s, t) => (s, t.as_deref(), "Font"),
        Artifact::Pkg(s) => (s, None, "Pkg"),
        Artifact::Manpage(s, t) => (s, t.as_deref(), "Manpage"),
        Artifact::Suite(s, t) => (s, t.as_deref(), "App Suite"),
        Artifact::KeyboardLayout(s, t) => (s, t.as_deref(), "Keyboard Layout"),
        Artifact::ScreenSaver(s, t) => (s, t.as_deref(), "Screen Saver"),
        Artifact::PrefPane(s, t) => (s, t.as_deref(), "Preference Pane"),
        Artifact::Service(s, t) => (s, t.as_deref(), "Service"),
        Artifact::Generic(s, t) => (s, t.as_deref(), "Generic Artifact"),
        Artifact::Wrapper(name, _, _, t) => (name, t.as_deref(), "Command Wrapper"),
        Artifact::Library(s, t, dir) => (s, t.as_deref(), library_label(dir)),
        Artifact::Other(_) => return None,
    };
    let basename = source.rsplit('/').next().unwrap_or(source);
    let shown = match target {
        Some(t) if matches!(a, Artifact::Generic(..)) || t != basename => {
            format!("{source} -> {t}")
        }
        _ => source.to_string(),
    };
    Some(format!("{shown} ({label})"))
}

fn library_label(dir: &str) -> &'static str {
    match dir {
        "Library/QuickLook" => "Quick Look Plugin",
        "Library/ColorPickers" => "Colorpicker",
        "Library/Dictionaries" => "Dictionary",
        "Library/Input Methods" => "Input Method",
        "Library/Internet Plug-Ins" => "Internet Plugin",
        "Library/Spotlight" => "Spotlight metadata importer",
        "Library/Audio/Plug-Ins/Components" => "Audio Unit Plugin",
        "Library/Audio/Plug-Ins/VST" => "VST Plugin",
        "Library/Audio/Plug-Ins/VST3" => "VST3 Plugin",
        "share/bash-completion/completions" => "Bash Completion",
        "share/zsh/site-functions" => "Zsh Completion",
        "share/fish/vendor_completions.d" => "Fish Completion",
        _ => "Artifact",
    }
}

fn print_cask_info(prefix: &Prefix, api: &Api, c: &super::formula::Cask) {
    let inst = prefix.installed_one(Kind::Cask, &c.token);
    let outdated = !c.auto_updates
        && inst
            .as_ref()
            .is_some_and(|i| version_cmp(&i.version, &c.version) == Ordering::Less);
    let version = if outdated {
        format!(
            "{} → {}",
            inst.as_ref().map(|i| i.version.clone()).unwrap_or_default(),
            c.version
        )
    } else {
        c.version.clone()
    };
    println!(
        "==> {}{}: {version}{}",
        c.token,
        if c.name.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.name)
        },
        if c.auto_updates {
            " (auto_updates)"
        } else {
            ""
        }
    );
    if !c.desc.is_empty() {
        println!("{}", c.desc);
    }
    if !c.homepage.is_empty() {
        println!("{}", c.homepage);
    }
    if c.deprecated {
        println!("Deprecated!");
    } else if c.disabled {
        println!("Disabled!");
    }
    match &inst {
        Some(i) => {
            let dir = prefix.cask_dir(&c.token, &i.version);
            let on_request = super::prefix::receipt_word(&dir, "installed_on_request").as_deref()
                == Some("true");
            println!(
                "Installed ({})",
                if on_request {
                    "on request"
                } else {
                    "as dependency"
                }
            );
            println!(
                "{} ({})",
                dir.display(),
                disk_usage_readable(dir_disk_usage(&dir))
            );
        }
        None => println!("Not installed"),
    }
    println!(
        "From: https://github.com/Homebrew/homebrew-cask/blob/HEAD/Casks/{}/{}.rb",
        cask_shard(&c.token),
        c.token
    );
    if !c.depends_formula.is_empty() || !c.depends_cask.is_empty() {
        let mut all: Vec<String> = c.depends_formula.clone();
        all.extend(c.depends_cask.iter().map(|t| format!("{t} (cask)")));
        println!("==> Dependencies");
        println!("Required ({}): {}", all.len(), all.join(", "));
        let mut recursive: Vec<(String, bool)> = Vec::new();
        for dep in &c.depends_formula {
            recursive.push((
                dep.clone(),
                prefix.installed_one(Kind::Formula, dep).is_some(),
            ));
            if let Ok(df) = api.formula(dep) {
                for sub in flat_dependencies(&df, false, |n| api.formula(n).ok()) {
                    recursive.push((
                        sub.clone(),
                        prefix.installed_one(Kind::Formula, &sub).is_some(),
                    ));
                }
            }
        }
        for dep in &c.depends_cask {
            recursive.push((dep.clone(), prefix.installed_one(Kind::Cask, dep).is_some()));
        }
        recursive.sort();
        recursive.dedup();
        if !recursive.is_empty() {
            let installed_count = recursive.iter().filter(|(_, ok)| *ok).count();
            let counts = if installed_count == recursive.len() {
                "all installed ✔".to_string()
            } else {
                format!(
                    "{installed_count} installed ✔, {} missing ✘",
                    recursive.len() - installed_count
                )
            };
            println!("Recursive Runtime ({}): {counts}", recursive.len());
        }
    }
    let mut required: Vec<String> = Vec::new();
    if let Some(min) = &c.macos_min {
        required.push(format!("macOS >= {min}"));
    }
    if let Some(exact) = &c.macos_exact {
        required.push(format!("macOS == {exact}"));
    }
    if let Some(arch) = &c.arch {
        required.push(arch.clone());
    }
    if !required.is_empty() {
        println!("==> Requirements");
        println!("Required: {}", required.join(", "));
    }
    let artifacts: Vec<String> = c.artifacts.iter().filter_map(artifact_line).collect();
    if !artifacts.is_empty() {
        println!("==> Artifacts");
        for line in artifacts {
            println!("{line}");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DescField {
    Name,
    Description,
    Either,
}

fn desc_matches(
    query: &SearchQuery,
    field: DescField,
    cask: bool,
    name: &str,
    display: &str,
    desc: &str,
) -> Result<bool, aterm_regex::StepLimitExceeded> {
    let fields: &[&str] = match (field, cask) {
        (DescField::Name, _) => &[name],
        (DescField::Description, false) => &[desc],
        (DescField::Description, true) => &[display, desc],
        (DescField::Either, false) => &[name, desc],
        (DescField::Either, true) => &[name, display, desc],
    };
    query.hits_any(fields)
}

fn desc_search(api: &Api, rest: &[String], field: DescField) -> ExitCode {
    let joined = operands(rest).join(" ");
    if joined.is_empty() {
        return usage("desc search needs a term");
    }
    let query = match search_regex_pattern(&joined) {
        Some(pattern) => match compile_search_regex(pattern) {
            Ok(re) => SearchQuery::Regex(re),
            Err(_) => return failed(&format!("abrew: {joined} is not a valid regex.")),
        },
        None => SearchQuery::text(super::api::canonical_cask_token(&joined)),
    };
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let both = !want_cask && !want_formula;
    for casks in [false, true] {
        if (want_cask && !casks) || (want_formula && casks) {
            continue;
        }
        let rows = match api.desc_index(casks) {
            Ok(rows) => rows,
            Err(e) => return failed(&e),
        };
        let mut hits: Vec<(String, String, String)> = Vec::new();
        for (name, display, desc) in rows {
            if desc.is_empty() {
                continue;
            }
            match desc_matches(&query, field, casks, &name, &display, &desc) {
                Ok(true) => hits.push((name, display, desc)),
                Ok(false) => {}
                Err(_) => {
                    return failed(
                        "abrew: native regex search ran out of its work budget; simplify the pattern",
                    );
                }
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        if casks {
            if both {
                println!();
            }
            println!("==> Casks");
        } else {
            println!("==> Formulae");
        }
        for (name, display, desc) in hits {
            if casks && !display.is_empty() {
                println!("{name}: ({display}) {desc}");
            } else {
                println!("{name}: {desc}");
            }
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_desc(api: &Api, rest: &[String]) -> ExitCode {
    let field = if has_flag(rest, "--search", "-s") {
        Some(DescField::Either)
    } else if has_flag(rest, "--name", "-n") {
        Some(DescField::Name)
    } else if has_flag(rest, "--description", "-d") {
        Some(DescField::Description)
    } else {
        None
    };
    if let Some(field) = field {
        return desc_search(api, rest, field);
    }
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let mut lines = Vec::new();
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !want_cask {
            if let Ok(f) = api.formula(&name) {
                lines.push(format!("{}: {}", f.name, f.desc));
                continue;
            }
            if want_formula {
                return failed(&if api.cask(short).is_ok() {
                    format!("abrew: no such formula: {short} (it is a cask; drop --formula)")
                } else {
                    no_such(api, &name, Some("formula"))
                });
            }
        }
        if !want_formula {
            if let Ok(c) = api.cask(short) {
                lines.push(if c.name.is_empty() {
                    format!("{}: {}", c.token, c.desc)
                } else {
                    format!("{}: ({}) {}", c.token, c.name, c.desc)
                });
                continue;
            }
            if want_cask {
                return failed(&if api.formula(&name).is_ok() {
                    format!("abrew: no such cask: {short} (it is a formula; drop --cask)")
                } else {
                    no_such(api, &name, Some("cask"))
                });
            }
        }
        return failed(&no_such(api, &name, None));
    }
    for line in lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_home(api: &Api, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        println!("https://brew.sh");
        return ExitCode::SUCCESS;
    }
    let mut urls = Vec::new();
    for name in names {
        let url = api
            .formula(&name)
            .map(|f| f.homepage)
            .or_else(|_| api.cask(&name).map(|c| c.homepage));
        match url {
            Ok(u) if !u.is_empty() => urls.push(u),
            _ => return failed(&format!("abrew: no homepage for {name}")),
        }
    }
    for u in urls {
        println!("{u}");
    }
    ExitCode::SUCCESS
}

fn core_shard(short: &str) -> String {
    if short.starts_with("lib") {
        "lib".to_string()
    } else {
        short.chars().next().unwrap_or('x').to_string()
    }
}

fn cask_shard(token: &str) -> String {
    if let Some(rest) = token.strip_prefix("font-") {
        format!("font/font-{}", rest.chars().next().unwrap_or('x'))
    } else {
        token.chars().next().unwrap_or('x').to_string()
    }
}

pub(super) fn cmd_cat(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let Some(name) = operands(rest).into_iter().next() else {
        return usage("cat needs a formula name");
    };
    if has_flag(rest, "--cask", "") {
        let short = super::api::canonical_cask_token(Formula::short_name(&name));
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3
            && let (Some(u), Some(r)) = (parts.first(), parts.get(1))
            && !(*u == "homebrew" && *r == "cask")
        {
            let root = prefix.taps_dir().join(u).join(format!("homebrew-{r}"));
            for cand in [
                root.join("Casks").join(format!("{short}.rb")),
                root.join("Casks")
                    .join(cask_shard(short))
                    .join(format!("{short}.rb")),
            ] {
                if let Ok(text) = std::fs::read_to_string(&cand) {
                    print!("{text}");
                    return ExitCode::SUCCESS;
                }
            }
            return failed(&format!("abrew: no cask file for {name} in the local tap"));
        }
        let url = format!(
            "https://raw.githubusercontent.com/Homebrew/homebrew-cask/HEAD/Casks/{}/{short}.rb",
            cask_shard(short)
        );
        return match fetch::curl_get(&url, &[]) {
            Ok(bytes) => {
                print!("{}", String::from_utf8_lossy(&bytes));
                ExitCode::SUCCESS
            }
            Err(e) => failed(&e),
        };
    }
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() == 3
        && let (Some(u), Some(r), Some(n)) = (parts.first(), parts.get(1), parts.get(2))
    {
        let t = tap::Tap {
            user: (*u).to_string(),
            repo: (*r).to_string(),
            path: prefix.taps_dir().join(u).join(format!("homebrew-{r}")),
        };
        if let Some(file) = tap::formula_file(&t, n) {
            return match std::fs::read_to_string(&file) {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => failed(&format!("abrew: {e}")),
            };
        }
    }
    let short = Formula::short_name(&name);
    let url = format!(
        "https://raw.githubusercontent.com/Homebrew/homebrew-core/HEAD/Formula/{}/{short}.rb",
        core_shard(short)
    );
    match fetch::curl_get(&url, &[]) {
        Ok(bytes) => {
            print!("{}", String::from_utf8_lossy(&bytes));
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

/// `brew --prefix/--cellar/--caskroom <name>`: validate package existence and print its path.
pub(super) fn cmd_prefix_query(prefix: &Prefix, api: &Api, cmd: &str, name: &str) -> ExitCode {
    let short = Formula::short_name(name);
    if cmd == "--caskroom" {
        return match api.cask(short) {
            Ok(c) => {
                println!("{}", prefix.caskroom().join(&c.token).display());
                ExitCode::SUCCESS
            }
            Err(_) if api.formula(short).is_ok() => failed(&format!(
                "abrew: no such cask: {short} (it is a formula; try --cellar)"
            )),
            Err(_) => failed(&no_such(api, short, Some("cask"))),
        };
    }
    let resolved = api.formula(name).ok().or_else(|| tap::lookup(prefix, name));
    match resolved {
        Some(_) => {
            if cmd == "--prefix" {
                println!("{}", prefix.opt(short).display());
            } else {
                println!("{}", prefix.cellar().join(short).display());
            }
            ExitCode::SUCCESS
        }
        None if api.cask(short).is_ok() => failed(&format!(
            "abrew: no such formula: {short} (it is a cask; try --caskroom)"
        )),
        None => failed(&no_such(api, short, Some("formula"))),
    }
}

/// `brew --cache <name>…`: print the cached download path for each resolved package.
pub(super) fn cmd_cache_query(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let mut paths = Vec::new();
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !want_cask {
            let resolved = api
                .formula(&name)
                .ok()
                .or_else(|| tap::lookup(prefix, &name));
            match resolved {
                Some(f) => {
                    if f.bottle_for(&super::machine::Machine::this().bottle_tags())
                        .is_none()
                    {
                        return from_install(
                            prefix,
                            args,
                            Err(install::InstallError::Unsupported(format!(
                                "no bottle for {} on this platform",
                                f.name
                            ))),
                        );
                    }
                    paths.push(prefix.cache().join(format!(
                        "{}-{}.bottle.tar.gz",
                        f.name,
                        install::keg_version(&f)
                    )));
                    continue;
                }
                None if want_formula => {
                    return failed(&match api.cask(short) {
                        Ok(_) => format!(
                            "abrew: no such formula: {short} (it is a cask; drop --formula)"
                        ),
                        Err(_) => no_such(api, &name, Some("formula")),
                    });
                }
                None => {}
            }
        }
        match api.cask(short) {
            Ok(c) => {
                let c = match cask::resolve(&c, super::machine::Machine::this()) {
                    Ok(c) => c.into_owned(),
                    Err(e) => return from_install(prefix, args, Err(e)),
                };
                paths.push(prefix.cache().join(cask::download_name(&c)));
            }
            Err(e) => {
                return failed(&if want_cask {
                    lookup_error(api, &name, "cask", || api.formula(&name).is_ok(), e)
                } else if e.starts_with("abrew: no such ") {
                    no_such(api, &name, None)
                } else {
                    e
                });
            }
        }
    }
    for p in paths {
        println!("{}", p.display());
    }
    ExitCode::SUCCESS
}

pub(super) fn list_index(api: &Api, casks: bool) -> ExitCode {
    match api.index(casks) {
        Ok(rows) => {
            for (name, _) in rows {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

pub(super) fn cmd_config(prefix: &Prefix) -> ExitCode {
    println!("ABREW_VERSION: {} (aterm)", env!("CARGO_PKG_VERSION"));
    println!("ABREW_PREFIX: {}", prefix.root.display());
    println!("ABREW_CELLAR: {}", prefix.cellar().display());
    println!("ABREW_CASKROOM: {}", prefix.caskroom().display());
    println!("CPU: {}", std::env::consts::ARCH);
    println!("OS: {}", std::env::consts::OS);
    println!("IMPLEMENTATION: atpkg native (no Homebrew fallback)");
    ExitCode::SUCCESS
}

fn shellenv(prefix: &Prefix) -> String {
    let root = prefix.root.to_string_lossy();
    let quote = crate::platform::sh_quote_str;
    format!(
        "export ABREW_PREFIX={};\nexport PATH={}\"${{PATH+:$PATH}}\";\nexport MANPATH={}\"${{MANPATH+:$MANPATH}}:\";\nexport INFOPATH={}\"${{INFOPATH:-}}\";\n",
        quote(&root),
        quote(&format!("{root}/bin:{root}/sbin")),
        quote(&format!("{root}/share/man")),
        quote(&format!("{root}/share/info:"))
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellKind {
    Posix,
    Fish,
    Csh,
    Pwsh,
}

fn shell_kind(name: &str) -> ShellKind {
    let base = name.rsplit('/').next().unwrap_or(name);
    let base = base.strip_prefix('-').unwrap_or(base);
    match base {
        "fish" => ShellKind::Fish,
        "csh" | "tcsh" => ShellKind::Csh,
        "pwsh" | "pwsh-preview" | "powershell" => ShellKind::Pwsh,
        _ => ShellKind::Posix,
    }
}

pub(super) fn cmd_shellenv(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let shell = operands(rest)
        .into_iter()
        .next()
        .or_else(crate::caller_shell::invoking_shell)
        .unwrap_or_default();
    let root = prefix.root.to_string_lossy();
    let text = match shell_kind(&shell) {
        ShellKind::Posix => shellenv(prefix),
        ShellKind::Fish => {
            let q = |s: &str| {
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
            };
            format!(
                "set -gx ABREW_PREFIX \"{}\";\nset -gx PATH \"{}/bin\" \"{}/sbin\" $PATH;\nset -gx MANPATH \"{}/share/man\" $MANPATH;\nset -gx INFOPATH \"{}/share/info\" $INFOPATH;\n",
                q(&root),
                q(&root),
                q(&root),
                q(&root),
                q(&root)
            )
        }
        ShellKind::Csh => {
            let quote = crate::platform::sh_quote_str;
            let man = format!("{root}/share/man");
            let info = format!("{root}/share/info");
            format!(
                "setenv ABREW_PREFIX {};\nsetenv PATH {}:\"$PATH\";\nif ($?MANPATH) then\nsetenv MANPATH {}:\"$MANPATH\"\nelse\nsetenv MANPATH {}\nendif\nif ($?INFOPATH) then\nsetenv INFOPATH {}:\"$INFOPATH\"\nelse\nsetenv INFOPATH {}\nendif\n",
                quote(&root),
                quote(&format!("{root}/bin:{root}/sbin")),
                quote(&man),
                quote(&format!("{man}:")),
                quote(&info),
                quote(&format!("{info}:")),
            )
        }
        ShellKind::Pwsh => {
            let q = |s: &str| s.replace('\'', "''");
            let set = |name: &str, value: String| {
                format!(
                    "[System.Environment]::SetEnvironmentVariable('{name}',{value},[System.EnvironmentVariableTarget]::Process)\n"
                )
            };
            format!(
                "{}{}{}{}",
                set("ABREW_PREFIX", format!("'{}'", q(&root))),
                set(
                    "PATH",
                    format!("('{}/bin:{}/sbin:'+$ENV:PATH)", q(&root), q(&root))
                ),
                set(
                    "MANPATH",
                    format!("('{}/share/man:'+${{ENV:MANPATH}})", q(&root))
                ),
                set(
                    "INFOPATH",
                    format!("('{}/share/info:'+${{ENV:INFOPATH}})", q(&root))
                ),
            )
        }
    };
    print!("{text}");
    ExitCode::SUCCESS
}

fn unlinked_keg(prefix: &Prefix, i: &super::prefix::Installed) -> bool {
    keg_bin_linked(prefix, &i.name, &i.version) == Some(false)
}

fn keg_bin_linked(prefix: &Prefix, name: &str, version: &str) -> Option<bool> {
    let keg = prefix.keg(name, version);
    if super::prefix::receipt_word(&keg, "keg_only").as_deref() != Some("false") {
        return None;
    }
    let entries = std::fs::read_dir(keg.join("bin"))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if entries.is_empty() {
        return None;
    }
    let mut unknown = false;
    for entry in entries {
        let name = entry.file_name();
        match super::install::link_points_to(
            &prefix.bin().join(&name),
            &keg.join("bin").join(&name),
        ) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => unknown = true,
        }
    }
    if unknown { None } else { Some(false) }
}

fn linked_keg_version(prefix: &Prefix, i: &Installed) -> Option<String> {
    let mut linked: Option<String> = None;
    for version in &i.versions {
        if keg_bin_linked(prefix, &i.name, version) == Some(true) {
            if linked
                .as_deref()
                .is_some_and(|prior| prior != version.as_str())
            {
                return None;
            }
            linked = Some(version.clone());
        }
    }
    linked
}

pub(super) fn cmd_doctor(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let quiet = has_flag(rest, "--quiet", "-q");
    let mut problems = 0usize;
    let mut note = |m: String| {
        eprintln!("Warning: {m}");
        problems += 1;
    };
    if prefix.ensure().is_err() {
        note(format!(
            "prefix {} cannot be created/written",
            prefix.root.display()
        ));
    }
    for sub in ["bin", "sbin", "lib", "Applications", "opt"] {
        if let Ok(entries) = std::fs::read_dir(prefix.root.join(sub)) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_symlink()) && !e.path().exists() {
                    note(format!("broken symlink: {}", e.path().display()));
                }
            }
        }
    }
    for i in prefix.installed(Kind::Formula) {
        let receipt = prefix.keg(&i.name, &i.version).join("INSTALL_RECEIPT.json");
        if !receipt.is_file() {
            note(format!(
                "{} {} has no install receipt (poured by something else?)",
                i.name, i.version
            ));
        }
        if i.versions.len() > 1 {
            note(format!(
                "{} has {} versions; `abrew cleanup` frees the old ones",
                i.name,
                i.versions.len()
            ));
        }
        if unlinked_keg(prefix, &i) {
            note(format!(
                "{} {} is installed but not linked (run: abrew link {})",
                i.name, i.version, i.name
            ));
        }
    }
    for i in prefix.installed(Kind::Cask) {
        if i.versions.len() > 1 {
            note(format!(
                "{} has {} versions; `abrew cleanup` frees the old ones",
                i.name,
                i.versions.len()
            ));
        }
    }
    if let Ok(entries) = std::fs::read_dir(prefix.root.join("Pinned")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if prefix.pinned_version(&name).is_none() {
                if prefix.pin_link_dangling_ours(&name) {
                    note(format!(
                        "pin for {name} points at a keg that is gone (run: abrew unpin {name})"
                    ));
                } else {
                    note(format!("pin for {name} is not owned by abrew"));
                }
            }
        }
    }
    let path_has_bin = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d == prefix.bin()));
    if !prefix.installed(Kind::Formula).is_empty() && !path_has_bin {
        note(format!(
            "{} is not on PATH (eval \"$(abrew shellenv)\")",
            prefix.bin().display()
        ));
    }
    if let Some(_lock) = super::prefix::Lock::try_acquire(prefix) {
        prefix.restore_stale_asides();
        let _ = prefix.rebuild_state();
    }
    if problems == 0 {
        if !quiet {
            println!("Your abrew is ready to brew.");
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn install_opts(rest: &[String]) -> InstallOpts {
    InstallOpts {
        ignore_dependencies: has_flag(rest, "--ignore-dependencies", ""),
        dry_run: has_flag(rest, "--dry-run", "-n"),
        force: has_flag(rest, "--force", "-f"),
        quiet: has_flag(rest, "--quiet", "-q"),
        upgrade: false,
        reinstall: false,
        as_dependency: false,
    }
}

pub(super) fn cmd_install(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("install needs at least one name");
    }
    let opts = install_opts(rest);
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    for name in &names {
        if want_cask {
            if let Err(e) = api.cask(name) {
                if e.contains("no such") {
                    if api.formula(name).is_ok() {
                        return failed(&format!(
                            "abrew: {name} is a formula, not a cask (drop --cask)"
                        ));
                    }
                    return failed(&no_such(api, name, Some("cask")));
                }
                return failed(&e);
            }
        } else if want_formula {
            if api.formula(name).is_err() && tap::lookup(prefix, name).is_none() {
                if api.cask(name).is_ok() {
                    return failed(&format!(
                        "abrew: {name} is a cask, not a formula (drop --formula)"
                    ));
                }
                return failed(&no_such(api, name, Some("formula")));
            }
        } else if api.formula(name).is_err()
            && tap::lookup(prefix, name).is_none()
            && let Err(error) = api.cask(name)
        {
            if error.contains("no such") {
                return failed(&no_such(api, name, None));
            }
            return failed(&error);
        }
    }
    let mut landed = false;
    let mut failures = 0;
    for name in &names {
        let result = if !want_cask && !want_formula {
            if api.formula(name).is_ok() {
                install::install(api, prefix, name, &opts).map(|_| ())
            } else {
                match api.cask(name) {
                    Ok(c) => cask::install_with_deps(
                        api,
                        prefix,
                        &c,
                        opts.dry_run,
                        opts.force,
                        false,
                        false,
                    )
                    .map(|_| ()),
                    Err(error) if error.contains("no such") => {
                        return failed(&no_such(api, name, None));
                    }
                    Err(error) => return failed(&error),
                }
            }
        } else if want_cask {
            match api.cask(name) {
                Ok(c) => {
                    cask::install_with_deps(api, prefix, &c, opts.dry_run, opts.force, false, false)
                        .map(|_| ())
                }
                Err(e) if e.contains("no such") => {
                    if api.formula(name).is_ok() {
                        return failed(&format!(
                            "abrew: {name} is a formula, not a cask (drop --cask)"
                        ));
                    }
                    return failed(&no_such(api, name, Some("cask")));
                }
                Err(e) => return failed(&e),
            }
        } else {
            match install::install(api, prefix, name, &opts).map(|_| ()) {
                Err(install::InstallError::Failed(e)) if e.contains("no such") => {
                    if api.cask(name).is_ok() {
                        return failed(&format!(
                            "abrew: {name} is a cask, not a formula (drop --formula)"
                        ));
                    }
                    return failed(&no_such(api, name, Some("formula")));
                }
                other => other,
            }
        };
        match result {
            Ok(()) => landed = true,
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
            Err(error @ install::InstallError::Unsupported(_)) => {
                if names.len() > 1 && landed {
                    let install::InstallError::Unsupported(reason) = &error else {
                        unreachable!()
                    };
                    eprintln!("abrew: {reason}; skipped");
                    failures += 1;
                } else {
                    return from_install(prefix, args, Err(error));
                }
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Brew's `Utils::Text.to_sentence`: "a", "a and b", "a, b, and c".
pub(super) fn to_sentence(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (last, head) = items.split_last().expect("non-empty");
            format!("{}, and {last}", head.join(", "))
        }
    }
}

pub(super) fn cmd_uninstall(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("uninstall needs at least one name");
    }
    let ignore = has_flag(rest, "--ignore-dependencies", "");
    let dry = has_flag(rest, "--dry-run", "-n");
    let force = has_flag(rest, "--force", "-f");
    if !ignore && !dry && !has_flag(rest, "--cask", "") {
        let removing: std::collections::BTreeSet<String> = names
            .iter()
            .map(|n| Formula::short_name(n).to_string())
            .collect();
        let mut reqs: Vec<String> = Vec::new();
        let mut deps: Vec<String> = Vec::new();
        for name in &names {
            let short = Formula::short_name(name);
            let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
                continue;
            };
            if let Ok(on) = install::dependents(api, prefix, short) {
                let on: Vec<String> = on.into_iter().filter(|d| !removing.contains(d)).collect();
                if !on.is_empty() {
                    reqs.push(prefix.keg(short, &inst.version).display().to_string());
                    for d in on {
                        if !deps.contains(&d) {
                            deps.push(d);
                        }
                    }
                }
            }
        }
        if !reqs.is_empty() {
            eprintln!(
                "Error: Refusing to uninstall {}\nbecause {} {} required by {}, which {} currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies {}",
                to_sentence(&reqs),
                if reqs.len() == 1 { "it" } else { "they" },
                if reqs.len() == 1 { "is" } else { "are" },
                to_sentence(&deps),
                if deps.len() == 1 { "is" } else { "are" },
                names.join(" ")
            );
            return ExitCode::FAILURE;
        }
    }
    let mut failures = 0;
    let mut completed = false;
    for name in &names {
        let short = Formula::short_name(name);
        let cask_short = super::api::canonical_cask_token(short);
        if !has_flag(rest, "--cask", "")
            && !has_flag(rest, "--formula", "")
            && prefix.installed_one(Kind::Formula, short).is_some()
            && prefix.installed_one(Kind::Cask, cask_short).is_some()
        {
            eprintln!(
                "abrew: {short} is installed as both a formula and a cask; say --formula or --cask"
            );
            failures += 1;
            continue;
        }
        let r = if has_flag(rest, "--cask", "")
            || (!has_flag(rest, "--formula", "")
                && prefix.installed_one(Kind::Formula, short).is_none()
                && prefix.installed_one(Kind::Cask, cask_short).is_some())
        {
            if force
                && has_flag(rest, "--cask", "")
                && prefix.installed_one(Kind::Cask, cask_short).is_none()
            {
                Ok(())
            } else {
                cask::uninstall(prefix, cask_short, dry)
            }
        } else {
            let pinned_refusal = !force && prefix.pinned_version(short).is_some();
            if !dry
                && !pinned_refusal
                && let Some(inst) = prefix.installed_one(Kind::Formula, short)
            {
                if force {
                    println!(
                        "Uninstalling {short}... ({})",
                        keg_abv(&prefix.cellar().join(short))
                    );
                } else {
                    let targets = install::uninstall_versions(prefix, &inst, false);
                    let keg = prefix.keg(short, &targets[0]);
                    println!("Uninstalling {}... ({})", keg.display(), keg_abv(&keg));
                }
            }
            install::uninstall(api, prefix, name, ignore, force, dry).inspect(|_| {
                if !dry
                    && !force
                    && let Some(left) = prefix.installed_one(Kind::Formula, short)
                    && !left.versions.is_empty()
                {
                    println!(
                        "{short} {} {} still installed.",
                        to_sentence(&left.versions),
                        if left.versions.len() == 1 {
                            "is"
                        } else {
                            "are"
                        }
                    );
                    println!("To remove all versions, run:");
                    println!("  brew uninstall --force {short}");
                }
            })
        };
        match r {
            Ok(()) => completed = true,
            Err(e @ install::InstallError::Unsupported(_)) => {
                return from_install(
                    prefix,
                    args,
                    Err(super::after_native_progress(e, completed)),
                );
            }
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_reinstall(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("reinstall needs at least one name");
    }
    let opts = InstallOpts {
        force: true,
        reinstall: true,
        ..install_opts(rest)
    };
    let want_cask = has_flag(rest, "--cask", "");
    let want_formula = has_flag(rest, "--formula", "");
    let mut completed = false;
    let mut failures = 0;
    for name in &names {
        let short = Formula::short_name(name);
        let cask_short = super::api::canonical_cask_token(short);
        let as_cask = if want_cask {
            true
        } else if want_formula || prefix.installed_one(Kind::Formula, short).is_some() {
            false
        } else if prefix.installed_one(Kind::Cask, cask_short).is_some() {
            true
        } else {
            api.formula(name).is_err() && api.cask(cask_short).is_ok()
        };
        let r: Result<(), install::InstallError> = if as_cask {
            match api.cask(cask_short) {
                Ok(c) => cask::install_with_deps(api, prefix, &c, opts.dry_run, true, false, true)
                    .map(|_| ()),
                Err(error) => Err(error.into()),
            }
        } else {
            install::install(api, prefix, short, &opts).map(|_| ())
        };
        completed |= super::native_attempt_may_have_progress(&r);
        match r {
            Ok(()) => {
                let target = if as_cask { cask_short } else { short };
                if let Err(error) = auto_cleanup_old_versions(prefix, api, opts.dry_run, &[target])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(e @ install::InstallError::Unsupported(_)) => {
                return from_install(
                    prefix,
                    args,
                    Err(super::after_native_progress(e, completed)),
                );
            }
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn outdated_rows(
    prefix: &Prefix,
    api: &Api,
    casks: bool,
    greedy: bool,
) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    if casks {
        for i in prefix.installed(Kind::Cask) {
            let c = api.cask(&i.name)?;
            if c.auto_updates && !greedy {
                continue;
            }
            if version_cmp(&i.version, &c.version) == Ordering::Less {
                out.push((i.name, i.version, c.version));
            }
        }
    } else {
        for i in prefix.installed(Kind::Formula) {
            let f = api.formula(&i.name)?;
            if version_cmp(&i.version, &install::keg_version(&f)) == Ordering::Less {
                out.push((i.name, i.version, install::keg_version(&f)));
            }
        }
    }
    Ok(out)
}

fn outdated_json_row(prefix: &Prefix, kind: Kind, name: &str, latest: &str) -> aterm_json::Value {
    let installed = prefix.installed_one(kind, name);
    let versions: Vec<aterm_json::Value> = installed
        .as_ref()
        .map(|i| {
            i.versions
                .iter()
                .cloned()
                .map(aterm_json::Value::String)
                .collect()
        })
        .unwrap_or_default();
    let pinned = kind == Kind::Formula && prefix.is_pinned(name);
    let mut map = aterm_json::Map::new();
    map.insert(
        "name".to_string(),
        aterm_json::Value::String(name.to_string()),
    );
    map.insert(
        "installed_versions".to_string(),
        aterm_json::Value::Array(versions),
    );
    map.insert(
        "current_version".to_string(),
        aterm_json::Value::String(latest.to_string()),
    );
    map.insert("pinned".to_string(), aterm_json::Value::Bool(pinned));
    map.insert(
        "pinned_version".to_string(),
        if pinned {
            prefix
                .pinned_version(name)
                .map(aterm_json::Value::String)
                .unwrap_or(aterm_json::Value::Null)
        } else {
            aterm_json::Value::Null
        },
    );
    aterm_json::Value::Object(map)
}

const fn outdated_failed(any: bool, named: bool) -> bool {
    any && named
}

pub(super) fn cmd_outdated(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let scope: Vec<String> = operands(rest)
        .iter()
        .map(|n| Formula::short_name(n).to_string())
        .collect();
    for name in &scope {
        let unknown = package_definitively_unknown(
            prefix,
            api,
            name,
            !has_flag(rest, "--cask", ""),
            !has_flag(rest, "--formula", ""),
        );
        if unknown {
            return failed(&format!("abrew: no formula or cask named \"{name}\""));
        }
    }
    if has_flag(rest, "--json=v1", "") {
        return failed("abrew: outdated --json=v1 is not supported; use --json=v2");
    }
    let json = has_flag(rest, "--json", "") || has_flag(rest, "--json=v2", "");
    let mut collected: [Vec<(String, String, String)>; 2] = [Vec::new(), Vec::new()];
    for (slot, casks) in [(0usize, false), (1usize, true)] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        match outdated_rows(prefix, api, casks, has_flag(rest, "--greedy", "")) {
            Ok(mut rows) => {
                if !scope.is_empty() {
                    rows.retain(|(name, _, _)| {
                        scope.iter().any(|s| {
                            s == name || (casks && super::api::canonical_cask_token(s) == name)
                        })
                    });
                }
                collected[slot] = rows;
            }
            Err(error) => return failed(&error),
        }
    }
    let any = collected.iter().any(|r| !r.is_empty());
    if json {
        let rows_json = |rows: &[(String, String, String)], kind: Kind| {
            rows.iter()
                .map(|(name, _, latest)| outdated_json_row(prefix, kind, name, latest))
                .collect::<Vec<_>>()
        };
        let mut map = aterm_json::Map::new();
        map.insert(
            "formulae".to_string(),
            aterm_json::Value::Array(rows_json(&collected[0], Kind::Formula)),
        );
        map.insert(
            "casks".to_string(),
            aterm_json::Value::Array(rows_json(&collected[1], Kind::Cask)),
        );
        let value = aterm_json::Value::Object(map);
        match aterm_json::to_string(&value) {
            Ok(text) => println!("{text}"),
            Err(e) => return failed(&format!("abrew: {e}")),
        }
    } else {
        for (rows, casks) in collected.iter().zip([false, true]) {
            for (name, have, latest) in rows {
                let pin = if !casks && prefix.is_pinned(name) {
                    " [pinned]"
                } else {
                    ""
                };
                if has_flag(rest, "--verbose", "-v") || !has_flag(rest, "--quiet", "-q") {
                    println!("{name} ({have}) < {latest}{pin}");
                } else {
                    println!("{name}");
                }
            }
        }
    }
    if outdated_failed(any, !scope.is_empty()) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_upgrade(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let mut opts = install_opts(rest);
    opts.upgrade = true;
    if has_flag(rest, "--cask", "") {
        return cmd_upgrade_casks(prefix, api, args, rest, &opts, operands(rest), false);
    }
    let names = operands(rest);
    if !names.is_empty() && !has_flag(rest, "--formula", "") {
        let (cask_names, formula_names): (Vec<String>, Vec<String>) =
            names.into_iter().partition(|name| {
                let short = Formula::short_name(name);
                let cask_short = super::api::canonical_cask_token(short);
                prefix.installed_one(Kind::Formula, short).is_none()
                    && prefix.installed_one(Kind::Cask, cask_short).is_some()
            });
        if !formula_names.is_empty() {
            let code = upgrade_formula_names(prefix, api, args, &formula_names, &opts);
            if code != ExitCode::SUCCESS {
                return code;
            }
        }
        if !cask_names.is_empty() {
            return cmd_upgrade_casks(
                prefix,
                api,
                args,
                rest,
                &opts,
                cask_names,
                !formula_names.is_empty(),
            );
        }
        return ExitCode::SUCCESS;
    }
    let names = operands(rest);
    let Some(_name) = names.first() else {
        let rows = match outdated_rows(prefix, api, false, true) {
            Ok(rows) => rows,
            Err(error) => return failed(&error),
        };
        if rows.is_empty() {
            println!("abrew: everything is up to date");
            return ExitCode::SUCCESS;
        }
        let mut failures = 0;
        for (name, have, latest) in rows {
            if prefix.is_pinned(&name) {
                eprintln!("==> {name} ({have}) is pinned; skipping");
                continue;
            }
            println!("==> Upgrading {name} {have} -> {latest}");
            match install::install(api, prefix, &name, &opts) {
                Ok(now) if now != have => {
                    if !opts.dry_run
                        && let Err(error) =
                            auto_cleanup_old_versions(prefix, api, false, &[name.as_str()])
                    {
                        eprintln!("abrew: {error}");
                        failures += 1;
                    }
                }
                Ok(_) => {}
                Err(install::InstallError::Unsupported(reason)) => {
                    eprintln!("abrew: {name}: {reason}; skipped");
                    failures += 1;
                }
                Err(install::InstallError::Failed(error)) => {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
        }
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    };
    upgrade_formula_names(prefix, api, args, &names, &opts)
}

fn upgrade_formula_names(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    names: &[String],
    opts: &install::InstallOpts,
) -> ExitCode {
    let mut failures = 0;
    let mut landed = false;
    for name in names {
        let short = Formula::short_name(name);
        let Some(before) = prefix.installed_one(Kind::Formula, short) else {
            eprintln!(
                "abrew: {} is not installed (upgrade does not install)",
                Formula::short_name(name)
            );
            failures += 1;
            continue;
        };
        if prefix.is_pinned(short) {
            let latest = api
                .formula(name)
                .ok()
                .or_else(|| tap::lookup(prefix, name))
                .map(|f| install::keg_version(&f));
            match latest {
                Some(latest) if version_cmp(&before.version, &latest) != Ordering::Less => {
                    eprintln!("Warning: {short} {} already installed", before.version);
                }
                _ => {
                    eprintln!("abrew: {short} is pinned; skipping");
                    failures += 1;
                }
            }
            continue;
        }
        let result = install::install(api, prefix, name, opts);
        landed |= super::native_attempt_may_have_progress(&result);
        match result {
            Ok(installed_now) => {
                if !opts.dry_run
                    && installed_now != before.version
                    && let Err(error) = auto_cleanup_old_versions(prefix, api, false, &[name])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(install::InstallError::Failed(msg)) => {
                eprintln!("{msg}");
                failures += 1;
            }
            Err(error @ install::InstallError::Unsupported(_)) => {
                if names.len() > 1 && landed {
                    let install::InstallError::Unsupported(reason) = &error else {
                        unreachable!()
                    };
                    eprintln!("abrew: {reason}; skipped");
                    failures += 1;
                } else {
                    return from_install(prefix, args, Err(error));
                }
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_upgrade_casks(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    opts: &install::InstallOpts,
    names: Vec<String>,
    prior_native_progress: bool,
) -> ExitCode {
    let mut failures = 0;
    let rows: Vec<(String, String, String)> = if !names.is_empty() {
        let mut rows = Vec::new();
        for name in &names {
            let short = Formula::short_name(name);
            let cask_short = super::api::canonical_cask_token(short);
            let Some(inst) = prefix.installed_one(Kind::Cask, cask_short) else {
                eprintln!("abrew: cask {short} is not installed (upgrade does not install)");
                failures += 1;
                continue;
            };
            rows.push((cask_short.to_string(), inst.version, String::new()));
        }
        rows
    } else {
        match outdated_rows(prefix, api, true, has_flag(rest, "--greedy", "")) {
            Ok(rows) => rows,
            Err(error) => return failed(&error),
        }
    };
    if rows.is_empty() {
        if failures == 0 {
            println!("abrew: everything is up to date");
            return ExitCode::SUCCESS;
        }
        return ExitCode::FAILURE;
    }
    let mut completed = prior_native_progress;
    for (token, have, _latest) in rows {
        let c = match api.cask(&token) {
            Ok(c) => c,
            Err(error) => return from_install(prefix, args, Err(error.into())),
        };
        if names.is_empty() && c.auto_updates && !has_flag(rest, "--greedy", "") {
            eprintln!("==> {token} ({have}) auto-updates; skipping");
            continue;
        }
        if super::formula::version_cmp(&have, &c.version) != std::cmp::Ordering::Less {
            if !names.is_empty() {
                eprintln!(
                    "Warning: Not upgrading {token}, the latest version is already installed"
                );
            }
            continue;
        }
        println!("==> Upgrading cask {token} {have} -> {}", c.version);
        let result =
            cask::install_with_deps(api, prefix, &c, opts.dry_run, opts.force, true, false);
        completed |= super::native_attempt_may_have_progress(&result);
        match result {
            Ok(now) => {
                if !opts.dry_run
                    && now != have
                    && let Err(error) =
                        auto_cleanup_old_versions(prefix, api, false, &[token.as_str()])
                {
                    eprintln!("abrew: {error}");
                    failures += 1;
                }
            }
            Err(install::InstallError::Unsupported(reason)) => {
                if !names.is_empty() {
                    return from_install(
                        prefix,
                        args,
                        Err(super::after_native_progress(
                            install::InstallError::Unsupported(reason),
                            completed,
                        )),
                    );
                }
                eprintln!("abrew: {token}: {reason}; skipped");
                failures += 1;
            }
            Err(install::InstallError::Failed(error)) => {
                eprintln!("abrew: {error}");
                failures += 1;
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct UpdateReport {
    new_formulae: Vec<(String, String)>,
    new_casks: Vec<(String, String)>,
    deleted_formulae: Vec<String>,
    deleted_casks: Vec<String>,
    outdated_formulae: Vec<String>,
    outdated_casks: Vec<String>,
}

fn update_report(
    old: Option<&[super::api::IndexRow]>,
    new: &[super::api::IndexRow],
    installed: &[Installed],
    new_report: bool,
) -> (Vec<(String, String)>, Vec<String>, Vec<String>) {
    let installed_names: std::collections::BTreeSet<&str> =
        installed.iter().map(|i| i.name.as_str()).collect();
    let (mut added, mut deleted) = (Vec::new(), Vec::new());
    if let Some(old) = old {
        let old_names: std::collections::BTreeSet<&str> =
            old.iter().map(|r| r.name.as_str()).collect();
        let new_names: std::collections::BTreeSet<&str> =
            new.iter().map(|r| r.name.as_str()).collect();
        for row in new {
            if !old_names.contains(row.name.as_str())
                && !installed_names.contains(row.name.as_str())
            {
                added.push((row.name.clone(), row.desc.clone()));
            }
        }
        for row in old {
            if !new_names.contains(row.name.as_str()) && installed_names.contains(row.name.as_str())
            {
                deleted.push(row.name.clone());
            }
        }
        added.sort();
        deleted.sort();
        if !new_report {
            added.clear();
        }
    }
    let latest: std::collections::BTreeMap<&str, &super::api::IndexRow> =
        new.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut outdated = Vec::new();
    for inst in installed {
        if let Some(row) = latest.get(inst.name.as_str())
            && !row.auto_updates
            && !row.version.is_empty()
            && version_cmp(&inst.version, &row.version) == Ordering::Less
        {
            outdated.push(inst.name.clone());
        }
    }
    outdated.sort();
    (added, deleted, outdated)
}

fn print_update_report(r: &UpdateReport) -> bool {
    let mut printed = false;
    let new_section = |title: &str, rows: &[(String, String)], printed: &mut bool| {
        if rows.is_empty() {
            return;
        }
        *printed = true;
        println!("==> {title}");
        for (name, desc) in rows {
            if desc.is_empty() {
                println!("{name}");
            } else {
                println!("{name}: {desc}");
            }
        }
    };
    let name_section = |title: &str, rows: &[String], printed: &mut bool| {
        if rows.is_empty() {
            return;
        }
        *printed = true;
        println!("==> {title}");
        for name in rows {
            println!("{name}");
        }
    };
    new_section("New Formulae", &r.new_formulae, &mut printed);
    new_section("New Casks", &r.new_casks, &mut printed);
    name_section(
        "Deleted Installed Formulae",
        &r.deleted_formulae,
        &mut printed,
    );
    name_section("Deleted Installed Casks", &r.deleted_casks, &mut printed);
    name_section("Outdated Formulae", &r.outdated_formulae, &mut printed);
    name_section("Outdated Casks", &r.outdated_casks, &mut printed);
    let (of, oc) = (r.outdated_formulae.len(), r.outdated_casks.len());
    if of + oc > 0 {
        let mut msg = String::new();
        if of > 0 {
            msg += &format!("{of} outdated formula{}", if of == 1 { "" } else { "e" });
        }
        if oc > 0 {
            if !msg.is_empty() {
                msg += " and ";
            }
            msg += &format!("{oc} outdated cask{}", if oc == 1 { "" } else { "s" });
        }
        let pronoun = if of + oc == 1 { "it" } else { "them" };
        let upgrade = match (of, oc) {
            (_, 0) => "brew upgrade",
            (0, _) => "brew upgrade --cask",
            _ => "brew upgrade and brew upgrade --cask",
        };
        println!();
        println!("You have {msg} installed.");
        println!("You can upgrade {pronoun} with {upgrade}");
        println!("or list {pronoun} with brew outdated.");
    }
    printed
}

pub(super) fn cmd_update(prefix: &Prefix, api: &Api) -> ExitCode {
    let old_f = api.cached_index_rows(false);
    let old_c = api.cached_index_rows(true);
    let mut tap_failures = 0usize;
    for t in tap::list(prefix) {
        if let Err(error) = tap::update(prefix, &t) {
            eprintln!(
                "abrew: warning: {}",
                error.strip_prefix("abrew: ").unwrap_or(&error)
            );
            tap_failures += 1;
        }
    }
    let staged = (api.index_rows_fresh(false), api.index_rows_fresh(true));
    if let (Ok(_), Ok(_)) = &staged {
        api.refresh_except(&["formula-index.json", "cask-index.json"]);
    }
    match staged {
        (Ok(f), Ok(c)) => {
            let report_new = std::env::var_os("HOMEBREW_NO_UPDATE_REPORT_NEW").is_none();
            let inst_f = prefix.installed(Kind::Formula);
            let inst_c = prefix.installed(Kind::Cask);
            let (new_f, del_f, out_f) = update_report(old_f.as_deref(), &f, &inst_f, report_new);
            let (new_c, del_c, out_c) = update_report(
                old_c.as_deref(),
                &c,
                &inst_c,
                report_new && !inst_c.is_empty(),
            );
            let report = UpdateReport {
                new_formulae: new_f,
                new_casks: new_c,
                deleted_formulae: del_f,
                deleted_casks: del_c,
                outdated_formulae: out_f,
                outdated_casks: out_c,
            };
            if !print_update_report(&report) {
                println!("Already up-to-date.");
            }
            if tap_failures > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        (Err(e), _) | (_, Err(e)) => failed(&e),
    }
}

fn remove_links_into(prefix: &Prefix, dir: &std::path::Path) {
    fn walk(root: &std::path::Path, dir: &std::path::Path, depth: u8) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(&path)
                    && install::link_target_points_into(&path, &target, dir)
                {
                    let _ = std::fs::remove_file(&path);
                }
            } else if meta.is_dir() {
                walk(&path, dir, depth - 1);
            }
        }
    }
    for name in [
        "bin",
        "sbin",
        "lib",
        "include",
        "share",
        "etc",
        "Applications",
        "Library",
    ] {
        walk(&prefix.root.join(name), dir, 8);
    }
}

fn auto_cleanup_old_versions(
    prefix: &Prefix,
    api: &Api,
    dry: bool,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    if super::no_install_cleanup() {
        return Ok((0, 0));
    }
    cleanup_old_versions(prefix, api, dry, scope)
}

fn disk_usage_readable(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB"] {
        if (size * 10.0).round() / 10.0 < 1000.0 {
            break;
        }
        size /= 1000.0;
        unit = next;
    }
    let rounded = (size * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{}{unit}", rounded as u64)
    } else {
        format!("{rounded:.1}{unit}")
    }
}

fn dir_disk_usage(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn cleanup_old_versions(
    prefix: &Prefix,
    api: &Api,
    dry: bool,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    let mut removed = 0;
    let mut freed = 0u64;
    for kind in [Kind::Formula, Kind::Cask] {
        for i in prefix.installed(kind) {
            let _ = api;
            let active = prefix
                .opt(&i.name)
                .read_link()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            if !scope.is_empty() && !scope.iter().any(|name| Formula::short_name(name) == i.name) {
                continue;
            }
            let pinned = (kind == Kind::Formula)
                .then(|| prefix.pinned_version(&i.name))
                .flatten();
            for v in &i.versions {
                if *v != i.version
                    && pinned.as_deref() != Some(v.as_str())
                    && active.as_deref() != Some(v.as_str())
                {
                    let dir = match kind {
                        Kind::Formula => prefix.keg(&i.name, v),
                        Kind::Cask => prefix.cask_dir(&i.name, v),
                    };
                    if !super::prefix::owned_receipt(&dir, kind, &i.name, v) {
                        continue;
                    }
                    let usage = dir_disk_usage(&dir);
                    if dry {
                        println!(
                            "Would remove: {} ({})",
                            dir.display(),
                            disk_usage_readable(usage)
                        );
                    } else {
                        println!(
                            "Removing: {}... ({})",
                            dir.display(),
                            disk_usage_readable(usage)
                        );
                        remove_links_into(prefix, &dir);
                        std::fs::remove_dir_all(&dir)
                            .map_err(|e| format!("abrew: cannot remove {}: {e}", dir.display()))?;
                    }
                    freed = freed.saturating_add(usage);
                    removed += 1;
                }
            }
        }
    }
    Ok((removed, freed))
}

fn prune_downloads(
    prefix: &Prefix,
    dry: bool,
    max_age: Option<std::time::Duration>,
    scope: &[&str],
) -> Result<(usize, u64), String> {
    if !scope.is_empty() {
        return Ok((0, 0));
    }
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    let mut freed = 0u64;
    let entries = match std::fs::read_dir(prefix.cache()) {
        Ok(entries) => entries,
        Err(_) => return Ok((0, 0)),
    };
    for entry in entries.flatten() {
        if !entry
            .file_type()
            .is_ok_and(|t| t.is_file() && !t.is_symlink())
        {
            continue;
        }
        if let Some(max) = max_age {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= max);
            if !old {
                continue;
            }
        }
        let path = entry.path();
        let usage = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if dry {
            println!(
                "Would remove: {} ({})",
                path.display(),
                disk_usage_readable(usage)
            );
        } else {
            println!(
                "Removing: {}... ({})",
                path.display(),
                disk_usage_readable(usage)
            );
            std::fs::remove_file(&path)
                .map_err(|e| format!("abrew: cannot remove {}: {e}", path.display()))?;
        }
        freed = freed.saturating_add(usage);
        removed += 1;
    }
    Ok((removed, freed))
}

/// Parse and validate `--prune` cache age options before running cleanup.
pub(super) fn cleanup_max_age(
    rest: &[String],
) -> Result<Option<Option<std::time::Duration>>, &'static str> {
    const INVALID: &str = "cleanup --prune takes all or a whole number of days";
    let mut max_age = None;
    for value in rest.iter().filter_map(|a| a.strip_prefix("--prune=")) {
        let age = if value == "all" {
            None
        } else {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(INVALID);
            }
            let seconds = value
                .parse::<u64>()
                .ok()
                .and_then(|days| days.checked_mul(86_400))
                .ok_or(INVALID)?;
            Some(std::time::Duration::from_secs(seconds))
        };
        if max_age.is_none() {
            max_age = Some(age);
        }
    }
    Ok(if has_flag(rest, "--scrub", "-s") {
        Some(None)
    } else {
        max_age
    })
}

#[derive(Debug, PartialEq, Eq)]
enum UnpinOutcome {
    Remove,
    WarnNotPinned,
    ErrorNotInstalled,
    FailUnknown,
}

const fn unpin_outcome(pinned: bool, installed: bool, definitively_unknown: bool) -> UnpinOutcome {
    if pinned {
        UnpinOutcome::Remove
    } else if installed {
        UnpinOutcome::WarnNotPinned
    } else if definitively_unknown {
        UnpinOutcome::FailUnknown
    } else {
        UnpinOutcome::ErrorNotInstalled
    }
}

pub(super) fn cmd_unpin(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let mut failures = 0;
    for name in operands(rest) {
        let short = Formula::short_name(&name);
        if !super::valid_component(short) {
            eprintln!("abrew: invalid formula name {short}");
            failures += 1;
            continue;
        }
        let pinned = prefix.pinned_version(short).is_some() || prefix.pin_link_dangling_ours(short);
        let installed = prefix.installed_one(Kind::Formula, short).is_some()
            || prefix
                .installed_one(Kind::Cask, super::api::canonical_cask_token(short))
                .is_some();
        let unknown =
            !pinned && !installed && package_definitively_unknown(prefix, api, short, true, true);
        match unpin_outcome(pinned, installed, unknown) {
            UnpinOutcome::Remove => {
                if let Err(error) = prefix.unpin(short) {
                    eprintln!("{error}");
                    failures += 1;
                }
            }
            UnpinOutcome::WarnNotPinned => eprintln!("Warning: {short} not pinned"),
            UnpinOutcome::ErrorNotInstalled => eprintln!("Error: {short} not installed"),
            UnpinOutcome::FailUnknown => {
                eprintln!("abrew: no formula or cask named \"{short}\"");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_cleanup(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let max_age = match cleanup_max_age(rest) {
        Ok(age) => age,
        Err(error) => return usage(error),
    };
    let dry = has_flag(rest, "--dry-run", "-n");
    let named = operands(rest);
    for name in &named {
        let short = Formula::short_name(name);
        if package_definitively_unknown(prefix, api, short, true, true) {
            return failed(&format!("abrew: no formula or cask named \"{short}\""));
        }
    }
    let scope: Vec<&str> = named
        .iter()
        .map(|s| super::api::canonical_cask_token(Formula::short_name(s)))
        .collect();
    let mut freed = match cleanup_old_versions(prefix, api, dry, &scope) {
        Ok((_, bytes)) => bytes,
        Err(error) => return failed(&error),
    };
    if let Some(max_age) = max_age {
        match prune_downloads(prefix, dry, max_age, &scope) {
            Ok((_, bytes)) => freed = freed.saturating_add(bytes),
            Err(error) => return failed(&error),
        }
    }
    if freed > 0 {
        if dry {
            println!(
                "==> This operation would free approximately {} of disk space.",
                disk_usage_readable(freed)
            );
        } else {
            println!(
                "==> This operation has freed approximately {} of disk space.",
                disk_usage_readable(freed)
            );
        }
    }
    ExitCode::SUCCESS
}

const MAX_REQUEST_RECEIPT_BYTES: usize = 64 * 1024;

/// Return whether any installed version of `i` was explicitly requested.
pub(super) fn requested(
    prefix: &Prefix,
    installed: &super::prefix::Installed,
) -> Result<bool, String> {
    let mut any = false;
    for version in &installed.versions {
        let file = prefix
            .keg(&installed.name, version)
            .join("INSTALL_RECEIPT.json");
        let bytes = crate::metadata_io::read_bounded_regular(&file, MAX_REQUEST_RECEIPT_BYTES)
            .map_err(|e| format!("abrew: cannot read request receipt {}: {e}", file.display()))?;
        let value: aterm_json::Value = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("abrew: bad receipt {}: {e}", file.display()))?;
        match value
            .get("installed_on_request")
            .and_then(aterm_json::Value::as_str)
        {
            Some("true") => any = true,
            Some("false") => {}
            _ => {
                return Err(format!(
                    "abrew: unknown request status in {}",
                    file.display()
                ));
            }
        }
    }
    Ok(any)
}

fn autoremove_targets(
    prefix: &Prefix,
    read: impl FnMut(&str) -> Result<Formula, String>,
) -> Result<Vec<String>, String> {
    let installed = prefix.installed_checked(Kind::Formula)?;
    let mut roots = Vec::new();
    let mut pinned = std::collections::BTreeSet::new();
    for i in &installed {
        let pin = prefix.is_pinned_checked(&i.name)?;
        if pin {
            pinned.insert(i.name.clone());
        }
        if requested(prefix, i)? || pin {
            roots.push(i.name.clone());
        }
    }
    let books = install::dependency_books(&installed, read)?;
    let mut needed = std::collections::BTreeSet::new();
    while let Some(name) = roots.pop() {
        if !needed.insert(name.clone()) {
            continue;
        }
        let (_, formula) = books
            .iter()
            .find(|(i, _)| i.name == name)
            .ok_or_else(|| format!("abrew: cannot complete installed dependency {name}"))?;
        roots.extend(
            formula
                .dependencies
                .iter()
                .map(|d| Formula::short_name(d).to_string()),
        );
    }
    let mut remaining: std::collections::BTreeSet<_> = installed
        .iter()
        .filter(|i| !needed.contains(&i.name) && !pinned.contains(&i.name))
        .map(|i| i.name.clone())
        .collect();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .find(|candidate| {
                !books.iter().any(|(i, f)| {
                    remaining.contains(&i.name)
                        && i.name != **candidate
                        && f.dependencies
                            .iter()
                            .any(|d| Formula::short_name(d) == candidate.as_str())
                })
            })
            .cloned()
            .ok_or_else(|| {
                "abrew: cyclic orphan dependencies; automatic deletion refused".to_string()
            })?;
        remaining.remove(&next);
        ordered.push(next);
    }
    Ok(ordered)
}

pub(super) fn cmd_autoremove(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let dry = has_flag(rest, "--dry-run", "-n");
    let targets = match autoremove_targets(prefix, |n| api.dependency_formula(n)) {
        Ok(targets) => targets,
        Err(e) => return failed(&e),
    };
    if targets.is_empty() {
        println!("Nothing to autoremove.");
    }
    for name in targets {
        if dry {
            println!("Would autoremove {name}");
        } else {
            if let Err(e) = install::uninstall(api, prefix, &name, false, true, false) {
                return from_install(prefix, &[], Err(e));
            }
            println!("Autoremoving {name}");
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_link(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    link: bool,
) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("link needs a formula name");
    }
    let dry_run = has_flag(rest, "--dry-run", "-n");
    for name in names {
        let short = Formula::short_name(&name);
        if !link {
            let inst = prefix.installed_one(Kind::Formula, short);
            if dry_run {
                if inst.is_none() {
                    return failed(&format!("abrew: {short} is not installed"));
                }
                match install::unlink_targets(prefix, short) {
                    Ok(targets) => {
                        println!("Would remove:");
                        for t in targets {
                            println!("{}", t.display());
                        }
                    }
                    Err(e) => return failed(&e),
                }
                continue;
            }
            let installed = inst.is_some();
            let shown = match &inst {
                Some(i) => prefix.keg(short, &i.version).display().to_string(),
                None => short.to_string(),
            };
            match install::unlink_keg(prefix, short) {
                Ok(0) if !installed => match install::remove_dangling_links(prefix, short) {
                    Ok(0) => {
                        return failed(&format!("abrew: {short} is not installed"));
                    }
                    Ok(n) => println!("Unlinking {shown}... {n} symlinks removed."),
                    Err(e) => return failed(&e),
                },
                Ok(n) => println!("Unlinking {shown}... {n} symlinks removed."),
                Err(e) => return failed(&e),
            }
            continue;
        }
        let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
            return failed(&format!("abrew: {short} is not installed"));
        };
        let f = match api.formula(short) {
            Ok(f) => f,
            Err(e) => return failed(&e),
        };
        if f.keg_only && !has_flag(rest, "--force", "-f") {
            println!("{short} is keg-only; refreshing its opt link only");
        }
        let keg = prefix.keg(short, &inst.version);
        if dry_run {
            match install::link_destinations(prefix, &f) {
                Ok(targets) => {
                    println!("Would link:");
                    for t in targets {
                        println!("{}", t.display());
                    }
                }
                Err(e) => return from_install(prefix, args, Err(e)),
            }
            continue;
        }
        if !f.keg_only && install::is_linked(prefix, short, &inst.version) {
            eprintln!("Warning: Already linked: {}", keg.display());
            println!("To relink, run:");
            println!("  brew unlink {short} && brew link {short}");
            continue;
        }
        match install::relink(prefix, &f) {
            Ok(n) => println!("Linking {}... {n} symlinks created.", keg.display()),
            Err(e) => return from_install(prefix, args, Err(e)),
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_switch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    let (Some(name), Some(version)) = (ops.first(), ops.get(1)) else {
        return usage("switch needs <formula> <version>");
    };
    let short = Formula::short_name(name);
    if !super::valid_component(short) || !super::valid_cask_version(version) {
        return failed("abrew: invalid formula name or version");
    }
    let f = match api.formula(short) {
        Ok(f) => f,
        Err(e) => return failed(&e),
    };
    if let Err(e) = install::switch_keg(prefix, short, version, f.keg_only) {
        return from_install(prefix, args, Err(e));
    }
    println!("{short} now at {version}");
    ExitCode::SUCCESS
}

#[cfg(unix)]
struct LongLister {
    users: std::collections::BTreeMap<u32, String>,
    groups: std::collections::BTreeMap<u32, String>,
    offsets: std::collections::BTreeMap<i64, i64>,
    now: i64,
}

#[cfg(unix)]
impl LongLister {
    fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        LongLister {
            users: Default::default(),
            groups: Default::default(),
            offsets: Default::default(),
            now,
        }
    }

    fn offset_at(&mut self, secs: i64) -> i64 {
        *self
            .offsets
            .entry(secs)
            .or_insert_with(|| aterm_uds::zone::local_offset_at(secs).unwrap_or(0))
    }

    fn date(&mut self, mtime: i64) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let local = i128::from(mtime) + i128::from(self.offset_at(mtime));
        let days = i64::try_from(local.div_euclid(86_400)).expect("local seconds fit i64 days");
        let (year, month, day) = aterm_types::rfc3339::civil_from_days(days);
        let month = MONTHS
            .get(usize::try_from(month - 1).unwrap_or(0))
            .copied()
            .unwrap_or("Jan");
        let age = i128::from(self.now) - i128::from(mtime);
        if !(-3_600..=15_552_000).contains(&age) {
            format!("{month} {day:>2}  {year}")
        } else {
            let secs = local.rem_euclid(86_400);
            format!("{month} {day:>2} {:02}:{:02}", secs / 3_600, secs / 60 % 60)
        }
    }

    fn user(&mut self, uid: u32) -> String {
        self.users
            .entry(uid)
            .or_insert_with(|| {
                aterm_uds::account::user_name(uid).unwrap_or_else(|| uid.to_string())
            })
            .clone()
    }

    fn group(&mut self, gid: u32) -> String {
        self.groups
            .entry(gid)
            .or_insert_with(|| gid.to_string())
            .clone()
    }

    fn line(&mut self, path: &std::path::Path) -> String {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return format!("?????????? {}", path.display());
        };
        let user = self.user(meta.uid());
        let group = self.group(meta.gid());
        let date = self.date(meta.mtime());
        let mut shown = path.display().to_string();
        if meta.file_type().is_symlink()
            && let Ok(target) = std::fs::read_link(path)
        {
            shown = format!("{shown} -> {}", target.display());
        }
        format!(
            "{} {:>2} {} {} {:>8} {} {}",
            mode_string(&meta),
            meta.nlink(),
            user,
            group,
            meta.len(),
            date,
            shown
        )
    }
}

#[cfg(unix)]
fn mode_string(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let mode = meta.mode();
    let kind = if meta.is_dir() {
        'd'
    } else if meta.file_type().is_symlink() {
        'l'
    } else {
        '-'
    };
    let triplet = |shift: u32, special: u32, mark: char| -> [char; 3] {
        let bits = (mode >> shift) & 0o7;
        let exec = if mode & special != 0 {
            if bits & 0o1 != 0 {
                mark.to_ascii_lowercase()
            } else {
                mark
            }
        } else if bits & 0o1 != 0 {
            'x'
        } else {
            '-'
        };
        [
            if bits & 0o4 != 0 { 'r' } else { '-' },
            if bits & 0o2 != 0 { 'w' } else { '-' },
            exec,
        ]
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    for chars in [
        triplet(6, 0o4000, 'S'),
        triplet(3, 0o2000, 'S'),
        triplet(0, 0o1000, 'T'),
    ] {
        out.extend(chars);
    }
    out
}

fn list_json(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let opt_str = |v: Option<String>| match v {
        Some(s) => aterm_json::Value::String(s),
        None => aterm_json::Value::Null,
    };
    let version_values = |i: &Installed| {
        let mut vs = i.versions.clone();
        vs.sort();
        vs.dedup();
        vs.into_iter().map(aterm_json::Value::String).collect()
    };
    let mut formulae = Vec::new();
    if !has_flag(rest, "--cask", "") {
        for i in prefix.installed(Kind::Formula) {
            let opt = prefix.optlinked_version(&i.name);
            let linked = linked_keg_version(prefix, &i);
            let mut map = aterm_json::Map::new();
            map.insert("name".into(), aterm_json::Value::String(i.name.clone()));
            map.insert(
                "versions".into(),
                aterm_json::Value::Array(version_values(&i)),
            );
            map.insert("linked_version".into(), opt_str(linked));
            map.insert("optlinked_version".into(), opt_str(opt));
            map.insert(
                "pinned_version".into(),
                opt_str(prefix.pinned_version(&i.name)),
            );
            formulae.push(aterm_json::Value::Object(map));
        }
    }
    let mut casks = Vec::new();
    if !has_flag(rest, "--formula", "") {
        for i in prefix.installed(Kind::Cask) {
            let mut map = aterm_json::Map::new();
            map.insert("token".into(), aterm_json::Value::String(i.name.clone()));
            map.insert(
                "versions".into(),
                aterm_json::Value::Array(version_values(&i)),
            );
            map.insert("pinned_version".into(), aterm_json::Value::Null);
            casks.push(aterm_json::Value::Object(map));
        }
    }
    let mut root = aterm_json::Map::new();
    root.insert("formulae".into(), aterm_json::Value::Array(formulae));
    root.insert("casks".into(), aterm_json::Value::Array(casks));
    match aterm_json::to_string(&aterm_json::Value::Object(root)) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => failed(&format!("abrew: cannot encode list JSON: {e}")),
    }
}

fn multiple_version_lines(rows: &[Installed]) -> Vec<String> {
    let mut rows: Vec<&Installed> = rows.iter().filter(|i| i.versions.len() >= 2).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows.iter()
        .map(|i| format!("{} {}", i.name, i.versions.join(" ")))
        .collect()
}

fn list_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(d: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(d) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    walk(&e.path(), out);
                } else if e.file_name() != ".DS_Store" {
                    out.push(e.path());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

pub(super) fn cmd_list(prefix: &Prefix, rest: &[String]) -> ExitCode {
    if has_flag(rest, "--json", "") {
        return list_json(prefix, rest);
    }
    let versions = has_flag(rest, "--versions", "");
    let multiple = has_flag(rest, "--multiple", "");
    let full_name = has_flag(rest, "--full-name", "");
    let long = has_flag(rest, "--long", "-l");
    let named = operands(rest);
    if multiple {
        let mut failed_any = false;
        let rows: Vec<Installed> = if named.is_empty() {
            prefix.installed(Kind::Formula)
        } else {
            named
                .iter()
                .filter_map(
                    |n| match prefix.installed_one(Kind::Formula, Formula::short_name(n)) {
                        Some(i) => Some(i),
                        None => {
                            failed_any = true;
                            None
                        }
                    },
                )
                .collect()
        };
        for line in multiple_version_lines(&rows) {
            println!("{line}");
        }
        return if failed_any {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    if versions && !named.is_empty() {
        let mut rows = Vec::new();
        for name in &named {
            let short = Formula::short_name(name);
            let cask_short = super::api::canonical_cask_token(short);
            let kind = if has_flag(rest, "--cask", "")
                || (!has_flag(rest, "--formula", "")
                    && prefix.installed_one(Kind::Formula, short).is_none()
                    && prefix.installed_one(Kind::Cask, cask_short).is_some())
            {
                Kind::Cask
            } else {
                Kind::Formula
            };
            let lookup = if kind == Kind::Cask {
                cask_short
            } else {
                short
            };
            match prefix.installed_one(kind, lookup) {
                Some(inst) => rows.push(inst),
                None => return failed(&format!("abrew: {short} is not installed")),
            }
        }
        for inst in rows {
            println!("{} {}", inst.name, inst.versions.join(" "));
        }
        return ExitCode::SUCCESS;
    }
    if !named.is_empty() {
        let _ = long;
        let mut roots: Vec<std::path::PathBuf> = Vec::new();
        let mut seen: Vec<(Kind, String)> = Vec::new();
        for name in &named {
            let short = Formula::short_name(name).to_string();
            let cask_short = super::api::canonical_cask_token(&short).to_string();
            let kind = if has_flag(rest, "--cask", "")
                || (!has_flag(rest, "--formula", "")
                    && prefix.installed_one(Kind::Formula, &short).is_none()
                    && prefix.installed_one(Kind::Cask, &cask_short).is_some())
            {
                Kind::Cask
            } else {
                Kind::Formula
            };
            let lookup = if kind == Kind::Cask {
                &cask_short
            } else {
                &short
            };
            let Some(inst) = prefix.installed_one(kind, lookup) else {
                return failed(&format!("abrew: {short} is not installed"));
            };
            if seen.contains(&(kind, lookup.clone())) {
                continue;
            }
            seen.push((kind, lookup.clone()));
            match kind {
                Kind::Formula => {
                    for v in &inst.versions {
                        roots.push(prefix.keg(&short, v));
                    }
                }
                Kind::Cask => roots.push(prefix.caskroom().join(&cask_short)),
            }
        }
        for root in &roots {
            for path in list_files(root) {
                println!("{}", path.display());
            }
        }
        return ExitCode::SUCCESS;
    }
    for casks in [false, true] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        let kind = if casks { Kind::Cask } else { Kind::Formula };
        #[cfg(unix)]
        let mut lister = long.then(LongLister::new);
        for i in prefix.installed(kind) {
            if has_flag(rest, "--pinned", "") && (casks || !prefix.is_pinned(&i.name)) {
                continue;
            }
            #[cfg(unix)]
            if let Some(lister) = &mut lister {
                let dir = match kind {
                    Kind::Formula => prefix.keg(&i.name, &i.version),
                    Kind::Cask => prefix.cask_dir(&i.name, &i.version),
                };
                println!("{}", lister.line(&dir));
                continue;
            }
            let shown = if full_name {
                prefix.full_name(&i)
            } else {
                i.name.clone()
            };
            if versions {
                println!("{} {}", shown, i.versions.join(" "));
            } else {
                println!("{shown}");
            }
        }
    }
    ExitCode::SUCCESS
}

fn lookup_error(
    api: &Api,
    name: &str,
    kind: &str,
    other_kind_exists: impl FnOnce() -> bool,
    e: String,
) -> String {
    if !e.starts_with("abrew: no such ") {
        return e;
    }
    let short = Formula::short_name(name);
    if other_kind_exists() {
        let other = if kind == "cask" { "formula" } else { "cask" };
        format!("abrew: no such {kind}: {short} (it is a {other}; drop --{kind})")
    } else {
        no_such(api, name, Some(kind))
    }
}

enum FetchJob {
    Cask(super::formula::Cask),
    Formula(Formula, Vec<Formula>),
}

pub(super) fn cmd_fetch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("fetch needs a name");
    }
    let mut jobs = Vec::new();
    for name in &names {
        if has_flag(rest, "--cask", "") {
            match api.cask(name) {
                Ok(c) => {
                    let c = match cask::resolve(&c, super::machine::Machine::this()) {
                        Ok(c) => c.into_owned(),
                        Err(e) => return from_install(prefix, args, Err(e)),
                    };
                    if !super::valid_component(&c.token) || !super::valid_cask_version(&c.version) {
                        return failed("abrew: unsupported cask token/version");
                    }
                    if c.disabled {
                        return failed(&format!("abrew: cask {} is disabled upstream", c.token));
                    }
                    jobs.push(FetchJob::Cask(c));
                }
                Err(e) => {
                    return failed(&lookup_error(
                        api,
                        name,
                        "cask",
                        || api.formula(name).is_ok(),
                        e,
                    ));
                }
            }
            continue;
        }
        match api.formula(name) {
            Ok(f) => {
                let deps = if has_flag(rest, "--deps", "") {
                    match install::fetch_plan(api, name) {
                        Ok(plan) => plan.into_iter().filter(|dep| dep.name != f.name).collect(),
                        Err(e) => return from_install(prefix, args, Err(e)),
                    }
                } else {
                    Vec::new()
                };
                jobs.push(FetchJob::Formula(f, deps));
            }
            Err(e) => {
                return failed(&lookup_error(
                    api,
                    name,
                    "formula",
                    || api.cask(Formula::short_name(name)).is_ok(),
                    e,
                ));
            }
        }
    }
    for job in &jobs {
        match job {
            FetchJob::Cask(c) => {
                let dest = prefix.cache().join(cask::download_name(c));
                let expected = c.sha256.as_deref().unwrap_or("no_check");
                let reusable = c.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
                if !reusable && let Err(e) = fetch::curl_to_file(&c.url, &dest, &[]) {
                    return failed(&e);
                }
                if let Err(e) = fetch::verify_sha256(&dest, expected) {
                    return failed(&e);
                }
                println!("Downloaded to {}", dest.display());
            }
            FetchJob::Formula(f, deps) => {
                for dep in deps {
                    if let Err(e) = install::fetch_only(prefix, dep) {
                        return from_install(prefix, args, Err(e));
                    }
                }
                if let Err(e) = install::fetch_only(prefix, f) {
                    return from_install(prefix, args, Err(e));
                }
            }
        }
    }
    ExitCode::SUCCESS
}

fn flat_dependencies(
    f: &Formula,
    direct: bool,
    mut read: impl FnMut(&str) -> Option<Formula>,
) -> std::collections::BTreeSet<String> {
    let root = Formula::short_name(&f.name).to_string();
    let mut seen: std::collections::BTreeSet<String> = [root.clone()].into_iter().collect();
    let mut stack: Vec<String> = f.dependencies.clone();
    while let Some(d) = stack.pop() {
        let short = Formula::short_name(&d).to_string();
        if seen.insert(short.clone())
            && !direct
            && let Some(df) = read(&short)
        {
            stack.extend(df.dependencies.iter().cloned());
        }
    }
    seen.remove(&root);
    seen
}

fn ordered_dependencies(
    f: &Formula,
    direct: bool,
    topological: bool,
    mut read: impl FnMut(&str) -> Option<Formula>,
) -> Vec<String> {
    if !topological {
        return flat_dependencies(f, direct, read).into_iter().collect();
    }
    if direct {
        let mut seen = std::collections::BTreeSet::new();
        let root = Formula::short_name(&f.name);
        return f
            .dependencies
            .iter()
            .map(|d| Formula::short_name(d).to_string())
            .filter(|d| d != root && seen.insert(d.clone()))
            .collect();
    }
    enum Step {
        Enter(String),
        Emit(String),
    }
    let root = Formula::short_name(&f.name).to_string();
    let mut on_stack: std::collections::BTreeSet<String> = [root].into_iter().collect();
    let mut emitted: std::collections::BTreeSet<String> = Default::default();
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<Step> = f
        .dependencies
        .iter()
        .rev()
        .map(|d| Step::Enter(Formula::short_name(d).to_string()))
        .collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(name) => {
                if emitted.contains(&name) || !on_stack.insert(name.clone()) {
                    continue;
                }
                let children = read(&name).map(|df| df.dependencies).unwrap_or_default();
                stack.push(Step::Emit(name));
                for child in children.iter().rev() {
                    stack.push(Step::Enter(Formula::short_name(child).to_string()));
                }
            }
            Step::Emit(name) => {
                on_stack.remove(&name);
                emitted.insert(name.clone());
                out.push(name);
            }
        }
    }
    out
}

pub(super) fn cmd_deps(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let tree = has_flag(rest, "--tree", "");
    let direct = has_flag(rest, "--direct", "-1")
        || has_flag(rest, "--1", "")
        || has_flag(rest, "--declared", "");
    let for_each = has_flag(rest, "--for-each", "");
    let union = has_flag(rest, "--union", "");
    let topological = has_flag(rest, "--topological", "-n");
    let names = operands(rest);
    let installed_only = has_flag(rest, "--installed", "");
    let is_installed = |name: &str| {
        prefix
            .installed_one(Kind::Formula, Formula::short_name(name))
            .is_some()
    };
    if installed_only && names.is_empty() {
        for i in prefix.installed(Kind::Formula) {
            if let Ok(f) = api.formula(&i.name) {
                if tree {
                    println!("{}", f.name);
                    dependency_tree(
                        &f,
                        |n| api.formula(n).ok(),
                        |_| true,
                        direct,
                        |line| println!("{line}"),
                    );
                } else {
                    let deps = flat_dependencies(&f, direct, |n| api.formula(n).ok());
                    let shown: Vec<&str> = deps
                        .iter()
                        .filter(|d| is_installed(d))
                        .map(String::as_str)
                        .collect();
                    println!("{}: {}", f.full_name, shown.join(" "));
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    if names.is_empty() {
        return usage("deps needs a formula name (or --installed)");
    }
    let mut formulae = Vec::new();
    for name in &names {
        match api.formula(name).or_else(|_| {
            tap::lookup(prefix, name).ok_or_else(|| format!("abrew: no formula {name}"))
        }) {
            Ok(f) => formulae.push(f),
            Err(e) => return failed(&e),
        }
    }
    if tree {
        let mut include = |name: &str| !installed_only || is_installed(name);
        for f in &formulae {
            println!("{}", f.name);
            dependency_tree(
                f,
                |n| api.formula(n).ok(),
                &mut include,
                direct,
                |line| println!("{line}"),
            );
        }
        return ExitCode::SUCCESS;
    }
    if for_each {
        formulae.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &formulae {
            let deps = flat_dependencies(f, direct, |n| api.formula(n).ok());
            let shown: Vec<&str> = deps
                .iter()
                .filter(|d| !installed_only || is_installed(d))
                .map(String::as_str)
                .collect();
            println!("{}: {}", f.full_name, shown.join(" "));
        }
        return ExitCode::SUCCESS;
    }
    let mut lists = formulae
        .iter()
        .map(|f| ordered_dependencies(f, direct, topological, |n| api.formula(n).ok()));
    let mut all = lists.next().unwrap_or_default();
    for list in lists {
        if union {
            for d in list {
                if !all.contains(&d) {
                    all.push(d);
                }
            }
        } else {
            all.retain(|d| list.contains(d));
        }
    }
    if !topological {
        all.sort();
    }
    for d in all.iter().filter(|d| !installed_only || is_installed(d)) {
        println!("{d}");
    }
    ExitCode::SUCCESS
}

fn included_dependencies<'a>(
    f: &'a Formula,
    include: &mut impl FnMut(&str) -> bool,
) -> Vec<&'a String> {
    f.dependencies.iter().filter(|name| include(name)).collect()
}

fn dependency_tree(
    f: &Formula,
    mut read: impl FnMut(&str) -> Option<Formula>,
    mut include: impl FnMut(&str) -> bool,
    direct: bool,
    mut emit: impl FnMut(String),
) {
    fn walk(
        f: &Formula,
        depth: usize,
        direct: bool,
        seen: &mut Vec<String>,
        read: &mut impl FnMut(&str) -> Option<Formula>,
        include: &mut impl FnMut(&str) -> bool,
        emit: &mut impl FnMut(String),
    ) {
        if depth > 12 {
            return;
        }
        let children = included_dependencies(f, include);
        for (i, d) in children.iter().enumerate() {
            let last = i + 1 == children.len();
            let branch = if last { "└── " } else { "├── " };
            emit(format!("{}{branch}{d}", "    ".repeat(depth - 1)));
            if direct {
                continue;
            }
            let short = Formula::short_name(d).to_string();
            if !seen.contains(&short) {
                seen.push(short.clone());
                if let Some(df) = read(&short) {
                    walk(&df, depth + 1, direct, seen, read, include, emit);
                }
            }
        }
    }
    walk(
        f,
        1,
        direct,
        &mut Vec::new(),
        &mut read,
        &mut include,
        &mut emit,
    );
}

pub(super) fn cmd_uses(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("uses needs a formula name");
    }
    for name in names {
        let short = Formula::short_name(&name);
        let users = match install::dependents(api, prefix, short) {
            Ok(users) => users,
            Err(error) => return from_install(prefix, &[], Err(error)),
        };
        for u in users {
            println!("{u}");
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_leaves(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let on_request_only = has_flag(rest, "--installed-on-request", "");
    let as_dependency_only = has_flag(rest, "--installed-as-dependency", "");
    let installed = prefix.installed(Kind::Formula);
    let mut needed: std::collections::BTreeSet<String> = Default::default();
    for i in &installed {
        let f = match api.formula(&i.name) {
            Ok(f) => f,
            Err(error) => return failed(&error),
        };
        for d in f.dependencies {
            needed.insert(Formula::short_name(&d).to_string());
        }
    }
    for i in &installed {
        if needed.contains(&i.name) {
            continue;
        }
        if on_request_only && !super::prefix::receipt_on_request(&prefix.keg(&i.name, &i.version)) {
            continue;
        }
        if as_dependency_only && super::prefix::receipt_on_request(&prefix.keg(&i.name, &i.version))
        {
            continue;
        }
        println!("{}", i.name);
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_missing(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let scope: Vec<String> = operands(rest)
        .iter()
        .map(|n| Formula::short_name(n).to_string())
        .collect();
    for name in &scope {
        if prefix.installed_one(Kind::Formula, name).is_none() {
            return failed(&format!("abrew: {name} is not installed"));
        }
    }
    let hidden: Vec<String> = rest
        .iter()
        .filter_map(|arg| arg.strip_prefix("--hide="))
        .flat_map(|v| v.split(','))
        .map(|part| Formula::short_name(part.trim()).to_string())
        .collect();
    let mut any = false;
    for i in prefix.installed(Kind::Formula) {
        if !scope.is_empty() && !scope.contains(&i.name) {
            continue;
        }
        let f = match api.formula(&i.name) {
            Ok(f) => f,
            Err(error) => return failed(&error),
        };
        for d in &f.dependencies {
            let short = Formula::short_name(d);
            if hidden.iter().any(|h| h == short) {
                continue;
            }
            if prefix.installed_one(Kind::Formula, short).is_none() {
                println!("{}: missing {}", i.name, short);
                any = true;
            }
        }
    }
    if any {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `brew options`: print declared formula options or delegate tap-only formulae.
pub(super) fn cmd_options(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
) -> ExitCode {
    let compact = has_flag(rest, "--compact", "");
    let names = operands(rest);
    let tapped = || {
        from_install(
            prefix,
            args,
            Err(install::InstallError::Unsupported(
                "options for tapped formulae live in tap recipes".to_string(),
            )),
        )
    };
    let rows: Vec<super::api::OptionsRow> = if names.is_empty() {
        if !tap::list(prefix).is_empty() {
            return tapped();
        }
        match api.options_index() {
            Ok(rows) => rows,
            Err(e) => return failed(&e),
        }
    } else if has_flag(rest, "--installed", "") {
        let mut rows = Vec::new();
        for i in prefix.installed(Kind::Formula) {
            match api.formula_value(&i.name) {
                Ok(v) => {
                    let (options, head) = super::api::parse_options_record(&v);
                    let full_name = v
                        .get("full_name")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or(&i.name)
                        .to_string();
                    rows.push(super::api::OptionsRow {
                        full_name,
                        options,
                        head,
                    });
                }
                Err(_) => return tapped(),
            }
        }
        rows
    } else {
        let mut rows = Vec::new();
        for name in &names {
            match api.formula_value(name) {
                Ok(v) => {
                    let (options, head) = super::api::parse_options_record(&v);
                    let full_name = v
                        .get("full_name")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or(name)
                        .to_string();
                    rows.push(super::api::OptionsRow {
                        full_name,
                        options,
                        head,
                    });
                }
                Err(_) if tap::lookup(prefix, name).is_some() => return tapped(),
                Err(_) => return failed(&no_such(api, name, Some("formula"))),
            }
        }
        rows
    };
    print_options(&rows, compact);
    ExitCode::SUCCESS
}

fn print_options(rows: &[super::api::OptionsRow], compact: bool) {
    let multiple = rows.len() > 1;
    for row in rows {
        if row.options.is_empty() {
            continue;
        }
        if compact {
            let flags: Vec<String> = row.options.iter().map(|(f, _)| format!("--{f}")).collect();
            println!("{}", flags.join(" "));
        } else {
            if multiple {
                println!("{}", row.full_name);
            }
            for (flag, desc) in &row.options {
                println!("--{flag}");
                println!("\t{desc}");
            }
            if row.head {
                println!("--HEAD");
                println!("\tInstall HEAD version");
            }
            println!();
        }
    }
}

fn is_macho(head: &[u8]) -> bool {
    matches!(
        head.get(..4),
        Some(
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    ) && !crate::macho::is_java_class(head)
}

fn linkage_problem(
    dylib: &str,
    file_dir: &std::path::Path,
    lib_dirs: &[std::path::PathBuf],
    exists: &impl Fn(&std::path::Path) -> bool,
) -> Option<String> {
    if dylib.contains("@@HOMEBREW")
        || dylib.starts_with("/opt/homebrew/")
        || dylib.starts_with("/usr/local/Cellar/")
        || dylib.starts_with("/home/linuxbrew/")
    {
        return Some(format!("unrelocated reference {dylib}"));
    }
    if dylib.starts_with("/usr/lib/") || dylib.starts_with("/System/") {
        return None;
    }
    if let Some(rest) = dylib.strip_prefix("@rpath/") {
        if lib_dirs.iter().any(|d| exists(&d.join(rest))) {
            return None;
        }
        return Some(format!("unresolved @rpath reference {dylib}"));
    }
    for token in ["@loader_path/", "@executable_path/"] {
        if let Some(rest) = dylib.strip_prefix(token) {
            if exists(&file_dir.join(rest)) {
                return None;
            }
            return Some(format!("missing reference {dylib}"));
        }
    }
    if dylib.starts_with('/') {
        if exists(std::path::Path::new(dylib)) {
            return None;
        }
        return Some(format!("missing reference {dylib}"));
    }
    None
}

/// The install name and the dylibs the Mach-O at `path` loads, each once
/// in load-command order: what `otool -L` lists, read in house
/// ([`crate::macho::load_names_at`]) rather than through `otool`, whose
/// `/usr/bin` shim raises Apple's install dialog on a Mac without the
/// developer tools.
///
/// # Errors
/// The file cannot be read or its load commands are malformed: a problem
/// to report, never an empty list.
fn linked_dylibs(path: &std::path::Path) -> Result<Vec<String>, String> {
    let names = crate::macho::load_names_at(path)
        .map_err(|error| format!("inspection failed: {error}"))?
        .once();
    let mut rows = names.id;
    for dylib in names.loads {
        if !rows.contains(&dylib) {
            rows.push(dylib);
        }
    }
    Ok(rows)
}

fn linked_library_exists(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

pub(super) fn cmd_linkage(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("linkage needs a formula name");
    }
    let mut kegs = Vec::new();
    for name in &names {
        let short = Formula::short_name(name);
        let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
            return failed(&format!("abrew: {short} is not installed"));
        };
        kegs.push((short.to_string(), inst));
    }
    let mut any_problems = false;
    for (short, inst) in &kegs {
        let short = short.as_str();
        let keg = prefix.keg(short, &inst.version);
        if !cfg!(target_os = "macos") {
            println!("{short}: linkage inspection reads Mach-O load commands: macOS only");
            continue;
        }
        let lib_dirs = [keg.join("lib"), prefix.root.join("lib")];
        let exists = linked_library_exists;
        let mut checked = 0usize;
        let mut problems = 0usize;
        fn walk(
            dir: &std::path::Path,
            lib_dirs: &[std::path::PathBuf],
            exists: &impl Fn(&std::path::Path) -> bool,
            checked: &mut usize,
            problems: &mut usize,
        ) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                let p = e.path();
                if ft.is_dir() {
                    walk(&p, lib_dirs, exists, checked, problems);
                    continue;
                }
                if !ft.is_file() {
                    continue;
                }
                let mut head = [0u8; 8];
                let is_bin = std::fs::File::open(&p)
                    .and_then(|mut f| {
                        use std::io::Read as _;
                        f.read_exact(&mut head)
                    })
                    .is_ok()
                    && is_macho(&head);
                if !is_bin {
                    continue;
                }
                let rows = match linked_dylibs(&p) {
                    Ok(rows) => rows,
                    Err(problem) => {
                        println!("{}: {problem}", p.display());
                        *problems += 1;
                        continue;
                    }
                };
                *checked += 1;
                let file_dir = p.parent().unwrap_or(dir);
                for dylib in rows {
                    if let Some(problem) = linkage_problem(&dylib, file_dir, lib_dirs, exists) {
                        println!("{}: {problem}", p.display());
                        *problems += 1;
                    }
                }
            }
        }
        walk(&keg, &lib_dirs, &exists, &mut checked, &mut problems);
        println!("{short}: {checked} Mach-O files checked, {problems} problem(s)");
        any_problems |= problems > 0;
    }
    if any_problems {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

pub(super) fn cmd_log(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    let Some(name) = names.first() else {
        for t in tap::list(prefix) {
            println!("==> tap {}", t.name());
            let _ = fetch::run_inherit(
                aterm_uds::os_path!("git"),
                &["-C", &t.path.to_string_lossy(), "log", "--oneline", "-5"],
            );
        }
        return ExitCode::SUCCESS;
    };
    let short = Formula::short_name(name);
    match prefix.installed_one(Kind::Formula, short) {
        Some(_) => {
            println!("{short}: installed by abrew (see INSTALL_RECEIPT.json in the keg)");
            ExitCode::SUCCESS
        }
        None => {
            println!("{short}: no abrew install record (abrew keeps no build logs)");
            ExitCode::SUCCESS
        }
    }
}

pub(super) fn cmd_tap(prefix: &Prefix, rest: &[String]) -> ExitCode {
    if has_flag(rest, "--list-official", "") {
        for t in [
            "homebrew/bundle",
            "homebrew/cask",
            "homebrew/core",
            "homebrew/services",
        ] {
            println!("{t}");
        }
        return ExitCode::SUCCESS;
    }
    let ops = operands(rest);
    if ops.is_empty() {
        for t in tap::list(prefix) {
            println!("{}", t.name());
        }
        return ExitCode::SUCCESS;
    }
    let url = ops.get(1).map(String::as_str);
    match tap::add(prefix, ops.first().map_or("", String::as_str), url) {
        Ok((t, newly)) => {
            if newly {
                println!("Tapped {} ({})", t.name(), t.path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

pub(super) fn cmd_tap_info(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    let taps = tap::list(prefix);
    let count_rb = |d: &std::path::Path, acc: &mut usize| {
        if let Ok(entries) = std::fs::read_dir(d) {
            for e in entries.flatten() {
                if e.path().extension().is_some_and(|x| x == "rb") {
                    *acc += 1;
                } else if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                    count_rb_depth(&e.path(), acc, 0);
                }
            }
        }
    };
    if ops.is_empty() && taps.is_empty() {
        println!("0 taps, 0 private, 0 formulae, 0 commands, 0 casks");
        return ExitCode::SUCCESS;
    }
    for op in &ops {
        if !taps.iter().any(|t| &t.name() == op) {
            println!("{op}: Not installed");
        }
    }
    let show: Vec<&tap::Tap> = if ops.is_empty() {
        taps.iter().collect()
    } else {
        taps.iter()
            .filter(|t| ops.iter().any(|o| o == &t.name()))
            .collect()
    };
    for t in show {
        let mut formulas = 0usize;
        let mut casks = 0usize;
        count_rb(&t.path.join("Formula"), &mut formulas);
        count_rb(&t.path.join("Casks"), &mut casks);
        if formulas == 0 && casks == 0 {
            count_rb(&t.path, &mut formulas);
        }
        println!("{}: {} formula(e), {} cask(s)", t.name(), formulas, casks);
        println!("  path: {}", t.path.display());
    }
    ExitCode::SUCCESS
}

fn count_rb_depth(dir: &std::path::Path, acc: &mut usize, depth: usize) {
    if depth > 2 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "rb") {
                *acc += 1;
            } else if e.file_type().is_ok_and(|t| t.is_dir()) {
                count_rb_depth(&e.path(), acc, depth + 1);
            }
        }
    }
}

pub(super) fn cmd_bundle(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    rest: &[String],
    prepared: Option<Vec<bundle::BrewfileEntry>>,
) -> ExitCode {
    let sub = rest
        .first()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'))
        .unwrap_or("install");
    match sub {
        "status" => {
            bundle::bundle_status(prefix);
            ExitCode::SUCCESS
        }
        "remove" => from_install(prefix, args, bundle::bundle_remove(prefix)),
        "dump" => {
            let text = bundle::brewfile_dump(prefix);
            let mut file: Option<std::path::PathBuf> = None;
            let mut force = false;
            let mut it = rest[1..].iter();
            while let Some(arg) = it.next() {
                if arg == "--file" {
                    file = it.next().map(std::path::PathBuf::from);
                } else if let Some(path) = arg.strip_prefix("--file=") {
                    file = Some(std::path::PathBuf::from(path));
                } else if matches!(arg.as_str(), "--force" | "-f") {
                    force = true;
                }
            }
            match file {
                None => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Some(path) => match write_bundle_dump(&path, text.as_bytes(), force) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(e) => failed(&format!("abrew: cannot write {}: {e}", path.display())),
                },
            }
        }
        "install" | "check" => match prepared {
            Some(entries) => bundle_file(prefix, api, args, &entries, sub),
            None => failed("abrew: Brewfile was not admitted"),
        },
        _ => super::unsupported("bundle subcommand is not native"),
    }
}

fn write_bundle_dump(path: &std::path::Path, text: &[u8], force: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    options.open(path)?.write_all(text)
}

fn bundle_file(
    prefix: &Prefix,
    api: &Api,
    args: &[String],
    entries: &[bundle::BrewfileEntry],
    sub: &str,
) -> ExitCode {
    let dry = sub == "check";
    let mut missing = false;
    let mut landed = false;
    let mut failures = 0;
    for entry in entries {
        let result: Result<(), install::InstallError> = match entry {
            bundle::BrewfileEntry::Tap(n) => {
                if dry {
                    let have = tap::list(prefix).iter().any(|t| t.name() == *n);
                    missing |= !have;
                    println!("tap {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else {
                    match tap::add(prefix, n, None) {
                        Ok(_) => {
                            landed = true;
                            Ok(())
                        }
                        Err(e) => return failed(&e),
                    }
                }
            }
            bundle::BrewfileEntry::Brew(n) => {
                let have = prefix
                    .installed_one(Kind::Formula, Formula::short_name(n))
                    .is_some();
                if dry {
                    missing |= !have;
                    println!("brew {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else if !have {
                    install::install(api, prefix, n, &InstallOpts::default()).map(|_| ())
                } else {
                    Ok(())
                }
            }
            bundle::BrewfileEntry::Cask(n) => {
                let have = prefix
                    .installed_one(Kind::Cask, super::api::canonical_cask_token(n))
                    .is_some();
                if dry {
                    missing |= !have;
                    println!("cask {n}: {}", if have { "present" } else { "missing" });
                    Ok(())
                } else if !have {
                    match api.cask(n) {
                        Ok(c) => {
                            cask::install_with_deps(api, prefix, &c, false, false, false, false)
                                .map(|_| ())
                        }
                        Err(e) => return failed(&e),
                    }
                } else {
                    Ok(())
                }
            }
        };
        match result {
            Ok(()) => {
                if !dry {
                    landed = true;
                }
            }
            Err(e @ install::InstallError::Unsupported(_)) if landed && entries.len() > 1 => {
                let install::InstallError::Unsupported(reason) = &e else {
                    unreachable!()
                };
                eprintln!("abrew: skipping Brewfile entry ({reason})");
                failures += 1;
            }
            Err(e) => return from_install(prefix, args, Err(e)),
        }
    }
    if missing || failures > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn unlinked_keg_judges_only_receipt_recorded_linkable_kegs() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        p.ensure().unwrap();
        let mk = |name: &str, keg_only: Option<&str>, with_bin: bool| {
            let dir = p.keg(name, "1.0");
            std::fs::create_dir_all(&dir).unwrap();
            let mut fields = vec![
                ("installed_by", "abrew"),
                ("name", name),
                ("version", "1.0"),
            ];
            if let Some(v) = keg_only {
                fields.push(("keg_only", v));
            }
            p.write_receipt(&dir, &fields).unwrap();
            if with_bin {
                std::fs::create_dir_all(dir.join("bin")).unwrap();
                std::fs::write(dir.join("bin").join(name), b"#!/bin/sh\n").unwrap();
            }
            super::super::prefix::Installed {
                name: name.to_string(),
                version: "1.0".to_string(),
                kind: Kind::Formula,
                versions: vec!["1.0".to_string()],
            }
        };
        let demo = mk("demo", Some("false"), true);
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(keg_bin_linked(&p, "demo", "1.0"), Some(false));
        assert_eq!(linked_keg_version(&p, &demo), None);
        let linked = p.bin().join("demo");
        std::fs::write(&linked, b"foreign file").unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &demo), None);
        std::fs::remove_file(&linked).unwrap();
        let foreign = p.keg("other", "1.0").join("bin/demo");
        std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        std::fs::write(&foreign, b"other keg").unwrap();
        super::super::prefix::symlink_file(&foreign, &linked).unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &demo), None);
        std::fs::remove_file(&linked).unwrap();
        let newer = p.keg("demo", "2.0");
        std::fs::create_dir_all(newer.join("bin")).unwrap();
        std::fs::write(newer.join("bin/demo"), b"newer version").unwrap();
        p.write_receipt(
            &newer,
            &[
                ("installed_by", "abrew"),
                ("name", "demo"),
                ("version", "2.0"),
                ("keg_only", "false"),
            ],
        )
        .unwrap();
        let admitted = p.installed_one(Kind::Formula, "demo").unwrap();
        super::super::prefix::symlink_file(&p.keg("demo", "1.0"), &p.opt("demo")).unwrap();
        assert_eq!(p.optlinked_version("demo").as_deref(), Some("1.0"));
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo"), &linked)
            .unwrap();
        assert!(unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("2.0"));
        std::fs::remove_file(&linked).unwrap();
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/1.0/bin/demo"), &linked)
            .unwrap();
        assert!(!unlinked_keg(&p, &demo));
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
        std::fs::write(newer.join("bin/demo-newer"), b"second command").unwrap();
        let second = p.bin().join("demo-newer");
        super::super::prefix::symlink_file(Path::new("../Cellar/demo/2.0/bin/demo-newer"), &second)
            .unwrap();
        assert_eq!(linked_keg_version(&p, &admitted), None);
        std::fs::remove_file(second).unwrap();
        assert_eq!(linked_keg_version(&p, &admitted).as_deref(), Some("1.0"));
        std::fs::remove_file(&linked).unwrap();
        super::super::prefix::symlink_file(
            &p.keg("demo", "1.0").join("bin/demo"),
            &p.bin().join("demo"),
        )
        .unwrap();
        assert!(!unlinked_keg(&p, &demo));
        for item in [
            mk("kegonly", Some("true"), true),
            mk("legacy", None, true),
            mk("libonly", Some("false"), false),
        ] {
            assert!(!unlinked_keg(&p, &item));
            assert_eq!(keg_bin_linked(&p, &item.name, &item.version), None);
            assert_eq!(linked_keg_version(&p, &item), None);
        }
        let empty = mk("emptybin", Some("false"), false);
        std::fs::create_dir(p.keg("emptybin", "1.0").join("bin")).unwrap();
        assert_eq!(keg_bin_linked(&p, &empty.name, &empty.version), None);
        assert_eq!(linked_keg_version(&p, &empty), None);
        let unreadable = mk("unreadable", Some("false"), false);
        std::fs::write(p.keg("unreadable", "1.0").join("bin"), b"not a directory").unwrap();
        assert_eq!(
            keg_bin_linked(&p, &unreadable.name, &unreadable.version),
            None
        );
        assert!(!unlinked_keg(&p, &unreadable));
        assert_eq!(linked_keg_version(&p, &unreadable), None);
    }

    #[test]
    fn prune_downloads_respects_age_and_dry_run() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let p = Prefix::at(tmp.path());
        std::fs::create_dir_all(p.cache().join("api")).unwrap();
        std::fs::write(p.cache().join("demo-1.bottle.tar.gz"), b"x").unwrap();
        std::fs::write(p.cache().join("api/formula-demo.json"), b"{}").unwrap();
        assert_eq!(prune_downloads(&p, true, None, &[]).unwrap().0, 1);
        assert!(p.cache().join("demo-1.bottle.tar.gz").exists());
        assert_eq!(
            prune_downloads(
                &p,
                false,
                Some(std::time::Duration::from_secs(999_999_999)),
                &[]
            )
            .unwrap()
            .0,
            0
        );
        assert_eq!(
            prune_downloads(&p, false, Some(std::time::Duration::ZERO), &[])
                .unwrap()
                .0,
            1
        );
        assert!(!p.cache().join("demo-1.bottle.tar.gz").exists());
        assert!(p.cache().join("api/formula-demo.json").exists());
        let downloads: [(&str, &[u8]); 5] = [
            ("demo-2.bottle.tar.gz", b"ordinary bottle"),
            (
                "demo-extra-1.bottle.tar.gz",
                b"hyphenated name or alpha version",
            ),
            ("demo-extra-release.zip", b"arbitrary cask URL basename"),
            ("demo-extra-1-fetch", b"stray cache file"),
            ("other-1.bottle.tar.gz", b"other package"),
        ];
        for (file, contents) in downloads {
            std::fs::write(p.cache().join(file), contents).unwrap();
        }
        for (dry, max_age, scope) in [
            (false, Some(std::time::Duration::ZERO), &["demo"][..]),
            (false, None, &["demo-extra"][..]),
            (true, None, &["demo"][..]),
            (false, None, &["demo", "other"][..]),
            (false, None, &["oth"][..]),
        ] {
            assert_eq!(prune_downloads(&p, dry, max_age, scope).unwrap().0, 0);
            for (file, contents) in downloads {
                assert_eq!(std::fs::read(p.cache().join(file)).unwrap(), contents);
            }
        }
        assert_eq!(
            prune_downloads(&p, true, None, &[]).unwrap().0,
            downloads.len()
        );
        for (file, contents) in downloads {
            assert_eq!(std::fs::read(p.cache().join(file)).unwrap(), contents);
        }
        assert_eq!(
            prune_downloads(&p, false, None, &[]).unwrap().0,
            downloads.len()
        );
        for (file, _) in downloads {
            assert!(!p.cache().join(file).exists());
        }
        assert_eq!(
            std::fs::read(p.cache().join("api/formula-demo.json")).unwrap(),
            b"{}"
        );
    }

    #[test]
    fn edit_distance_measures_typo_closeness() {
        assert_eq!(edit_distance("hello", "hello"), 0);
        assert_eq!(edit_distance("helo", "hello"), 1);
        assert_eq!(edit_distance("wget", "wgwt"), 1);
        assert_eq!(edit_distance("wget", "wgt"), 1);
        assert_eq!(edit_distance("tree", "tpe"), 2);
        assert!(edit_distance("hello", "goodbye") > 2);
        assert_eq!(edit_distance("", "ab"), 2);
    }

    #[test]
    fn regex_search_matches_brew_case_and_anchoring() {
        assert_eq!(search_regex_pattern("/wget.+/"), Some("wget.+"));
        assert_eq!(search_regex_pattern("/http client/"), Some("http client"));
        assert_eq!(search_regex_pattern("//"), Some(""));
        assert_eq!(search_regex_pattern("/"), None);
        assert_eq!(search_regex_pattern("/wget"), None);
        assert_eq!(search_regex_pattern("wget/"), None);
        assert_eq!(search_regex_pattern("wget"), None);
        assert_eq!(search_regex_pattern("/a\nb/"), None);

        let query = |p: &str| SearchQuery::Regex(compile_search_regex(p).unwrap());
        assert!(query("wget").hits("wget2", "", false).unwrap());
        assert!(!query("WGET").hits("wget", "", false).unwrap());
        assert!(query("(?i)WGET").hits("wget", "", false).unwrap());
        assert!(query("get").hits("wget", "", false).unwrap());
        assert!(!query("^get").hits("wget", "", false).unwrap());
        assert!(query("^lib.*@\\d$").hits("libfoo@3", "", false).unwrap());
        assert!(query("").hits("anything", "", false).unwrap());
        assert!(!query("HTTP").hits("wget", "HTTP client", false).unwrap());
        assert!(query("HTTP").hits("wget", "HTTP client", true).unwrap());

        for refused in [r"(?=http)client", r"(\w)\1", r"\p{L}+", "(a{200}){200}"] {
            assert!(compile_search_regex(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn text_search_simplifies_like_brew_and_desc_fields_select() {
        assert_eq!(simplify_search_text("Node.js C++ @3"), "nodejsc++@3");
        let q = SearchQuery::text("node js");
        assert!(q.hits("node", "Platform built on Node.js", true).unwrap());
        assert!(
            SearchQuery::text("alt-tab")
                .hits("alt-tab", "", false)
                .unwrap()
        );
        assert!(!q.hits("deno", "A runtime", true).unwrap());

        let by = |s: &str| SearchQuery::text(s);
        assert!(desc_matches(&by("wget"), DescField::Name, false, "wget", "", "HTTP").unwrap());
        assert!(!desc_matches(&by("http"), DescField::Name, false, "wget", "", "HTTP").unwrap());
        assert!(
            desc_matches(
                &by("http"),
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &by("wget"),
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            desc_matches(
                &by("firefox"),
                DescField::Description,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        assert!(
            desc_matches(
                &by("browser"),
                DescField::Either,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &by("safari"),
                DescField::Either,
                true,
                "firefox",
                "Firefox",
                "Web browser"
            )
            .unwrap()
        );
        let re = SearchQuery::Regex(compile_search_regex("HTTP").unwrap());
        assert!(
            desc_matches(
                &re,
                DescField::Description,
                false,
                "wget",
                "",
                "HTTP client"
            )
            .unwrap()
        );
        assert!(
            !desc_matches(
                &re,
                DescField::Description,
                false,
                "wget",
                "",
                "http client"
            )
            .unwrap()
        );
    }

    fn owned_keg(prefix: &Prefix, name: &str, version: &str) {
        let dir = prefix.keg(name, version);
        std::fs::create_dir_all(&dir).unwrap();
        prefix
            .write_receipt(
                &dir,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", version),
                ],
            )
            .unwrap();
    }

    #[test]
    fn analytics_table_matches_brew_layout_at_80_columns() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(59_538), "59,538");
        assert_eq!(format_count(1_234_567), "1,234,567");
        let rows = [
            ("wget".to_string(), 59_416),
            ("wget --HEAD".to_string(), 122),
        ];
        let table = analytics_table("install", "30", &rows, 80);
        let expected = concat!(
            "==> install (30 days)\n",
            "Index | Name (with options)                                  |  Count |  Percent\n",
            "-----:|------------------------------------------------------|-------:|--------:\n",
            "1     | wget                                                 | 59,416 |   99.80%\n",
            "2     | wget --HEAD                                          |    122 |    0.20%\n",
            "Total |                                                      | 59,538 |  100.00%\n",
        );
        assert_eq!(table, expected);
        let one = analytics_table("install", "30", &rows[..1], 80);
        assert!(!one.contains("Total"), "{one}");
        assert!(one.contains("100.00%"), "{one}");
    }

    #[test]
    fn multiple_version_lines_keep_only_multi_version_formulae_sorted() {
        let row = |name: &str, versions: &[&str]| Installed {
            name: name.into(),
            version: versions
                .last()
                .map(|v| (*v).to_string())
                .unwrap_or_default(),
            kind: Kind::Formula,
            versions: versions.iter().map(|v| (*v).to_string()).collect(),
        };
        let rows = [
            row("zeta", &["1", "2"]),
            row("solo", &["3"]),
            row("alpha", &["0.9", "1.0", "1.1"]),
            row("none", &[]),
        ];
        assert_eq!(
            multiple_version_lines(&rows),
            ["alpha 0.9 1.0 1.1", "zeta 1 2"]
        );
    }

    #[test]
    fn flat_dependencies_close_over_the_full_graph_sorted() {
        let formula = |name: &str, deps: &[&str]| Formula {
            name: name.into(),
            dependencies: deps.iter().map(|name| (*name).to_string()).collect(),
            ..Formula::default()
        };
        let app = formula("app", &["zeta", "homebrew/core/mid", "zeta"]);
        let read = |name: &str| match name {
            "mid" => Some(formula("mid", &["leaf"])),
            "leaf" => Some(formula("leaf", &["app"])),
            "zeta" => Some(formula("zeta", &[])),
            _ => None,
        };
        let direct = flat_dependencies(&app, true, |_| {
            panic!("direct sets read nothing below the root")
        });
        assert_eq!(
            direct.iter().collect::<Vec<_>>(),
            [&"mid".to_string(), &"zeta".to_string()]
        );
        let all = flat_dependencies(&app, false, read);
        assert_eq!(
            all.iter().collect::<Vec<_>>(),
            [&"leaf".to_string(), &"mid".to_string(), &"zeta".to_string()]
        );
        assert_eq!(
            ordered_dependencies(&app, false, true, read),
            ["zeta", "leaf", "mid"]
        );
        assert_eq!(
            ordered_dependencies(&app, true, true, |_| {
                panic!("direct topological reads nothing")
            }),
            ["zeta", "mid"]
        );
    }

    #[test]
    fn the_installed_dependency_tree_filters_before_choosing_its_branches() {
        let formula = |name: &str, deps: &[&str]| Formula {
            name: name.into(),
            dependencies: deps.iter().map(|name| (*name).to_string()).collect(),
            ..Formula::default()
        };
        let app = formula("app", &["absent", "homebrew/core/kept", "missing-last"]);
        let read = |name: &str| match name {
            "kept" => Some(formula("kept", &["leaf", "missing-child"])),
            "leaf" => Some(formula("leaf", &["kept"])),
            _ => None,
        };
        let installed = |name: &str| matches!(Formula::short_name(name), "kept" | "leaf");
        assert_eq!(
            included_dependencies(&app, &mut |_: &str| true),
            app.dependencies.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            included_dependencies(&app, &mut |name: &str| installed(name)),
            [&"homebrew/core/kept".to_string()]
        );
        let mut filtered = Vec::new();
        let mut reads = Vec::new();
        dependency_tree(
            &app,
            |name| {
                reads.push(name.to_string());
                read(name)
            },
            installed,
            false,
            |line| filtered.push(line),
        );
        assert_eq!(
            filtered,
            ["└── homebrew/core/kept", "    └── leaf", "        └── kept"]
        );
        assert_eq!(reads, ["kept", "leaf"]);
        let mut direct_lines = Vec::new();
        dependency_tree(
            &app,
            |_| panic!("direct trees read nothing below the root"),
            |_| true,
            true,
            |line| direct_lines.push(line),
        );
        assert_eq!(
            direct_lines,
            ["├── absent", "├── homebrew/core/kept", "└── missing-last"]
        );
        let mut plain = Vec::new();
        dependency_tree(&app, read, |_| true, false, |line| plain.push(line));
        assert_eq!(
            plain,
            [
                "├── absent",
                "├── homebrew/core/kept",
                "    ├── leaf",
                "        └── kept",
                "    └── missing-child",
                "└── missing-last"
            ]
        );
        let mut empty = Vec::new();
        dependency_tree(
            &app,
            |_| panic!("excluded children must not be read"),
            |_| false,
            false,
            |line| empty.push(line),
        );
        assert!(empty.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn long_listing_dates_keep_each_instants_offset_and_handle_extreme_ages() {
        const NOW: i64 = 1_800_000_000;
        const OLD: i64 = NOW - 400 * 86_400;
        const BEFORE: i64 = 1_710_063_000;
        const AFTER: i64 = BEFORE + 3_600;
        assert_eq!(BEFORE.div_euclid(86_400), AFTER.div_euclid(86_400));
        let mut dated = LongLister {
            users: Default::default(),
            groups: Default::default(),
            offsets: [
                (NOW, 0),
                (OLD, 0),
                (BEFORE, -28_800),
                (AFTER, -25_200),
                (-1, 0),
                (i64::MAX, 3_600),
                (i64::MIN, -3_600),
            ]
            .into(),
            now: NOW,
        };
        assert_eq!(dated.date(NOW), "Jan 15 08:00");
        assert_eq!(dated.date(OLD), "Dec 11  2025");
        dated.now = AFTER;
        assert_eq!(dated.date(BEFORE), "Mar 10 01:30");
        assert_eq!(dated.date(AFTER), "Mar 10 03:30");

        for (now, mtime, expected) in [
            (i64::MIN, BEFORE, "Mar 10  2024"),
            (i64::MAX, -1, "Dec 31  1969"),
            (i64::MAX, i64::MAX, "Dec  4 16:30"),
            (i64::MIN, i64::MIN, "Jan 27 07:29"),
        ] {
            dated.now = now;
            assert_eq!(dated.date(mtime), expected, "now={now}, mtime={mtime}");
        }
    }

    #[test]
    fn core_shard_puts_lib_formulae_under_lib() {
        assert_eq!(core_shard("libffi"), "lib");
        assert_eq!(core_shard("lib"), "lib");
        assert_eq!(core_shard("tree"), "t");
        assert_eq!(core_shard("openssl@3"), "o");
    }

    #[test]
    fn linkage_classifier_judges_each_reference_kind() {
        let dir = std::path::Path::new("/keg/bin");
        let libs = [std::path::PathBuf::from("/keg/lib")];
        let present = |p: &std::path::Path| {
            p == std::path::Path::new("/keg/lib/libfoo.dylib")
                || p == std::path::Path::new("/keg/bin/libnear.dylib")
                || p == std::path::Path::new("/elsewhere/libok.dylib")
        };
        assert_eq!(
            linkage_problem("/usr/lib/libSystem.B.dylib", dir, &libs, &present),
            None
        );
        assert_eq!(
            linkage_problem("@rpath/libfoo.dylib", dir, &libs, &present),
            None
        );
        assert_eq!(
            linkage_problem("@loader_path/libnear.dylib", dir, &libs, &present),
            None
        );
        assert_eq!(
            linkage_problem("/elsewhere/libok.dylib", dir, &libs, &present),
            None
        );
        assert!(
            linkage_problem("/opt/homebrew/lib/libx.dylib", dir, &libs, &present)
                .is_some_and(|p| p.contains("unrelocated"))
        );
        assert!(
            linkage_problem("@@HOMEBREW_PREFIX@@/lib/libx.dylib", dir, &libs, &present)
                .is_some_and(|p| p.contains("unrelocated"))
        );
        assert!(
            linkage_problem("@rpath/libgone.dylib", dir, &libs, &present)
                .is_some_and(|p| p.contains("unresolved"))
        );
        assert!(
            linkage_problem("/nope/libgone.dylib", dir, &libs, &present)
                .is_some_and(|p| p.contains("missing"))
        );
        assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]));
        assert!(!is_macho(b"#!/bin/sh"));
        assert!(
            !is_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]),
            "a Java class file"
        );

        use crate::macho::fixtures::{
            ID_DYLIB, LOAD_DYLIB, archive, dylib_command, dylib_command32, fat, thin, thin32,
        };
        let files = aterm_tempfile::tempdir().expect("owned keg directory");
        let tool = thin(
            &[
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", true),
                dylib_command(LOAD_DYLIB, "@rpath/libfoo.dylib", true),
            ],
            true,
        );
        std::fs::write(files.path().join("tool"), &tool).unwrap();
        assert_eq!(
            linked_dylibs(&files.path().join("tool")).unwrap(),
            ["/usr/lib/libSystem.B.dylib", "@rpath/libfoo.dylib"]
        );
        let lib = thin(
            &[
                dylib_command(ID_DYLIB, "@rpath/libfoo.dylib", false),
                dylib_command(LOAD_DYLIB, "/usr/lib/libSystem.B.dylib", false),
            ],
            false,
        );
        std::fs::write(files.path().join("libfoo"), fat(&[lib.clone(), lib], true)).unwrap();
        assert_eq!(
            linked_dylibs(&files.path().join("libfoo")).unwrap(),
            ["@rpath/libfoo.dylib", "/usr/lib/libSystem.B.dylib"]
        );
        let watch = thin32(
            &[dylib_command32(
                LOAD_DYLIB,
                "/usr/lib/libSystem.B.dylib",
                true,
            )],
            true,
        );
        std::fs::write(files.path().join("watch"), &watch).unwrap();
        assert!(is_macho(&watch[..8]));
        assert_eq!(
            linked_dylibs(&files.path().join("watch")).unwrap(),
            ["/usr/lib/libSystem.B.dylib"]
        );
        std::fs::write(
            files.path().join("libx.a"),
            fat(&[archive(), archive()], false),
        )
        .unwrap();
        assert!(
            linked_dylibs(&files.path().join("libx.a"))
                .unwrap()
                .is_empty()
        );
        std::fs::write(files.path().join("static"), thin(&[], true)).unwrap();
        assert!(
            linked_dylibs(&files.path().join("static"))
                .unwrap()
                .is_empty()
        );
        std::fs::write(files.path().join("cut"), &tool[..40]).unwrap();
        assert!(
            linked_dylibs(&files.path().join("cut"))
                .is_err_and(|e| e.starts_with("inspection failed: malformed"))
        );
        assert!(linked_dylibs(&files.path().join("absent")).is_err());

        let scratch = aterm_tempfile::tempdir().expect("owned library directory");
        let library = scratch.path().join("libok.dylib");
        std::fs::write(&library, b"owned library bytes").unwrap();
        assert!(linked_library_exists(&library));
        assert!(!linked_library_exists(scratch.path()));
        assert!(!linked_library_exists(&scratch.path().join("absent.dylib")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = scratch.path().join("libalias.dylib");
            let dangling = scratch.path().join("libgone.dylib");
            symlink("libok.dylib", &alias).unwrap();
            symlink("absent.dylib", &dangling).unwrap();
            assert!(linked_library_exists(&alias));
            assert!(!linked_library_exists(&dangling));
            let owned_libs = [scratch.path().to_path_buf()];
            assert_eq!(
                linkage_problem(
                    "@rpath/libalias.dylib",
                    scratch.path(),
                    &owned_libs,
                    &linked_library_exists
                ),
                None
            );
            assert!(
                linkage_problem(
                    "@rpath/libgone.dylib",
                    scratch.path(),
                    &owned_libs,
                    &linked_library_exists
                )
                .is_some()
            );
        }
    }

    #[test]
    fn unpin_splits_like_brew() {
        assert_eq!(unpin_outcome(true, true, false), UnpinOutcome::Remove);
        assert_eq!(unpin_outcome(true, false, false), UnpinOutcome::Remove);
        assert_eq!(
            unpin_outcome(false, true, false),
            UnpinOutcome::WarnNotPinned
        );
        assert_eq!(
            unpin_outcome(false, false, false),
            UnpinOutcome::ErrorNotInstalled
        );
        assert_eq!(unpin_outcome(false, false, true), UnpinOutcome::FailUnknown);

        let url = "https://formulae.brew.sh/api/formula/pkg404.json";
        for code in [Some(22), Some(56)] {
            let missing = fetch::parse_get_response(
                url,
                code,
                b"\n404".to_vec(),
                b"curl: (56) The requested URL returned error: 404",
            )
            .unwrap_err();
            assert!(missing.is_not_found(), "code={code:?}");
            assert_eq!(
                unpin_outcome(false, false, missing.is_not_found()),
                UnpinOutcome::FailUnknown
            );
        }
        for (code, response) in [
            (Some(6), b"\n000".as_slice()),
            (Some(28), b"\n404".as_slice()),
            (Some(22), b"\n500".as_slice()),
            (None, b"\n404".as_slice()),
            (Some(22), b"\n40".as_slice()),
            (Some(22), b"\n40x".as_slice()),
            (Some(22), b"\n404junk".as_slice()),
            (Some(22), b"\n999".as_slice()),
            (Some(22), b"".as_slice()),
        ] {
            let error = fetch::parse_get_response(
                url,
                code,
                response.to_vec(),
                b"transport failure mentioning 404",
            )
            .unwrap_err();
            assert!(!error.is_not_found(), "{code:?} {response:?}");
            assert_eq!(
                unpin_outcome(false, false, error.is_not_found()),
                UnpinOutcome::ErrorNotInstalled
            );
            assert_eq!(
                error.into_message(),
                format!("abrew: download failed ({url}): transport failure mentioning 404")
            );
        }
        assert_eq!(
            fetch::parse_get_response(url, Some(0), b"body\n404\n200".to_vec(), b"").unwrap(),
            b"body\n404"
        );
        assert_eq!(
            fetch::parse_get_response(url, Some(0), vec![0xff, b'\n', b'0', b'0', b'0'], b"")
                .unwrap(),
            [0xff]
        );
        assert!(fetch::parse_get_response(url, Some(0), b"no status".to_vec(), b"").is_err());
    }

    #[test]
    fn outdated_fails_only_for_named_operands() {
        assert!(!outdated_failed(false, false));
        assert!(!outdated_failed(false, true));
        assert!(!outdated_failed(true, false), "bare census reports, rc 0");
        assert!(outdated_failed(true, true), "named + outdated fails");
    }

    #[test]
    fn cask_shard_keys_fonts_by_the_letter_after_font() {
        assert_eq!(cask_shard("alt-tab"), "a");
        assert_eq!(cask_shard("font-monoid"), "font/font-m");
        assert_eq!(cask_shard("font-iosevka-ss09"), "font/font-i");
        assert_eq!(cask_shard("firefox"), "f");
    }

    #[test]
    #[cfg(unix)]
    fn long_listing_renders_modes_dates_and_link_targets() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("tool");
        std::fs::write(&file, b"12345").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            mode_string(&std::fs::metadata(&file).unwrap()),
            "-rwxr-xr-x"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o4750)).unwrap();
        assert_eq!(
            mode_string(&std::fs::metadata(&file).unwrap()),
            "-rwsr-x---"
        );
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(mode_string(&std::fs::metadata(&dir).unwrap()), "drwxrwxrwt");
        let link = tmp.path().join("link");
        super::super::prefix::symlink_file(&file, &link).unwrap();
        let mut lister = LongLister::new();
        let line = lister.line(&link);
        let link_mode = mode_string(&std::fs::symlink_metadata(&link).unwrap());
        assert!(link_mode.starts_with('l'), "{link_mode}");
        assert!(line.starts_with(&link_mode), "{line}");
        assert!(line.contains(" -> "), "{line}");
        let line = lister.line(&file);
        assert!(line.contains("       5 "), "{line}");
    }

    #[test]
    #[cfg(unix)]
    fn bundle_dump_reserves_a_new_name_without_following_existing_links() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("Brewfile");
        write_bundle_dump(&file, b"first", false).unwrap();
        assert!(write_bundle_dump(&file, b"second", false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"first");
        write_bundle_dump(&file, b"forced", true).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"forced");
        let link = tmp.path().join("link");
        super::super::prefix::symlink_file(&file, &link).unwrap();
        assert!(write_bundle_dump(&link, b"changed", false).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"forced");
        let absent = tmp.path().join("absent");
        let dangling = tmp.path().join("dangling");
        super::super::prefix::symlink_file(&absent, &dangling).unwrap();
        assert!(write_bundle_dump(&dangling, b"created", false).is_err());
        assert!(!absent.exists());
        assert_eq!(std::fs::read_link(&dangling).unwrap(), absent);
    }

    #[test]
    fn on_refuses_an_occupied_marker_before_store_or_stub_changes() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        std::fs::create_dir(prefix.disabled_path()).unwrap();
        std::fs::write(prefix.disabled_path().join("owner"), b"keep").unwrap();
        assert_eq!(cmd_on(&prefix), ExitCode::from(1));
        assert_eq!(
            std::fs::read(prefix.disabled_path().join("owner")).unwrap(),
            b"keep"
        );
        assert!(prefix.is_disabled());
    }

    /// Unix only: a pin is a symlink, which abrew refuses to make elsewhere.
    #[test]
    #[cfg(unix)]
    fn cleanup_preserves_an_older_pin_foreign_versions_and_other_packages() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        for version in ["1", "2"] {
            owned_keg(&prefix, "wget", version);
        }
        prefix.pin("wget").unwrap();
        for version in ["3", "4"] {
            owned_keg(&prefix, "wget", version);
        }
        for version in ["1", "2"] {
            owned_keg(&prefix, "curl", version);
        }
        let foreign = prefix.keg("wget", "999");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("owner"), b"keep").unwrap();
        #[cfg(unix)]
        {
            std::fs::create_dir_all(prefix.bin()).unwrap();
            std::fs::write(prefix.keg("wget", "3").join("tool"), b"x").unwrap();
            std::os::unix::fs::symlink(
                prefix.keg("wget", "3").join("tool"),
                prefix.bin().join("wget-old"),
            )
            .unwrap();
            std::fs::write(prefix.keg("wget", "4").join("tool"), b"current").unwrap();
            std::os::unix::fs::symlink(
                prefix.keg("wget", "3").join("../4/tool"),
                prefix.bin().join("wget-current"),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(prefix.bin().join("wget-current")).unwrap(),
                b"current"
            );
            std::os::unix::fs::symlink(
                "../Cellar/wget/3/tool",
                prefix.bin().join("wget-old-relative"),
            )
            .unwrap();
        }
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), true, &["wget"])
                .unwrap()
                .0,
            2
        );
        assert!(prefix.keg("wget", "1").is_dir());
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), false, &["wget"])
                .unwrap()
                .0,
            2
        );
        for version in ["1", "3"] {
            assert!(!prefix.keg("wget", version).exists());
        }
        for version in ["2", "4"] {
            assert!(prefix.keg("wget", version).is_dir());
        }
        assert_eq!(std::fs::read(foreign.join("owner")).unwrap(), b"keep");
        assert!(prefix.keg("curl", "1").is_dir());
        assert_eq!(prefix.pinned_version("wget").as_deref(), Some("2"));
        #[cfg(unix)]
        {
            assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old")).is_err());
            assert!(std::fs::symlink_metadata(prefix.bin().join("wget-old-relative")).is_err());
            assert_eq!(
                std::fs::read_link(prefix.bin().join("wget-current")).unwrap(),
                prefix.keg("wget", "3").join("../4/tool"),
            );
            assert_eq!(
                std::fs::read(prefix.keg("wget", "4").join("tool")).unwrap(),
                b"current"
            );
        }
    }

    #[test]
    fn info_json_enrichment_reports_installed_state() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let prefix = Prefix::at(temp.path());
        prefix.ensure_api_cache().unwrap();
        let api_dir = prefix.cache().join("api");
        std::fs::write(
            api_dir.join("formula-demo.json"),
            br#"{"name":"demo","versions":{"stable":"2"},"dependencies":["dep"]}"#,
        )
        .unwrap();
        std::fs::write(
            api_dir.join("formula-dep.json"),
            br#"{"name":"dep","versions":{"stable":"3"}}"#,
        )
        .unwrap();
        let keg = prefix.keg("demo", "1");
        std::fs::create_dir_all(&keg).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", "demo"),
                    ("version", "1"),
                    ("installed_on_request", "true"),
                    ("full_name", "demo"),
                ],
            )
            .unwrap();
        owned_keg(&prefix, "dep", "3");
        let api = Api::new(prefix.clone());
        let mut record = aterm_json::json!({"name": "demo"});
        enrich_formula_json(&prefix, &api, &mut record);
        let installed = record
            .get("installed")
            .and_then(aterm_json::Value::as_array)
            .unwrap();
        assert_eq!(installed.len(), 1);
        let row = &installed[0];
        assert_eq!(row.get("version").unwrap(), &aterm_json::json!("1"));
        assert_eq!(
            row.get("installed_on_request").unwrap(),
            &aterm_json::json!(true)
        );
        assert!(row.get("time").is_some_and(|t| !t.is_null()));
        let runtime = row
            .get("runtime_dependencies")
            .and_then(aterm_json::Value::as_array)
            .unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(
            runtime[0].get("full_name").unwrap(),
            &aterm_json::json!("homebrew/core/dep")
        );
        assert_eq!(
            runtime[0].get("declared_directly").unwrap(),
            &aterm_json::json!(true)
        );
        assert!(record.get("linked_keg").is_some_and(|v| v.is_null()));
        assert_eq!(record.get("pinned").unwrap(), &aterm_json::json!(false));
        assert_eq!(record.get("outdated").unwrap(), &aterm_json::json!(true));
        let mut absent = aterm_json::json!({"name": "dep"});
        enrich_formula_json(&prefix, &api, &mut absent);
        assert_eq!(
            absent
                .get("installed")
                .and_then(aterm_json::Value::as_array)
                .unwrap()
                .len(),
            1,
            "dep itself is installed"
        );
    }

    #[test]
    fn disk_usage_readable_matches_brew_units() {
        assert_eq!(disk_usage_readable(0), "0B");
        assert_eq!(disk_usage_readable(999), "999B");
        assert_eq!(disk_usage_readable(1_000), "1KB");
        assert_eq!(disk_usage_readable(1_500), "1.5KB");
        assert_eq!(disk_usage_readable(193_300), "193.3KB");
        assert_eq!(disk_usage_readable(1_000_000), "1MB");
        assert_eq!(disk_usage_readable(1_050_000), "1.1MB");
        assert_eq!(disk_usage_readable(2_500_000_000), "2.5GB");
    }

    #[test]
    fn to_sentence_joins_like_brew() {
        let s = |v: &[&str]| to_sentence(&v.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&[]), "");
        assert_eq!(s(&["a"]), "a");
        assert_eq!(s(&["a", "b"]), "a and b");
        assert_eq!(s(&["a", "b", "c"]), "a, b, and c");
    }

    #[test]
    fn update_report_diffs_indexes_like_brew() {
        use super::super::api::IndexRow;
        let row = |name: &str, version: &str| IndexRow {
            name: name.into(),
            desc: format!("{name} desc"),
            version: version.into(),
            auto_updates: false,
        };
        let inst = |name: &str, version: &str| Installed {
            kind: Kind::Formula,
            name: name.into(),
            version: version.into(),
            versions: vec![version.into()],
        };
        let old = vec![row("gone", "1"), row("kept", "1"), row("old-inst", "1")];
        let new = vec![row("kept", "2"), row("fresh", "1"), row("old-inst", "1")];
        let installed = vec![inst("kept", "1"), inst("gone", "1")];
        let (added, deleted, outdated) = update_report(Some(&old), &new, &installed, true);
        assert_eq!(added, [("fresh".to_string(), "fresh desc".to_string())]);
        assert_eq!(deleted, ["gone"]);
        assert_eq!(outdated, ["kept"]);
        let (added, deleted, outdated) = update_report(None, &new, &installed, true);
        assert!(added.is_empty() && deleted.is_empty());
        assert_eq!(outdated, ["kept"]);
        let (added, deleted, _) = update_report(Some(&old), &new, &installed, false);
        assert!(added.is_empty());
        assert_eq!(deleted, ["gone"]);
        let mut auto = row("kept", "2");
        auto.auto_updates = true;
        let (_, _, outdated) = update_report(None, &[auto], &installed, true);
        assert!(outdated.is_empty());
    }

    #[test]
    fn list_files_excludes_directories_and_ds_store() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().join("keg");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("README"), b"r").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(root.join("bin/tool"), b"t").unwrap();
        std::fs::write(root.join("bin/.DS_Store"), b"junk").unwrap();
        #[cfg(unix)]
        super::super::prefix::symlink_file(&root.join("bin"), &root.join("binlink")).unwrap();
        let mut got: Vec<String> = list_files(&root)
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        got.sort();
        #[cfg(unix)]
        assert_eq!(got, ["README", "bin/tool", "binlink"]);
        #[cfg(not(unix))]
        assert_eq!(
            got,
            [
                "README".to_owned(),
                std::path::Path::new("bin")
                    .join("tool")
                    .display()
                    .to_string()
            ]
        );
    }

    #[test]
    fn artifact_lines_use_brew_summarize_and_english_names() {
        use super::super::formula::Artifact;
        assert_eq!(
            artifact_line(&Artifact::App("AltTab.app".into(), None)).as_deref(),
            Some("AltTab.app (App)")
        );
        assert_eq!(
            artifact_line(&Artifact::App("Foo.app".into(), Some("Bar.app".into()))).as_deref(),
            Some("Foo.app -> Bar.app (App)")
        );
        assert_eq!(
            artifact_line(&Artifact::Suite("SuiteDir".into(), None)).as_deref(),
            Some("SuiteDir (App Suite)")
        );
        assert_eq!(
            artifact_line(&Artifact::PrefPane("X.prefPane".into(), None)).as_deref(),
            Some("X.prefPane (Preference Pane)")
        );
        assert_eq!(
            artifact_line(&Artifact::Generic("f".into(), Some("/Library/X".into()))).as_deref(),
            Some("f -> /Library/X (Generic Artifact)")
        );
        assert_eq!(
            artifact_line(&Artifact::Wrapper(
                "demo".into(),
                "exe".into(),
                vec![],
                None
            ))
            .as_deref(),
            Some("demo (Command Wrapper)")
        );
        assert_eq!(
            artifact_line(&Artifact::Library(
                "P.qlgenerator".into(),
                None,
                "Library/QuickLook".into()
            ))
            .as_deref(),
            Some("P.qlgenerator (Quick Look Plugin)")
        );
        assert_eq!(artifact_line(&Artifact::Other("zap".into())), None);
    }

    #[test]
    fn shell_kind_follows_brew_families() {
        assert_eq!(shell_kind("fish"), ShellKind::Fish);
        assert_eq!(shell_kind("-fish"), ShellKind::Fish);
        assert_eq!(shell_kind("/usr/local/bin/fish"), ShellKind::Fish);
        assert_eq!(shell_kind("tcsh"), ShellKind::Csh);
        assert_eq!(shell_kind("-csh"), ShellKind::Csh);
        assert_eq!(shell_kind("pwsh"), ShellKind::Pwsh);
        assert_eq!(shell_kind("pwsh-preview"), ShellKind::Pwsh);
        assert_eq!(shell_kind("powershell"), ShellKind::Pwsh);
        assert_eq!(shell_kind("zsh"), ShellKind::Posix);
        assert_eq!(shell_kind("bash"), ShellKind::Posix);
        assert_eq!(shell_kind("nushell"), ShellKind::Posix);
        assert_eq!(shell_kind(""), ShellKind::Posix);
    }

    #[cfg(unix)]
    #[test]
    fn shellenv_keeps_prefix_bytes_literal_when_evaluated() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("unexpected");
        let root = temp.path().join(format!(
            "quote'\"$(touch {})`touch {}` path",
            sentinel.display(),
            sentinel.display()
        ));
        let prefix = Prefix::at(&root);
        let script = format!("{}\nprintf '%s' \"$ABREW_PREFIX\"", shellenv(&prefix));
        let output = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, root.as_os_str().as_encoded_bytes());
        assert!(!sentinel.exists());
    }

    #[test]
    fn deletion_requires_complete_dependency_and_request_inputs() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let p = Prefix::at(&tmp.path().join("prefix"));
        p.ensure().unwrap();
        for (name, on_request) in [("app", "true"), ("dep", "false"), ("orphan", "false")] {
            let keg = p.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            std::fs::write(keg.join("payload"), name).unwrap();
            p.write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", "1"),
                    ("installed_on_request", on_request),
                ],
            )
            .unwrap();
        }
        let read = |name: &str| {
            let deps = if name == "app" { vec!["dep"] } else { vec![] };
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": deps}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert_eq!(autoremove_targets(&p, read).unwrap(), vec!["orphan"]);
        let incomplete = |name: &str| {
            if name == "app" {
                Err("captured unknown".into())
            } else {
                read(name)
            }
        };
        assert!(autoremove_targets(&p, incomplete).is_err());
        for value in [
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": "dep"}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": [1]}),
            aterm_json::json!({"name": "wrong", "versions": {"stable": "1"}, "revision": 0, "dependencies": []}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": 0, "dependencies": ["../dep"]}),
            aterm_json::json!({"name": "app", "versions": {"stable": ""}, "revision": 0, "dependencies": []}),
            aterm_json::json!({"name": "app", "versions": {"stable": "1"}, "revision": "unknown", "dependencies": []}),
        ] {
            assert!(
                super::super::api::dependency_formula(
                    &value,
                    "app",
                    super::super::machine::Machine::this()
                )
                .is_err()
            );
        }
        let installed = p.installed_checked(Kind::Formula).unwrap();
        let revised = |name: &str| {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 2, "dependencies": []}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert!(
            install::dependency_books(&installed, revised).is_err(),
            "unqualified base does not prove revision2"
        );
        let mut qualified = installed.clone();
        for i in &mut qualified {
            i.version = "1_2".into();
            i.versions = vec!["1_2".into()];
        }
        assert!(
            install::dependency_books(&qualified, revised).is_ok(),
            "actual revision suffix is matched"
        );
        let receipt = p.keg("app", "1").join("INSTALL_RECEIPT.json");
        let saved = std::fs::read(&receipt).unwrap();
        std::fs::remove_file(&receipt).unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(
            &receipt,
            br#"{"installed_by":"abrew","name":"app","version":"1","installed_on_request":null}"#,
        )
        .unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(&receipt, vec![b' '; MAX_REQUEST_RECEIPT_BYTES + 1]).unwrap();
        assert!(autoremove_targets(&p, read).is_err());
        std::fs::write(&receipt, saved).unwrap();
        let old = p.keg("orphan", "0");
        std::fs::create_dir_all(&old).unwrap();
        p.write_receipt(
            &old,
            &[
                ("installed_by", "abrew"),
                ("name", "orphan"),
                ("version", "0"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
        let older = p
            .installed_checked(Kind::Formula)
            .unwrap()
            .into_iter()
            .find(|i| i.name == "orphan")
            .unwrap();
        assert!(requested(&p, &older).unwrap(), "old request is retained");
        assert!(
            autoremove_targets(&p, read).is_err(),
            "latest metadata cannot cover the older version"
        );
        std::fs::remove_dir_all(&old).unwrap();
        let missing = |name: &str| {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 0, "dependencies": ["absent"]}),
                name,
                super::super::machine::Machine::this(),
            )
        };
        assert!(autoremove_targets(&p, missing).is_err());
        #[cfg(unix)]
        {
            p.pin("orphan").unwrap();
            assert!(autoremove_targets(&p, read).unwrap().is_empty());
            std::fs::remove_file(p.root.join("Pinned/orphan")).unwrap();
            std::fs::remove_dir(p.root.join("Pinned")).unwrap();
            std::fs::write(p.root.join("Pinned"), b"not a directory").unwrap();
            assert!(autoremove_targets(&p, read).is_err());
            std::fs::remove_file(p.root.join("Pinned")).unwrap();
            let pins_outside = tmp.path().join("pins-outside");
            std::fs::create_dir(&pins_outside).unwrap();
            std::os::unix::fs::symlink(&pins_outside, p.root.join("Pinned")).unwrap();
            assert!(autoremove_targets(&p, read).is_err());
            assert!(std::fs::read_dir(&pins_outside).unwrap().next().is_none());
            std::fs::remove_file(p.root.join("Pinned")).unwrap();
            let outside = tmp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("marker"), b"retain outside").unwrap();
            std::os::unix::fs::symlink(&outside, p.cellar().join("alias")).unwrap();
            assert!(p.installed_checked(Kind::Formula).is_err());
            assert_eq!(
                p.installed(Kind::Formula).len(),
                3,
                "legacy read-only best effort"
            );
            assert!(autoremove_targets(&p, read).is_err());
            assert_eq!(
                std::fs::read(outside.join("marker")).unwrap(),
                b"retain outside"
            );
            std::fs::remove_file(p.cellar().join("alias")).unwrap();
        }
        assert_eq!(autoremove_targets(&p, read).unwrap(), vec!["orphan"]);
        for name in ["adep", "zapp"] {
            let keg = p.keg(name, "1");
            std::fs::create_dir_all(&keg).unwrap();
            std::fs::write(keg.join("payload"), name).unwrap();
            p.write_receipt(
                &keg,
                &[
                    ("installed_by", "abrew"),
                    ("name", name),
                    ("version", "1"),
                    ("installed_on_request", "false"),
                ],
            )
            .unwrap();
        }
        let chain = |name: &str| {
            if name == "zapp" {
                super::super::api::dependency_formula(
                    &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["adep"]}),
                    name,
                    super::super::machine::Machine::this(),
                )
            } else {
                read(name)
            }
        };
        assert_eq!(
            autoremove_targets(&p, chain).unwrap(),
            ["orphan", "zapp", "adep"]
        );
        let cycle = |name: &str| {
            if name == "adep" {
                super::super::api::dependency_formula(
                    &aterm_json::json!({"name":name,"versions":{"stable":"1"},"revision":0,"dependencies":["zapp"]}),
                    name,
                    super::super::machine::Machine::this(),
                )
            } else {
                chain(name)
            }
        };
        assert!(autoremove_targets(&p, cycle).is_err());
        let unknown = p.keg("notes", "1");
        std::fs::create_dir_all(&unknown).unwrap();
        std::fs::write(unknown.join("owner"), b"keep").unwrap();
        assert!(autoremove_targets(&p, chain).is_err());
        assert_eq!(std::fs::read(unknown.join("owner")).unwrap(), b"keep");
        for name in ["app", "dep", "orphan", "adep", "zapp"] {
            assert_eq!(
                std::fs::read(p.keg(name, "1").join("payload")).unwrap(),
                name.as_bytes()
            );
        }
    }

    #[test]
    fn caffiene_alias_works_for_list_outdated_and_uninstall() {
        let dir = crate::testdir::dir("abrew-caffiene-lifecycle-");
        let p = Prefix::at(&dir.join("prefix"));
        p.ensure().unwrap();
        let cask_dir = p.cask_dir("caffeine", "1.1.3");
        std::fs::create_dir_all(&cask_dir).unwrap();
        std::fs::write(cask_dir.join("Caffeine.app"), b"stub").unwrap();
        p.write_receipt(
            &cask_dir,
            &[
                ("installed_by", "abrew"),
                ("token", "caffeine"),
                ("version", "1.1.3"),
                ("installed_on_request", "true"),
            ],
        )
        .unwrap();
        p.rebuild_state().unwrap();

        assert_eq!(cmd_list(&p, &[String::from("caffiene")]), ExitCode::SUCCESS);
        assert_eq!(
            cmd_list(&p, &[String::from("--versions"), String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        let api = Api::read_only(p.clone());
        assert!(!package_definitively_unknown(
            &p, &api, "caffiene", false, true
        ));
        assert_eq!(
            cmd_unpin(&p, &api, &[String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            bundle_file(
                &p,
                &api,
                &[String::from("bundle"), String::from("check")],
                &[bundle::BrewfileEntry::Cask(String::from("caffiene"))],
                "check"
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            cmd_cleanup(&p, &api, &[String::from("caffiene")]),
            ExitCode::SUCCESS
        );
        assert_eq!(
            cmd_uninstall(
                &p,
                &api,
                &[String::from("uninstall"), String::from("caffiene")],
                &[String::from("caffiene")]
            ),
            ExitCode::SUCCESS
        );
        assert!(p.installed_one(Kind::Cask, "caffeine").is_none());
    }
}
