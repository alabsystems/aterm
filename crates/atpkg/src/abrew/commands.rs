// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The `cmd_*` bodies behind `mod.rs`' dispatch.

use std::cmp::Ordering;
use std::process::ExitCode;

use super::api::Api;
use super::formula::{Formula, version_cmp};
use super::install::{self, InstallOpts};
use super::prefix::{Kind, Prefix};
use super::{bundle, cask, fetch, from_install, has_flag, not_native, operands, stub, tap};

fn failed(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("abrew: {msg}");
    ExitCode::from(2)
}

// ── abrew's own switches ────────────────────────────────────────────────

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
        println!(
            "abrew: on — `brew` in aterm sessions runs abrew (`brew --real …` for real Homebrew)"
        );
    } else {
        println!("abrew: per-prefix on; [abrew] enabled=false still selects real Homebrew");
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
    if let Some(layout) = crate::store::resolve_from(crate::config::cached()) {
        stub::remove(&layout);
    }
    println!("abrew: off — `brew` runs real Homebrew until `abrew on`");
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
    match stub::resolve_real_brew(prefix) {
        Some(p) => println!("real brew: {}", p.display()),
        None => println!("real brew: none found (abrew bundle lays one)"),
    }
    bundle::bundle_status(prefix);
    println!(
        "installed: {} formulae, {} casks",
        prefix.installed(Kind::Formula).len(),
        prefix.installed(Kind::Cask).len()
    );
    ExitCode::SUCCESS
}

// ── discovery ───────────────────────────────────────────────────────────

pub(super) fn cmd_search(api: &Api, rest: &[String]) -> ExitCode {
    let query = operands(rest).join(" ").to_ascii_lowercase();
    if query.is_empty() {
        return usage("search needs a term");
    }
    let desc = has_flag(rest, "--desc", "");
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
                    if name.to_ascii_lowercase().contains(&query)
                        || (desc && d.to_ascii_lowercase().contains(&query))
                    {
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
                }
            }
            Err(e) => return failed(&e),
        }
    }
    if shown == 0 {
        println!("No formulae or casks matching \"{query}\".");
    }
    ExitCode::SUCCESS
}

fn print_formula_info(prefix: &Prefix, f: &Formula) {
    println!("==> {}: stable {}", f.full_name, f.version);
    if !f.desc.is_empty() {
        println!("{}", f.desc);
    }
    if !f.homepage.is_empty() {
        println!("{}", f.homepage);
    }
    if !f.license.is_empty() {
        println!("License: {}", f.license);
    }
    match prefix.installed_one(Kind::Formula, &f.name) {
        Some(i) => println!(
            "Installed: {} ({}){}",
            i.version,
            i.versions.join(", "),
            if prefix.is_pinned(&f.name) {
                " [pinned]"
            } else {
                ""
            }
        ),
        None => println!("Not installed"),
    }
    if !f.dependencies.is_empty() {
        println!("Dependencies: {}", f.dependencies.join(", "));
    }
    if f.keg_only {
        println!("keg-only (not linked)");
    }
    if f.deprecate {
        println!("Deprecated upstream");
    }
    if f.disable {
        println!("Disabled upstream");
    }
    if !f.caveats.is_empty() {
        println!("Caveats: {}", f.caveats.trim());
    }
}

pub(super) fn cmd_info(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        println!(
            "{} formulae, {} casks installed under {}",
            prefix.installed(Kind::Formula).len(),
            prefix.installed(Kind::Cask).len(),
            prefix.root.display()
        );
        return ExitCode::SUCCESS;
    }
    for name in &names {
        if has_flag(rest, "--cask", "") {
            match api.cask(name) {
                Ok(c) => {
                    println!("==> {}: {}", c.token, c.version);
                    if !c.name.is_empty() {
                        println!("{}", c.name);
                    }
                    if !c.desc.is_empty() {
                        println!("{}", c.desc);
                    }
                    println!("{}", c.homepage);
                    match prefix.installed_one(Kind::Cask, &c.token) {
                        Some(i) => println!("Installed: {}", i.version),
                        None => println!("Not installed"),
                    }
                }
                Err(e) => return failed(&e),
            }
            continue;
        }
        match api.formula(name) {
            Ok(f) => print_formula_info(prefix, &f),
            Err(_) => match tap::lookup(prefix, name) {
                Some(f) => print_formula_info(prefix, &f),
                None => return not_native(prefix, args, "info (unresolved name)"),
            },
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_desc(api: &Api, rest: &[String]) -> ExitCode {
    for name in operands(rest) {
        match api.formula(&name) {
            Ok(f) => println!("{}: {}", f.name, f.desc),
            Err(_) => match api.cask(&name) {
                Ok(c) => println!("{}: {}", c.token, c.desc),
                Err(e) => return failed(&e),
            },
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_home(api: &Api, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        println!("https://brew.sh");
        return ExitCode::SUCCESS;
    }
    for name in names {
        let url = api
            .formula(&name)
            .map(|f| f.homepage)
            .or_else(|_| api.cask(&name).map(|c| c.homepage));
        match url {
            Ok(u) if !u.is_empty() => println!("{u}"),
            _ => return failed(&format!("abrew: no homepage for {name}")),
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_cat(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let Some(name) = operands(rest).into_iter().next() else {
        return usage("cat needs a formula name");
    };
    // A tap formula's file, verbatim; a core formula's API record.
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
        short.chars().next().unwrap_or('x')
    );
    match fetch::curl_get(&url, &[]) {
        Ok(bytes) => {
            print!("{}", String::from_utf8_lossy(&bytes));
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
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
    match stub::resolve_real_brew(prefix) {
        Some(p) => println!("REAL_BREW: {}", p.display()),
        None => println!("REAL_BREW: none"),
    }
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

pub(super) fn cmd_shellenv(prefix: &Prefix) -> ExitCode {
    print!("{}", shellenv(prefix));
    ExitCode::SUCCESS
}

pub(super) fn cmd_doctor(prefix: &Prefix) -> ExitCode {
    let mut problems = 0usize;
    let mut note = |m: String| {
        println!("Warning: {m}");
        problems += 1;
    };
    if prefix.ensure().is_err() {
        note(format!(
            "prefix {} cannot be created/written",
            prefix.root.display()
        ));
    }
    if stub::resolve_real_brew(prefix).is_none() {
        note("no real Homebrew found; passthrough commands will fail (run: abrew bundle)".into());
    }
    // Broken links in bin/.
    for sub in ["bin", "sbin", "lib"] {
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
    }
    let path_has_bin = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d == prefix.bin()));
    if !prefix.installed(Kind::Formula).is_empty() && !path_has_bin {
        note(format!(
            "{} is not on PATH (eval \"$(abrew shellenv)\")",
            prefix.bin().display()
        ));
    }
    let _ = prefix.rebuild_state();
    if problems == 0 {
        println!("Your abrew is ready to brew.");
        ExitCode::SUCCESS
    } else {
        println!("Please note that these warnings are just used to help abrew maintainers.");
        ExitCode::from(1)
    }
}

// ── lifecycle ───────────────────────────────────────────────────────────

fn install_opts(rest: &[String]) -> InstallOpts {
    InstallOpts {
        ignore_dependencies: has_flag(rest, "--ignore-dependencies", ""),
        dry_run: has_flag(rest, "--dry-run", "-n"),
        force: has_flag(rest, "--force", "-f"),
        quiet: has_flag(rest, "--quiet", "-q"),
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
        if !want_cask && !want_formula {
            // Resolve the kind before installation. A failed formula install
            // must not turn into a cask install after changing the prefix.
            if api.formula(name).is_ok() {
                if let Err(error) = install::install(api, prefix, name, &opts) {
                    return from_install(prefix, args, Err(error));
                }
            } else {
                match api.cask(name) {
                    Ok(c) => {
                        if let Err(error) = cask::install(prefix, &c, opts.dry_run, opts.force) {
                            return from_install(prefix, args, Err(error));
                        }
                    }
                    Err(error) => return failed(&error),
                }
            }
        } else if want_cask {
            match api.cask(name) {
                Ok(c) => {
                    let r = cask::install(prefix, &c, opts.dry_run, opts.force).map(|_| ());
                    if let Err(e) = r {
                        return from_install(prefix, args, Err(e));
                    }
                }
                Err(e) => return failed(&e),
            }
        } else {
            match install::install(api, prefix, name, &opts) {
                Ok(_) => {}
                Err(e) => return from_install(prefix, args, Err(e)),
            }
        }
    }
    ExitCode::SUCCESS
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
    for name in &names {
        let short = Formula::short_name(name);
        if !has_flag(rest, "--cask", "")
            && !has_flag(rest, "--formula", "")
            && prefix.installed_one(Kind::Formula, short).is_some()
            && prefix.installed_one(Kind::Cask, short).is_some()
        {
            return failed(&format!(
                "abrew: {short} is installed as both a formula and a cask; say --formula or --cask"
            ));
        }
        if has_flag(rest, "--cask", "")
            || (!has_flag(rest, "--formula", "")
                && prefix.installed_one(Kind::Formula, short).is_none()
                && prefix.installed_one(Kind::Cask, short).is_some())
        {
            let r = cask::uninstall(prefix, short, dry);
            if let Err(e) = r {
                return from_install(prefix, args, Err(e));
            }
            continue;
        }
        let r = install::uninstall(api, prefix, name, ignore, dry);
        if let Err(e) = r {
            return from_install(prefix, args, Err(e));
        }
        if !dry {
            println!("Uninstalling {short}…");
        }
    }
    ExitCode::SUCCESS
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
        ..install_opts(rest)
    };
    for name in &names {
        let short = Formula::short_name(name).to_string();
        match install::install(api, prefix, &short, &opts) {
            Ok(_) => {}
            Err(e) => return from_install(prefix, args, Err(e)),
        }
    }
    ExitCode::SUCCESS
}

/// (name, installed, latest, is_cask) rows that are outdated.
fn outdated_rows(
    prefix: &Prefix,
    api: &Api,
    casks: bool,
) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    if casks {
        for i in prefix.installed(Kind::Cask) {
            let c = api.cask(&i.name)?;
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

pub(super) fn cmd_outdated(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let mut any = false;
    for casks in [false, true] {
        if has_flag(rest, "--cask", "") && !casks {
            continue;
        }
        if has_flag(rest, "--formula", "") && casks {
            continue;
        }
        let rows = match outdated_rows(prefix, api, casks) {
            Ok(rows) => rows,
            Err(error) => return failed(&error),
        };
        for (name, have, latest) in rows {
            let pin = if !casks && prefix.is_pinned(&name) {
                " [pinned]"
            } else {
                ""
            };
            println!("{name} ({have}) < {latest}{pin}");
            any = true;
        }
    }
    if any {
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
    let opts = install_opts(rest);
    let names = operands(rest);
    let Some(name) = names.first() else {
        return usage("upgrade needs a formula name");
    };
    let Some(before) = prefix.installed_one(Kind::Formula, Formula::short_name(name)) else {
        return failed(&format!(
            "abrew: {} is not installed (upgrade does not install)",
            Formula::short_name(name)
        ));
    };
    match install::install(api, prefix, name, &opts) {
        Ok(installed_now) if !opts.dry_run && installed_now != before.version => {
            match cleanup_old_versions(prefix, api, false, Some(name)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => failed(&error),
            }
        }
        result => from_install(prefix, args, result.map(|_| ())),
    }
}

pub(super) fn cmd_update(prefix: &Prefix, api: &Api) -> ExitCode {
    for t in tap::list(prefix) {
        if let Err(error) = tap::update(prefix, &t) {
            return failed(&error);
        }
    }
    // Stage: fetch both indexes fresh first; only then invalidate the
    // per-package cache. A network failure above leaves the working
    // cache exactly as it was.
    let staged = (api.index_fresh(false), api.index_fresh(true));
    if let (Ok(_), Ok(_)) = &staged {
        api.refresh_except(&["formula-index.json", "cask-index.json"]);
    }
    match staged {
        (Ok(f), Ok(c)) => {
            println!(
                "==> abrew data updated: {} formulae, {} casks",
                f.len(),
                c.len()
            );
            ExitCode::SUCCESS
        }
        (Err(e), _) | (_, Err(e)) => failed(&e),
    }
}

/// Remove all but the newest version of each keg (a pin keeps its
/// pinned version too). Returns rows removed.
/// Unlink prefix-level symlinks (bin/lib/share/… and cask Applications)
/// whose target lies under `dir`. Bounded walk; only symlinks are removed.
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
                if let Ok(target) = std::fs::read_link(&path) {
                    let resolved = if target.is_absolute() {
                        target
                    } else {
                        root.join(target)
                    };
                    if resolved.starts_with(dir) {
                        let _ = std::fs::remove_file(&path);
                    }
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
    ] {
        walk(&prefix.root.join(name), dir, 8);
    }
}

fn cleanup_old_versions(
    prefix: &Prefix,
    api: &Api,
    dry: bool,
    named: Option<&str>,
) -> Result<usize, String> {
    let mut removed = 0;
    for kind in [Kind::Formula, Kind::Cask] {
        for i in prefix.installed(kind) {
            // Only superseded versions are removed below; the current
            // (opt-active) version dependents resolve through stays.
            let _ = api;
            let active = prefix
                .opt(&i.name)
                .read_link()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            if named.is_some_and(|name| Formula::short_name(name) != i.name) {
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
                    // Only an abrew-owned receipt authorizes deleting
                    // this version; foreign or receiptless rows stay.
                    if !super::prefix::owned_receipt(&dir, kind, &i.name, v) {
                        continue;
                    }
                    if dry {
                        println!("Would remove {} ({v}) {}", i.name, dir.display());
                    } else {
                        // Drop prefix-level links into the version being
                        // removed so cleanup never leaves dangling links.
                        remove_links_into(prefix, &dir);
                        std::fs::remove_dir_all(&dir)
                            .map_err(|e| format!("abrew: cannot remove {}: {e}", dir.display()))?;
                        println!("Removing {} ({v})", i.name);
                    }
                    removed += 1;
                }
            }
        }
    }
    Ok(removed)
}

pub(super) fn cmd_cleanup(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let dry = has_flag(rest, "--dry-run", "-n");
    let named = operands(rest);
    match cleanup_old_versions(prefix, api, dry, named.first().map(String::as_str)) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => failed(&error),
    }
}

const MAX_REQUEST_RECEIPT_BYTES: usize = 64 * 1024;

/// Every standing version must say whether it was requested. An old requested
/// version still protects its dependencies; missing/unknown receipts preserve all.
fn requested(prefix: &Prefix, installed: &super::prefix::Installed) -> Result<bool, String> {
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
    // Admit every input before the first removal; never turn a partial book into
    // an orphan decision. A later removal failure is an error, not a rollback.
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
            // Recheck ordinary dependency protection. Automatic pruning never
            // supplies the user's explicit --ignore-dependencies optout.
            if let Err(e) = install::uninstall(api, prefix, &name, false, false) {
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
    for name in names {
        let short = Formula::short_name(&name);
        if !link {
            match install::unlink_keg(prefix, short) {
                Ok(()) => println!("Unlinking {short}…"),
                Err(e) => return failed(&e),
            }
            continue;
        }
        let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
            return failed(&format!("abrew: {short} is not installed"));
        };
        // Link goes through the same activation as install: keg-only
        // refreshes opt only, and opt is restored either way.
        let f = match api.formula(short) {
            Ok(f) => f,
            Err(e) => return failed(&e),
        };
        if f.keg_only && !has_flag(rest, "--force", "-f") {
            println!(
                "{short} is keg-only; refreshing opt only (link --force overrides in real brew)"
            );
        }
        if let Err(e) = install::relink(prefix, &f) {
            return from_install(prefix, args, Err(e));
        }
        println!("Linking {} {}…", short, inst.version);
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_switch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    let (Some(name), Some(version)) = (ops.first(), ops.get(1)) else {
        return usage("switch needs <formula> <version>");
    };
    let short = Formula::short_name(name);
    if !super::valid_component(short) || !super::valid_component(version) {
        return failed("abrew: invalid formula name or version");
    }
    // Unknown keg-only status must fail closed, not default to linkable.
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

pub(super) fn cmd_list(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let versions = has_flag(rest, "--versions", "");
    let named = operands(rest);
    if let Some(name) = named.first() {
        // `list <formula>`: the keg's files, brew-style.
        let short = Formula::short_name(name);
        let kind = if has_flag(rest, "--cask", "")
            || (!has_flag(rest, "--formula", "")
                && prefix.installed_one(Kind::Formula, short).is_none()
                && prefix.installed_one(Kind::Cask, short).is_some())
        {
            Kind::Cask
        } else {
            Kind::Formula
        };
        let Some(inst) = prefix.installed_one(kind, short) else {
            return failed(&format!("abrew: {short} is not installed"));
        };
        let dir = match kind {
            Kind::Formula => prefix.keg(short, &inst.version),
            Kind::Cask => prefix.cask_dir(short, &inst.version),
        };
        fn walk(d: &std::path::Path) {
            if let Ok(entries) = std::fs::read_dir(d) {
                for e in entries.flatten() {
                    println!("{}", e.path().display());
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        walk(&e.path());
                    }
                }
            }
        }
        walk(&dir);
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
        for i in prefix.installed(kind) {
            if has_flag(rest, "--pinned", "") && (casks || !prefix.is_pinned(&i.name)) {
                continue;
            }
            if versions {
                println!("{} {}", i.name, i.versions.join(" "));
            } else {
                println!("{}", i.name);
            }
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_fetch(prefix: &Prefix, api: &Api, args: &[String], rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("fetch needs a name");
    }
    for name in names {
        if has_flag(rest, "--cask", "") {
            match api.cask(&name) {
                Ok(c) => {
                    if !super::valid_component(&c.token) || !super::valid_component(&c.version) {
                        return failed("abrew: unsupported cask token/version");
                    }
                    if c.disabled {
                        return failed(&format!("abrew: cask {} is disabled upstream", c.token));
                    }
                    let dest = prefix
                        .cache()
                        .join(format!("{}-{}-fetch", c.token, c.version));
                    let expected = c.sha256.as_deref().unwrap_or("no_check");
                    let reusable =
                        c.sha256.is_some() && fetch::verify_sha256(&dest, expected).is_ok();
                    if !reusable && let Err(e) = fetch::curl_to_file(&c.url, &dest, &[]) {
                        return failed(&e);
                    }
                    if let Err(e) = fetch::verify_sha256(&dest, expected) {
                        return failed(&e);
                    }
                    println!("Downloaded to {}", dest.display());
                }
                Err(e) => return failed(&e),
            }
            continue;
        }
        match api.formula(&name) {
            Ok(f) => {
                let r = install::fetch_only(prefix, &f);
                if let Err(e) = r {
                    return from_install(prefix, args, Err(e));
                }
            }
            Err(e) => return failed(&e),
        }
    }
    ExitCode::SUCCESS
}

// ── graphs ──────────────────────────────────────────────────────────────

pub(super) fn cmd_deps(prefix: &Prefix, api: &Api, rest: &[String]) -> ExitCode {
    let tree = has_flag(rest, "--tree", "");
    if has_flag(rest, "--installed", "") {
        for i in prefix.installed(Kind::Formula) {
            if let Ok(f) = api.formula(&i.name) {
                println!("{}: {}", f.name, f.dependencies.join(" "));
            }
        }
        return ExitCode::SUCCESS;
    }
    let names = operands(rest);
    if names.is_empty() {
        return usage("deps needs a formula name (or --installed)");
    }
    for name in names {
        let f = match api.formula(&name).or_else(|_| {
            tap::lookup(prefix, &name).ok_or_else(|| format!("abrew: no formula {name}"))
        }) {
            Ok(f) => f,
            Err(e) => return failed(&e),
        };
        if tree {
            println!("{}", f.name);
            print_dep_tree(api, &f, 1, &mut Vec::new());
        } else if has_flag(rest, "--direct", "") {
            for d in &f.dependencies {
                println!("{d}");
            }
        } else {
            // Transitive closure, sorted, one per line (brew's default).
            let mut seen: std::collections::BTreeSet<String> = Default::default();
            let mut stack: Vec<String> = f.dependencies.clone();
            while let Some(d) = stack.pop() {
                let short = Formula::short_name(&d).to_string();
                if seen.insert(short.clone())
                    && let Ok(df) = api.formula(&short)
                {
                    stack.extend(df.dependencies.iter().cloned());
                }
            }
            for d in seen {
                println!("{d}");
            }
        }
    }
    ExitCode::SUCCESS
}

fn print_dep_tree(api: &Api, f: &Formula, depth: usize, seen: &mut Vec<String>) {
    if depth > 12 {
        return;
    }
    for (i, d) in f.dependencies.iter().enumerate() {
        let last = i + 1 == f.dependencies.len();
        let branch = if last { "└── " } else { "├── " };
        println!("{}{branch}{d}", "    ".repeat(depth - 1));
        let short = Formula::short_name(d).to_string();
        if !seen.contains(&short) {
            seen.push(short.clone());
            if let Ok(df) = api.formula(&short) {
                print_dep_tree(api, &df, depth + 1, seen);
            }
        }
    }
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

pub(super) fn cmd_leaves(prefix: &Prefix, api: &Api) -> ExitCode {
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
        if !needed.contains(&i.name) {
            println!("{}", i.name);
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_missing(prefix: &Prefix, api: &Api) -> ExitCode {
    let mut any = false;
    for i in prefix.installed(Kind::Formula) {
        let f = match api.formula(&i.name) {
            Ok(f) => f,
            Err(error) => return failed(&error),
        };
        for d in &f.dependencies {
            let short = Formula::short_name(d);
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

pub(super) fn cmd_options(api: &Api, rest: &[String]) -> ExitCode {
    for name in operands(rest) {
        match api.formula(&name) {
            Ok(f) => {
                println!("{} options:", f.name);
                println!(
                    "  (source builds run real Homebrew: brew --real install -s {})",
                    f.name
                );
                if !f.build_dependencies.is_empty() {
                    println!("  build deps: {}", f.build_dependencies.join(", "));
                }
            }
            Err(e) => return failed(&e),
        }
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_linkage(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    if names.is_empty() {
        return usage("linkage needs a formula name");
    }
    for name in names {
        let short = Formula::short_name(&name);
        let Some(inst) = prefix.installed_one(Kind::Formula, short) else {
            return failed(&format!("abrew: {short} is not installed"));
        };
        let keg = prefix.keg(short, &inst.version);
        if !cfg!(target_os = "macos") {
            println!("{short}: linkage inspection is macOS (otool) only in v1");
            continue;
        }
        for sub in ["bin", "lib"] {
            let dir = keg.join(sub);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_symlink()) {
                        continue;
                    }
                    let p = e.path();
                    if let Ok(out) = fetch::run_capture("otool", &["-L", &p.to_string_lossy()]) {
                        let text = String::from_utf8_lossy(&out.stdout);
                        for line in text.lines().skip(1) {
                            let line = line.trim();
                            if line.contains("/opt/homebrew") || line.contains("@@HOMEBREW") {
                                println!("{}: UNRELOCATED {line}", p.display());
                            }
                        }
                    }
                }
            }
        }
        println!("{short}: linkage checked (unrelocated rows above, if any)");
    }
    ExitCode::SUCCESS
}

pub(super) fn cmd_log(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let names = operands(rest);
    let Some(name) = names.first() else {
        // No name: the bundle/tap git history, briefly.
        for t in tap::list(prefix) {
            println!("==> tap {}", t.name());
            let _ = fetch::run_inherit(
                "git",
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
            println!(
                "{short}: no abrew install record (build log: {})",
                prefix
                    .cache()
                    .join("logs")
                    .join(format!("{short}.log"))
                    .display()
            );
            ExitCode::SUCCESS
        }
    }
}

// ── taps, services, bundle ──────────────────────────────────────────────

pub(super) fn cmd_tap(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    if ops.is_empty() {
        for t in tap::list(prefix) {
            println!("{}", t.name());
        }
        return ExitCode::SUCCESS;
    }
    let url = ops.get(1).map(String::as_str);
    match tap::add(prefix, ops.first().map_or("", String::as_str), url) {
        Ok(t) => {
            println!("Tapped {} ({})", t.name(), t.path.display());
            ExitCode::SUCCESS
        }
        Err(e) => failed(&e),
    }
}

pub(super) fn cmd_tap_info(prefix: &Prefix, rest: &[String]) -> ExitCode {
    let ops = operands(rest);
    let taps = tap::list(prefix);
    let show: Vec<&tap::Tap> = if ops.is_empty() {
        taps.iter().collect()
    } else {
        taps.iter()
            .filter(|t| ops.iter().any(|o| o == &t.name()))
            .collect()
    };
    for t in show {
        let mut formulas = 0usize;
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
        count_rb(&t.path.join("Formula"), &mut formulas);
        println!("{}: {} formula(e)", t.name(), formulas);
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
    let sub = rest.first().map(String::as_str).unwrap_or("");
    match sub {
        "status" => {
            bundle::bundle_status(prefix);
            ExitCode::SUCCESS
        }
        "remove" => from_install(prefix, args, bundle::bundle_remove(prefix)),
        "dump" => {
            print!("{}", bundle::brewfile_dump(prefix));
            ExitCode::SUCCESS
        }
        "install" | "check" => match prepared {
            Some(entries) => bundle_file(prefix, api, args, &entries, sub),
            None => failed("abrew: Brewfile was not admitted"),
        },
        _ => {
            let mut version: Option<&str> = None;
            let mut sha: Option<&str> = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--version" => version = it.next().map(String::as_str),
                    "--sha256" => sha = it.next().map(String::as_str),
                    _ => {}
                }
            }
            let force = has_flag(rest, "--force", "-f");
            from_install(
                prefix,
                args,
                bundle::install_bundle(prefix, version, sha, force),
            )
        }
    }
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
    for entry in entries {
        match entry {
            bundle::BrewfileEntry::Tap(n) => {
                if dry {
                    let have = tap::list(prefix).iter().any(|t| t.name() == *n);
                    missing |= !have;
                    println!("tap {n}: {}", if have { "present" } else { "missing" });
                } else if let Err(e) = tap::add(prefix, n, None) {
                    return failed(&e);
                }
            }
            bundle::BrewfileEntry::Brew(n) => {
                let have = prefix
                    .installed_one(Kind::Formula, Formula::short_name(n))
                    .is_some();
                if dry {
                    missing |= !have;
                    println!("brew {n}: {}", if have { "present" } else { "missing" });
                } else if !have {
                    let opts = InstallOpts::default();
                    if let Err(e) = install::install(api, prefix, n, &opts) {
                        return from_install(prefix, args, Err(e));
                    }
                }
            }
            bundle::BrewfileEntry::Cask(n) => {
                let have = prefix.installed_one(Kind::Cask, n).is_some();
                if dry {
                    missing |= !have;
                    println!("cask {n}: {}", if have { "present" } else { "missing" });
                } else if !have {
                    match api.cask(n) {
                        Ok(c) => {
                            if let Err(e) = cask::install(prefix, &c, false, false) {
                                return from_install(prefix, args, Err(e));
                            }
                        }
                        Err(e) => return failed(&e),
                    }
                }
            }
        }
    }
    if missing {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
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
        // A prefix-level link into a superseded version must not dangle.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(prefix.bin()).unwrap();
            std::fs::write(prefix.keg("wget", "3").join("tool"), b"x").unwrap();
            std::os::unix::fs::symlink(
                prefix.keg("wget", "3").join("tool"),
                prefix.bin().join("wget-old"),
            )
            .unwrap();
        }
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), true, Some("wget")).unwrap(),
            2
        );
        assert!(prefix.keg("wget", "1").is_dir());
        assert_eq!(
            cleanup_old_versions(&prefix, &Api::new(prefix.clone()), false, Some("wget")).unwrap(),
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
        assert!(!prefix.bin().join("wget-old").exists());
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

    /// The actual automatic-removal planner, complete census, receipt reader and
    /// metadata parser refuse unknowns before any deletion. Known rows carry ownership receipts.
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
            assert!(super::super::api::dependency_formula(&value, "app").is_err());
        }
        let installed = p.installed_checked(Kind::Formula).unwrap();
        let revised = |name: &str| {
            super::super::api::dependency_formula(
                &aterm_json::json!({"name": name, "versions": {"stable": "1"}, "revision": 2, "dependencies": []}),
                name,
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
}
