// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sandbox profile: ONE profile (`runs/<tag>/jail.sb`) for everything a lab starts — the stub, the instance, its shells, the real Claude Code, Codex, agy or Muse Code, and what the lab starts in the jail outside that tree (`codex-run`, the agents' `--version`, the selftests' controls) — so every child inherits it.

use std::path::Path;

use crate::paths::{physical, profile_safe};
use crate::protected::{
    MACH_DENY, MACH_DENY_OPEN, SCRATCH_DIRS, inside_protected, protected_paths,
};

/// The profile's fixed text, with `@…@` holes for what each lab names.
const TEMPLATE: &str = include_str!("jail.sb");

/// The same preference-channel denials in the regular lab and Codex rig.
pub(crate) const PREFERENCES: &str = include_str!("preferences.sb");

/// What a lab's profile names: its run and socket directories, the agents under test, whose data it guards, its mark, and what the lab trusts from OUTSIDE the jail (its state dir, its own program and source, the aterm its door runs, and the checkout's builds another lab's `up` may run).
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// The lab's run directory (`runs/<tag>`).
    pub run: String,
    /// The Claude Code under test.
    pub claude: String,
    /// The Codex under test, when one is managed here (empty: none).
    pub codex: String,
    /// The Antigravity CLI (`agy`) under test (empty: none).
    pub agy: String,
    /// The Muse Code (`muse`) under test (empty: none).
    pub muse: String,
    /// The Grok Build (`grok`) under test (empty: none).
    pub grok: String,
    /// The Kimi Code (`kimi`) under test (empty: none).
    pub kimi: String,
    /// The lab's socket directory.
    pub sockdir: String,
    /// The owner's home (or a probe's replica of it).
    pub home: String,
    /// The caller's physical HOME (or its replica), when different.
    pub caller_home: String,
    /// The owner's Claude socket directory (or its replica).
    pub ccsocks: String,
    /// The lab's state dir (`runs/`, `logs/`, `captures/`).
    pub state: String,
    /// The root under which every checkout's default state dir sits ([`crate::paths::state_root`], empty: this user's): fenced, so no jail reads another checkout's runs, logs or results; the lab's own state dir is reopened ([`render`]).
    pub states: String,
    /// The user's temp dir (empty: this user's, `DARWIN_USER_TEMP_DIR`).
    pub user_tmp: String,
    /// The jail's mark (empty: a fresh one).
    pub mark: String,
    /// The directories of the lab's own program and its source.
    pub lab_dirs: Vec<String>,
    /// The aterm the lab's door runs (empty: none yet).
    pub aterm: String,
    /// The checkout's `target` directory as written (its `debug/` and `release/` are trusted).
    pub repo_target: String,
}

impl Spec {
    /// The spec of a lab of this checkout: `lab_dirs`, `aterm` and `repo_target` filled from this program's own facts.
    #[must_use]
    pub fn for_lab(run: &str, claude: &str, codex: &str, sockdir: &str, mark: &str) -> Spec {
        Spec::for_lab_in(crate::paths::paths(), run, claude, codex, sockdir, mark)
    }

    /// [`Spec::for_lab`] from the facts `p` (the seam a test hands a fake account home: [`crate::paths::Paths::resolve_with`]).
    #[must_use]
    pub fn for_lab_in(
        p: &crate::paths::Paths,
        run: &str,
        claude: &str,
        codex: &str,
        sockdir: &str,
        mark: &str,
    ) -> Spec {
        Spec {
            run: run.into(),
            claude: claude.into(),
            codex: codex.into(),
            agy: String::new(),
            muse: String::new(),
            grok: String::new(),
            kimi: String::new(),
            sockdir: sockdir.into(),
            home: p.real_home.clone(),
            caller_home: p.caller_home.clone(),
            ccsocks: crate::protected::CCSOCKS.into(),
            state: p.state.to_string_lossy().into_owned(),
            states: String::new(),
            user_tmp: String::new(),
            mark: mark.into(),
            lab_dirs: vec![
                p.exe_dir().to_string_lossy().into_owned(),
                p.crate_dir().to_string_lossy().into_owned(),
            ],
            aterm: p
                .aterm
                .as_ref()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default(),
            repo_target: p.repo.join("target").to_string_lossy().into_owned(),
        }
    }
}

/// A fresh jail mark for `tag`: `lab.jail.<tag>.<16 hex>`.
#[must_use]
pub fn new_mark(tag: &str) -> String {
    let mut b = [0u8; 8];
    let _ = aterm_uds::rand::fill(&mut b);
    format!("lab.jail.{tag}.{:016x}", u64::from_ne_bytes(b))
}

/// Whether `mark` is a jail mark: `lab.jail.` and then letters, digits, `.`, `_` and `-`.
#[must_use]
pub fn valid_mark(mark: &str) -> bool {
    mark.strip_prefix("lab.jail.").is_some_and(|rest| {
        !rest.is_empty()
            && mark
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    })
}

/// `path` as a literal in the profile's regular expressions: each character
/// the expression syntax gives a meaning is escaped (a path the profile can
/// quote holds no quote and no backslash: [`profile_safe`]).
fn regex_literal(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if ".[]{}()*+?^$|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn parent(p: &str) -> String {
    Path::new(p)
        .parent()
        .map_or_else(|| "/".to_string(), |d| d.to_string_lossy().into_owned())
}

/// A selected non-store Codex package, never an arbitrary image's parent.
fn codex_package_root(exe: &str, homes: &[&str]) -> Option<String> {
    use aterm_agent::harness::codex_alab::{PACKAGE_JSON, package_entrypoint};
    use std::io::Read as _;

    let exe = Path::new(exe);
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" || exe.file_name()? != "codex" {
        return None;
    }
    let root = bin.parent()?;
    if !root.is_absolute() || homes.iter().any(|h| Path::new(h).starts_with(root)) {
        return None;
    }
    let root_text = root.to_str()?;
    if !profile_safe(root_text)
        || std::fs::canonicalize(exe).ok()? != exe
        || !std::fs::symlink_metadata(exe).ok()?.file_type().is_file()
    {
        return None;
    }
    let (manifest, meta) = crate::sys::scratch_file_with_meta(&root.join(PACKAGE_JSON)).ok()??;
    const LIMIT: u64 = 1024 * 1024;
    if meta.len() > LIMIT {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    manifest.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > LIMIT {
        return None;
    }
    let entry = package_entrypoint(&text)?;
    (std::fs::canonicalize(root.join(entry)).ok()? == exe).then(|| root_text.to_owned())
}

/// Render the profile of `spec`.
/// # Errors
/// A directory that does not exist, a path the profile cannot quote, a bad mark, or a run directory inside the owner's data.
#[allow(clippy::too_many_lines)]
pub fn render(spec: &Spec) -> Result<String, String> {
    let real = |p: &str| {
        std::fs::canonicalize(p)
            .map(|r| r.to_string_lossy().into_owned())
            .map_err(|_| format!("render_jail: no such directory: {p}"))
    };
    let (run, sockdir, st) = (real(&spec.run)?, real(&spec.sockdir)?, real(&spec.state)?);
    let ut = if spec.user_tmp.is_empty() {
        crate::sys::user_temp_dir().ok_or("render_jail: no DARWIN_USER_TEMP_DIR")?
    } else {
        spec.user_tmp.clone()
    };
    let ut = real(&ut)?;
    let mark = if spec.mark.is_empty() {
        new_mark("probe")
    } else {
        spec.mark.clone()
    };
    if !valid_mark(&mark) {
        return Err(format!("render_jail: bad mark {mark}"));
    }
    let (home, cc, claude, codex) = (&spec.home, &spec.ccsocks, &spec.claude, &spec.codex);
    if !Path::new(home).is_absolute() || home == "/" {
        return Err("render_jail: the account home must be an absolute non-root path".into());
    }
    let mut homes = vec![home.as_str()];
    if !spec.caller_home.is_empty() && spec.caller_home != *home {
        if !Path::new(&spec.caller_home).is_absolute() || spec.caller_home == "/" {
            return Err("render_jail: the caller home must be an absolute non-root path".into());
        }
        homes.push(&spec.caller_home);
    }
    let agy = &spec.agy;
    let muse = &spec.muse;
    let grok = &spec.grok;
    let kimi = &spec.kimi;
    let tg_written = spec.repo_target.clone();
    let tg = physical(Path::new(&tg_written))
        .to_string_lossy()
        .into_owned();
    let dbg = physical(Path::new(&format!("{tg}/debug")))
        .to_string_lossy()
        .into_owned();
    let rel = physical(Path::new(&format!("{tg}/release")))
        .to_string_lossy()
        .into_owned();
    let states = if spec.states.is_empty() {
        crate::paths::state_root(crate::sys::getuid())
            .to_string_lossy()
            .into_owned()
    } else {
        spec.states.clone()
    };
    let mut quoted: Vec<&str> = vec![
        home,
        &spec.caller_home,
        &states,
        &run,
        claude,
        &sockdir,
        cc,
        codex,
        agy,
        muse,
        grok,
        kimi,
        &st,
        &spec.aterm,
        &tg_written,
        &tg,
        &dbg,
        &rel,
        &ut,
    ];
    quoted.extend(spec.lab_dirs.iter().map(String::as_str));
    if quoted.iter().any(|p| !profile_safe(p)) {
        return Err(format!(
            "a quote, a backslash or a control character in {} cannot go into the sandbox profile",
            quoted.join(", ")
        ));
    }
    if homes.iter().any(|h| inside_protected(&run, h, cc)) {
        return Err(format!(
            "the lab's run directory may not sit inside the owner's data ({run})"
        ));
    }
    if let Some(h) = homes.iter().find(|h| {
        let p = physical(Path::new(h));
        Path::new(&run).starts_with(h) || Path::new(&run).starts_with(&p)
    }) {
        return Err(format!(
            "the lab's run directory ({run}) sits inside the home {h}, whose contents the jail fences: the harness's \
             tape store would be refused its way to the run. Leave LAB_STATE unset (the default is outside every \
             home) or name a directory outside every home"
        ));
    }
    let deny_mach = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("(deny mach-lookup (global-name \"{n}\"))"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut paths = homes
        .iter()
        .flat_map(|h| protected_paths(h, cc))
        .collect::<Vec<_>>();
    paths.sort_by(|a, b| a.1.cmp(&b.1));
    paths.dedup();
    let data: Vec<String> = paths
        .iter()
        .map(|(kind, path)| format!("(deny file-read* file-write* ({} \"{path}\"))", kind.word()))
        .collect();
    let tmp_replica: Vec<String> = homes
        .iter()
        .copied()
        .chain(std::iter::once(cc.as_str()))
        .filter(|p| p.starts_with("/private/tmp/at.") || p.starts_with("/private/tmp/labprobe."))
        .map(|p| {
            format!(
                ";; ... and its own home or Claude sockets dir there (a probe's replica) is left to its rules below\n\
                 (allow file-write* (subpath \"{p}\"))"
            )
        })
        .collect();
    let mut store: Vec<String> = Vec::new();
    for agent in [
        claude.as_str(),
        codex.as_str(),
        agy.as_str(),
        muse.as_str(),
        grok.as_str(),
        kimi.as_str(),
    ] {
        let Some(h) = homes
            .iter()
            .filter(|h| Path::new(agent).starts_with(h))
            .max_by_key(|h| h.len())
        else {
            continue;
        };
        let top = format!("{h}/Library/Application Support/aterm");
        if !agent.starts_with(&format!("{top}/")) {
            if agent == codex.as_str()
                && let Some(root) = codex_package_root(agent, &homes)
            {
                store.push(format!("(allow file-read* (subpath \"{root}\"))"));
                store.push(format!("(allow file-read-data (subpath \"{root}\"))"));
                continue;
            }
            store.push(format!("(allow file-read-data (literal \"{agent}\"))"));
            continue;
        }
        let mut path = parent(&parent(agent));
        store.push(";; ... but the agent under test, which lives in aterm's package store".into());
        store.push(format!("(allow file-read* (subpath \"{path}\"))"));
        store.push(format!("(allow file-read-data (subpath \"{path}\"))"));
        loop {
            path = parent(&path);
            if path.len() < h.len() || !(path == top || path.starts_with(&format!("{top}/"))) {
                break;
            }
            store.push(format!("(allow file-read-metadata (literal \"{path}\"))"));
        }
    }
    let mut trusted = vec![format!("(subpath \"{st}\")")];
    trusted.extend(spec.lab_dirs.iter().map(|d| format!("(subpath \"{d}\")")));
    if !spec.aterm.is_empty() {
        trusted.push(format!("(literal \"{}\")", spec.aterm));
    }
    trusted.push(format!("(subpath \"{tg}/debug\")"));
    trusted.push(format!("(subpath \"{tg}/release\")"));
    let mut target_itself = vec![format!("(literal \"{tg_written}\")")];
    if tg != tg_written {
        target_itself.push(format!("(literal \"{tg}\")"));
    }
    if dbg != format!("{tg}/debug") {
        trusted.push(format!("(subpath \"{dbg}\")"));
    }
    if rel != format!("{tg}/release") {
        trusted.push(format!("(subpath \"{rel}\")"));
    }
    let trusted_read = trusted.clone();
    trusted.extend(target_itself);
    let mut parents: Vec<String> = Vec::new();
    let mut roots: Vec<String> = vec![st.clone()];
    roots.extend(spec.lab_dirs.iter().cloned());
    if !spec.aterm.is_empty() {
        roots.push(spec.aterm.clone());
    }
    roots.extend([dbg.clone(), rel.clone()]);
    for r in roots {
        let mut p = r;
        while p != "/" {
            p = parent(&p);
            parents.push(p.clone());
        }
    }
    parents.sort();
    parents.dedup();
    let parents: Vec<String> = parents
        .iter()
        .map(|p| format!("(literal \"{p}\")"))
        .collect();
    let mut scratch = vec![
        format!("(literal \"{run}/scratch\")"),
        format!("(literal \"{sockdir}/proj\")"),
    ];
    scratch.extend(
        SCRATCH_DIRS
            .iter()
            .map(|n| format!("(literal \"{run}/scratch/{n}\")")),
    );
    let mut out = String::with_capacity(TEMPLATE.len() + 8192);
    for line in TEMPLATE.lines() {
        let piece = match line {
            "@MACH_DENY@" => deny_mach(MACH_DENY),
            "@MACH_DENY_OPEN@" => deny_mach(MACH_DENY_OPEN),
            "@PREFERENCES@" => PREFERENCES.to_string(),
            "@PROTECTED@" => data.join("\n"),
            "@TMP_REPLICA@" => tmp_replica.join("\n"),
            "@AGENT_STORE@" => store.join("\n"),
            "@HOME_READ_DENY@" => homes
                .iter()
                .map(|h| format!("(deny file-read-data (subpath \"{h}\"))"))
                .collect::<Vec<_>>()
                .join("\n"),
            "@STATES_READ_DENY@" => format!(
                "(deny file-read-data (regex #\"^{}/\"))",
                regex_literal(states.trim_end_matches('/'))
            ),
            "@HOME_WRITE_DENY@" => homes
                .iter()
                .map(|h| format!("(deny file-write* (subpath \"{h}\"))"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => line
                .replace("@TRUSTED_PARENTS@", &parents.join(" "))
                .replace("@TRUSTED_READ@", &trusted_read.join(" "))
                .replace("@TRUSTED@", &trusted.join(" "))
                .replace("@SCRATCH_DIRS@", &scratch.join(" "))
                .replace("@SOCKDIR@", &sockdir)
                .replace("@HOME@", home)
                .replace("@RUN@", &run)
                .replace("@MARK@", &mark)
                .replace("@UT@", &ut),
        };
        if !piece.is_empty() {
            out.push_str(&piece);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Whether the profile applies HERE: profiles do not nest, and the lab never runs without its jail (a trivial profile applies inside another sandbox, the lab's own does not: the real one is tested).
/// # Errors
/// Why it does not apply, said for the person.
pub fn check(profile: &std::path::Path) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(profile)
        .arg("/usr/bin/true")
        .output()
        .map_err(|e| format!("sandbox-exec could not run ({e})"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(format!(
        "sandbox-exec cannot apply the lab's profile here ({err}): if this shell is already sandboxed, profiles \
         do not nest. Run aterm-lab from an unsandboxed shell; the lab never runs without its jail."
    ))
}

#[cfg(test)]
#[path = "jail_tests.rs"]
mod tests;
