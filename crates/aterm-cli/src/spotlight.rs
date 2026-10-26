// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm doctor`'s `spotlight:` row.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const QUERY: Duration = Duration::from_secs(10);

const RUST_DOCS: &str = "share/doc/rust/html/";

/// init's Rust in Spotlight group, whose items remove rust-docs: `aterm_machines::init_spotlight::ID`,
/// which this crate reaches on macOS alone.
const INIT_SPOTLIGHT: &str = "init.spotlight";

/// What Spotlight's index holds under the home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spotlit {
    /// Each folder holding indexed crate files, `~`-relative, with its count, most first.
    pub builds: Vec<(String, usize)>,
    /// rustup's home, `~`-relative, and the `*.html` indexed in it.
    pub rustup: Option<(String, Pages)>,
    /// Why bare doctor leaves the indexed `rust-docs` pages where they are, when it does
    /// ([`kept_why`]): the person's System State plan leaves Rust in Spotlight unchecked, or the
    /// plan could not be read, and a choice doctor cannot see is never overridden.
    pub docs_kept: Option<String>,
}

impl Spotlit {
    /// Whether Spotlight indexes `rust-docs` pages the person did not choose to keep: bare
    /// `aterm doctor` then sets the docs aside (macOS's `aterm_machines::init::Doctor::rust_docs`).
    pub(crate) fn docs_indexed(&self) -> bool {
        self.docs_kept.is_none() && self.has_docs()
    }

    /// Whether Spotlight indexes any `rust-docs` page.
    fn has_docs(&self) -> bool {
        self.rustup
            .as_ref()
            .is_some_and(|(_, pages)| !pages.docs.is_empty())
    }

    /// These readings with the person's choice on indexed `rust-docs` read through `kept`, which
    /// is asked only when there are some ([`kept_by_choice`]).
    fn with_choice(mut self, kept: impl FnOnce() -> Option<String>) -> Self {
        self.docs_kept = if self.has_docs() { kept() } else { None };
        self
    }
}

/// Whether System State's records say the person chose to leave Rust in Spotlight as it is:
/// they cancelled the first-launch review (every group then reads Leave as is), or the plan in
/// force is theirs, not one aterm chose by itself, and leaves Rust in Spotlight unchecked.
#[cfg(any(target_os = "macos", test))]
const fn declines(cancelled: bool, provisional: bool, spotlight_checked: bool) -> bool {
    cancelled || (!provisional && !spotlight_checked)
}

/// Why the indexed `rust-docs` stay, from the setup records' read of whether the review was
/// cancelled, whether the plan is provisional and whether it checks Rust in Spotlight
/// ([`declines`]); `None` when bare doctor sets them aside. Records that cannot be read keep
/// them: doctor never overrides a choice it cannot see.
#[cfg(any(target_os = "macos", test))]
fn kept_why(read: Result<(bool, bool, bool), String>) -> Option<String> {
    match read {
        Ok((cancelled, provisional, spotlight_checked)) => {
            declines(cancelled, provisional, spotlight_checked).then(|| {
                "kept, as your System State plan leaves Rust in Spotlight unchecked".to_owned()
            })
        }
        Err(why) => Some(format!(
            "kept, as your System State plan could not be read ({why})"
        )),
    }
}

/// [`kept_why`] over this Mac's setup records.
#[cfg(target_os = "macos")]
fn kept_by_choice() -> Option<String> {
    kept_why(
        crate::macos_doctor::initial_setup_snapshot()
            .map(|snapshot| {
                (
                    snapshot.cancelled,
                    snapshot.provisional,
                    snapshot
                        .plan
                        .selection
                        .contains(aterm_types::macos_setup::Setting::Spotlight),
                )
            })
            .map_err(|why| why.to_string()),
    )
}

/// No setup records off macOS.
#[cfg(not(target_os = "macos"))]
const fn kept_by_choice() -> Option<String> {
    None
}

/// The `*.html` Spotlight indexes in rustup's home, each list most first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pages {
    /// `rust-docs` pages (a toolchain's `share/doc/rust/html`), per toolchain.
    pub docs: Vec<(String, usize)>,
    /// Every other page.
    pub other: Vec<(String, usize)>,
}

/// Spotlight's count, or why it was not taken; `None` off macOS (no row).
pub(crate) fn facts() -> Option<Result<Spotlit, String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if cfg!(test) || aterm_uds::test_binary::scratch_root().is_some() {
        return Some(Err("a test binary".into()));
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Some(Err("no home folder".into()));
    };
    let rustup = rustup_dir(&home);
    let kept = atpkg::probe_cache::file("spotlight")
        .and_then(|file| Some((file, day_key(&home, &rustup, atpkg::probe_cache::today()?)?)));
    Some(
        measure_kept(kept.as_ref(), || {
            measure(&home, &rustup, &|dir, pattern| mdfind(dir, pattern))
        })
        .map(|lit| lit.with_choice(kept_by_choice)),
    )
}

/// rustup's home: `$RUSTUP_HOME`, else `~/.rustup`.
fn rustup_dir(home: &Path) -> PathBuf {
    std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".rustup"), Into::into)
}

/// What a clean verdict depends on besides the index itself: the day, the home, rustup's home
/// and each toolchain's `rust-docs` folder (its time, or its absence), so a toolchain or a
/// `rust-docs` install, removal or set-aside measures again at once. A build written under the
/// home since the last measure waits for the next day.
fn day_key(home: &Path, rustup: &Path, day: u64) -> Option<String> {
    let mut parts = vec![
        Some(day.to_string()),
        Some(home.display().to_string()),
        Some(rustup.display().to_string()),
    ];
    let toolchains = rustup.join("toolchains");
    parts.push(Some(stamp(&toolchains)));
    if let Ok(entries) = std::fs::read_dir(&toolchains) {
        let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let docs = toolchains.join(&name).join("share/doc/rust");
            parts.push(Some(format!(
                "{} {} {}",
                name.to_string_lossy(),
                stamp(&docs),
                stamp(&docs.join("html"))
            )));
        }
    }
    atpkg::probe_cache::key(parts)
}

/// A folder's modification time in nanoseconds, or `absent`.
fn stamp(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(|| "absent".to_owned(), |at| at.as_nanos().to_string())
}

/// `measure`, or the clean verdict `kept` (its file and [`day_key`]) holds: only a clean one is
/// kept, so a verdict that names something to fix is measured every run and the fix shows at
/// once.
fn measure_kept(
    kept: Option<&(PathBuf, String)>,
    measure: impl FnOnce() -> Result<Spotlit, String>,
) -> Result<Spotlit, String> {
    if let Some((file, key)) = kept
        && let Some(lit) = atpkg::probe_cache::get(file, key).and_then(|v| decode(&v))
    {
        return Ok(lit);
    }
    let measured = measure();
    if let (Some((file, key)), Ok(lit)) = (kept, &measured)
        && lit.builds.is_empty()
        && lit
            .rustup
            .as_ref()
            .is_none_or(|(_, pages)| pages.docs.is_empty())
    {
        atpkg::probe_cache::put(file, key, &encode(lit));
    }
    measured
}

/// Separates fields in a kept clean verdict.
const FIELD: char = '\u{1c}';

/// A clean verdict as one line: `clean`, then rustup's home and each other page source with its
/// count.
fn encode(lit: &Spotlit) -> String {
    let mut out = String::from("clean");
    if let Some((home, pages)) = &lit.rustup {
        out.push(FIELD);
        out.push_str(home);
        for (source, n) in &pages.other {
            out.push(FIELD);
            out.push_str(source);
            out.push(FIELD);
            out.push_str(&n.to_string());
        }
    }
    out
}

fn decode(kept: &str) -> Option<Spotlit> {
    let mut fields = kept.split(FIELD);
    if fields.next()? != "clean" {
        return None;
    }
    let rustup = match fields.next() {
        None => None,
        Some(home) => {
            let mut other = Vec::new();
            while let Some(source) = fields.next() {
                other.push((source.to_owned(), fields.next()?.parse().ok()?));
            }
            Some((
                home.to_owned(),
                Pages {
                    docs: Vec::new(),
                    other,
                },
            ))
        }
    };
    Some(Spotlit {
        docs_kept: None,
        builds: Vec::new(),
        rustup,
    })
}

/// One Spotlight query: the NUL-separated paths named `pattern` under a folder.
type Query<'a> = dyn Fn(&Path, &str) -> Result<String, String> + Sync + 'a;

/// The two queries, crate files under the home and HTML pages in rustup's home, run at once.
fn measure(home: &Path, rustup: &Path, query: &Query<'_>) -> Result<Spotlit, String> {
    let real_rustup = std::fs::canonicalize(rustup)
        .ok()
        .filter(|real| real.is_dir());
    let (crates, listed) = std::thread::scope(|scope| {
        let html = real_rustup
            .as_deref()
            .map(|real| scope.spawn(move || query(real, "*.html")));
        let crates = query(home, "*.rmeta");
        let listed = html.map(|handle| {
            handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        (crates, listed)
    });
    let crates = crates?;
    let mut builds = Tally::default();
    for path in crates.split('\0').filter(|p| !p.is_empty()) {
        builds.add(tilde(home, &build_root(path)));
    }
    let rustup_home = match (real_rustup, listed) {
        (Some(real), Some(listed)) => {
            let listed = listed?;
            let mut manifests = HashMap::new();
            let found = pages(
                &real.to_string_lossy(),
                listed.split('\0').filter(|p| !p.is_empty()),
                |toolchain, rest| {
                    installed_by(
                        manifests.entry(toolchain.to_owned()).or_insert_with(|| {
                            read_manifests(&real.join("toolchains").join(toolchain))
                        }),
                        rest,
                    )
                },
                |folder| tilde(home, folder),
            );
            (found != Pages::default()).then(|| (tilde(home, &rustup.to_string_lossy()), found))
        }
        _ => None,
    };
    Ok(Spotlit {
        docs_kept: None,
        builds: builds.most_first(),
        rustup: rustup_home,
    })
}

/// Counts per name, in first-seen order until [`Tally::most_first`].
#[derive(Default)]
struct Tally(Vec<(String, usize)>);

impl Tally {
    fn add(&mut self, name: String) {
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, k)) => *k += 1,
            None => self.0.push((name, 1)),
        }
    }

    fn most_first(mut self) -> Vec<(String, usize)> {
        self.0
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.0
    }
}

/// Sorts Spotlight's `*.html` paths under rustup's home `root`.
pub(crate) fn pages<'a>(
    root: &str,
    paths: impl IntoIterator<Item = &'a str>,
    mut listed: impl FnMut(&str, &str) -> Option<String>,
    shown: impl Fn(&str) -> String,
) -> Pages {
    let mut docs = Tally::default();
    let mut other = Tally::default();
    for path in paths {
        let in_toolchain = path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix("/toolchains/"))
            .and_then(|rest| rest.split_once('/'));
        if let Some((toolchain, rest)) = in_toolchain {
            if rest.starts_with(RUST_DOCS) {
                docs.add(toolchain.to_owned());
                continue;
            }
            if let Some(component) = listed(toolchain, rest) {
                other.add(format!("the {component} component"));
                continue;
            }
        }
        let folder = path.rsplit_once('/').map_or(path, |(dir, _)| dir);
        other.add(shown(folder));
    }
    Pages {
        docs: docs.most_first(),
        other: other.most_first(),
    }
}

fn read_manifests(toolchain: &Path) -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir(toolchain.join("lib/rustlib")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let component = name.strip_prefix("manifest-")?.to_owned();
            let text = manifest_text(&entry.path())?;
            Some((component, text))
        })
        .collect()
}

fn manifest_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    const LIMIT: u64 = 16 * 1024 * 1024;

    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return None;
    }
    let mut text = String::new();
    file.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= LIMIT).then_some(text)
}

/// The component whose manifest lists `rest`, as a `file:` entry or under a `dir:` one.
pub(crate) fn installed_by(manifests: &[(String, String)], rest: &str) -> Option<String> {
    manifests.iter().find_map(|(component, text)| {
        text.lines()
            .any(|line| {
                line.strip_prefix("file:") == Some(rest)
                    || line.strip_prefix("dir:").is_some_and(|dir| {
                        rest.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
                    })
            })
            .then(|| component.clone())
    })
}

/// The paths named `pattern` under `dir` (NUL-separated): `mdfind -count` first, and the list
/// only when the count is not zero, so a clean index costs no listing.
fn mdfind(dir: &Path, pattern: &str) -> Result<String, String> {
    count_then_list(
        || mdfind_run(dir, pattern, true),
        || mdfind_run(dir, pattern, false),
    )
}

/// `list`'s answer, or nothing when `count` says there is nothing to list.
fn count_then_list(
    count: impl FnOnce() -> Result<String, String>,
    list: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if count()?.trim() == "0" {
        return Ok(String::new());
    }
    list()
}

fn mdfind_run(dir: &Path, pattern: &str, count: bool) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/mdfind");
    if count {
        cmd.arg("-count");
    }
    cmd.arg("-onlyin").arg(dir).arg("-0");
    cmd.arg(format!("kMDItemFSName == \"{pattern}\""));
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + QUERY)
        .map_err(|e| format!("mdfind did not answer: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mdfind failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The folder a crate file's build lives in, as a person would delete or rename it.
pub(crate) fn build_root(path: &str) -> String {
    if let Some((sysroot, _)) = path.split_once("/lib/rustlib/") {
        return sysroot.to_owned();
    }
    let parts: Vec<&str> = path.split('/').collect();
    for name in ["target", "build"] {
        if let Some(i) = parts[..parts.len().saturating_sub(1)]
            .iter()
            .position(|c| *c == name)
        {
            return parts[..=i].join("/");
        }
    }
    if let Some(i) = parts.iter().rposition(|c| *c == "deps")
        && i >= 2
    {
        return parts[..i - 1].join("/");
    }
    parts[..parts.len().saturating_sub(1)].join("/")
}

fn tilde(home: &Path, path: &str) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn largest(counts: &[(String, usize)], more: &str) -> String {
    let mut top: Vec<String> = counts
        .iter()
        .take(3)
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    if counts.len() > 3 {
        top.push(format!("{} more {more}", counts.len() - 3));
    }
    top.join(", ")
}

fn total(counts: &[(String, usize)]) -> usize {
    counts.iter().map(|(_, n)| n).sum()
}

/// The row's fact and whether it is clean, pure over the count.
pub(crate) fn row(measured: &Result<Spotlit, String>) -> (bool, String) {
    let lit = match measured {
        Err(why) => return (false, format!("not measured ({why})")),
        Ok(lit) => lit,
    };
    let none = Pages::default();
    let (home, pages) = lit
        .rustup
        .as_ref()
        .map_or(("~/.rustup", &none), |(home, pages)| (home.as_str(), pages));
    let clean = lit.builds.is_empty() && pages.docs.is_empty();
    let mut lines = Vec::new();
    if clean {
        lines.push("Spotlight indexes no Rust build output or rust-docs under ~".to_owned());
    }
    if !lit.builds.is_empty() {
        lines.push(format!(
            "Spotlight indexes {} crate builds (.rmeta) under ~: {}",
            total(&lit.builds),
            largest(&lit.builds, "folders")
        ));
        lines.push(
            "written where the shims' rule did not place them (a direct targo, another \
             repository's build, an installed toolchain); a folder leaves the index when \
             it is deleted or named *.noindex"
                .into(),
        );
    }
    if !pages.docs.is_empty() {
        lines.push(format!(
            "{} rust-docs pages indexed in {home} ({}): {}",
            total(&pages.docs),
            largest(&pages.docs, "toolchains"),
            lit.docs_kept.as_ref().map_or_else(
                || "bare `aterm doctor` keeps them out of Spotlight".to_owned(),
                |why| format!(
                    "{why}; `{}` removes them",
                    aterm_converge::Remedy::Init {
                        only: INIT_SPOTLIGHT.to_owned()
                    }
                    .command()
                    .unwrap_or_default()
                )
            )
        ));
    }
    if !pages.other.is_empty() {
        lines.push(format!(
            "{} other HTML files indexed in {home}, not rust-docs: {}",
            total(&pages.other),
            largest(&pages.other, "sources")
        ));
    }
    (clean, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate query and the HTML query run on two threads at once, and their answers land
    /// where each belongs. NEGATIVE CONTROL: with no rustup home only the crate query runs.
    #[test]
    fn the_two_queries_run_at_once() {
        let home = aterm_tempfile::tempdir().expect("home");
        let rustup = home.path().join(".rustup");
        std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
        let ran = std::sync::Mutex::new(Vec::new());
        let query = |dir: &Path, pattern: &str| -> Result<String, String> {
            ran.lock()
                .unwrap()
                .push((pattern.to_owned(), std::thread::current().id()));
            Ok(match pattern {
                "*.rmeta" => format!(
                    "{}/w/target/debug/deps/libx-1.rmeta\0",
                    home.path().display()
                ),
                _ => format!("{}/tmp/report.html\0", dir.display()),
            })
        };
        let lit = measure(home.path(), &rustup, &query).unwrap();
        assert_eq!(lit.builds, vec![("~/w/target".to_owned(), 1)]);
        assert_eq!(
            lit.rustup.as_ref().map(|(_, pages)| pages.other.len()),
            Some(1),
            "{lit:?}"
        );
        let ran = std::mem::take(&mut *ran.lock().unwrap());
        assert_eq!(ran.len(), 2, "{ran:?}");
        assert_ne!(ran[0].1, ran[1].1, "each query on its own thread: {ran:?}");
        let ran_alone = std::sync::Mutex::new(Vec::new());
        let alone = |_: &Path, pattern: &str| -> Result<String, String> {
            ran_alone.lock().unwrap().push(pattern.to_owned());
            Ok(String::new())
        };
        let lit = measure(home.path(), &home.path().join("no-rustup"), &alone).unwrap();
        assert_eq!(lit.rustup, None);
        assert_eq!(*ran_alone.lock().unwrap(), ["*.rmeta"]);
    }

    /// A PERSON WHO LEFT RUST IN SPOTLIGHT UNCHECKED KEEPS THE DOCS: indexed rust-docs pages
    /// are then theirs, bare doctor builds no repair for them and the row says why; the choice
    /// is read only when there are docs. NEGATIVE CONTROL: with no such choice the same pages
    /// are set aside and the row names bare doctor; a provisional plan, which aterm chose by
    /// itself, is no choice.
    #[test]
    fn a_person_who_left_rust_in_spotlight_unchecked_keeps_the_docs() {
        let docs = Spotlit {
            builds: Vec::new(),
            rustup: Some((
                "~/.rustup".to_owned(),
                Pages {
                    docs: vec![("stable-aarch64-apple-darwin".to_owned(), 84_915)],
                    other: Vec::new(),
                },
            )),
            docs_kept: None,
        };
        let chosen = docs
            .clone()
            .with_choice(|| kept_why(Ok((false, false, false))));
        assert!(
            !chosen.docs_indexed(),
            "kept by choice: nothing to set aside"
        );
        let (_, fact) = row(&Ok(chosen));
        assert!(
            fact.contains(
                "kept, as your System State plan leaves Rust in Spotlight unchecked; \
                 `aterm init --only init.spotlight` removes them"
            ),
            "{fact}"
        );
        let unread = docs
            .clone()
            .with_choice(|| kept_why(Err("a pass is writing them".to_owned())));
        assert!(
            !unread.docs_indexed(),
            "records doctor cannot read: nothing set aside"
        );
        let (_, fact) = row(&Ok(unread));
        assert!(
            fact.contains(
                "kept, as your System State plan could not be read (a pass is writing them)"
            ),
            "{fact}"
        );
        let unchosen = docs.with_choice(|| kept_why(Ok((false, false, true))));
        assert!(unchosen.docs_indexed());
        let (_, fact) = row(&Ok(unchosen));
        assert!(
            fact.contains("bare `aterm doctor` keeps them out of Spotlight"),
            "{fact}"
        );
        let clean = Spotlit {
            builds: Vec::new(),
            rustup: None,
            docs_kept: None,
        }
        .with_choice(|| panic!("no docs: the setup records are not read"));
        assert!(clean.docs_kept.is_none());

        assert!(
            declines(true, false, true),
            "a cancelled review leaves all as is"
        );
        assert!(
            declines(false, false, false),
            "the person's plan leaves it unchecked"
        );
        assert!(
            !declines(false, true, false),
            "aterm's own plan is no choice"
        );
        assert!(!declines(false, false, true), "the person's plan checks it");
    }

    /// A zero count lists nothing. NEGATIVE CONTROL: a nonzero count lists.
    #[test]
    fn a_zero_count_lists_nothing() {
        let listed = std::cell::Cell::new(0);
        let list = || {
            listed.set(listed.get() + 1);
            Ok("a\0".to_owned())
        };
        assert_eq!(
            count_then_list(|| Ok("0\n".into()), list),
            Ok(String::new())
        );
        assert_eq!(listed.get(), 0);
        assert_eq!(
            count_then_list(|| Ok("3\n".into()), list),
            Ok("a\0".to_owned())
        );
        assert_eq!(listed.get(), 1);
        assert_eq!(
            count_then_list(|| Err("mdfind did not answer".into()), list),
            Err("mdfind did not answer".into())
        );
        assert_eq!(listed.get(), 1, "an unanswered count lists nothing");
    }

    /// A clean verdict is kept for its key: the next run under that key measures nothing. A
    /// verdict that names something to fix is never kept. NEGATIVE CONTROL: another key (the
    /// next day, a set-aside `rust-docs` folder) measures again.
    #[test]
    fn a_clean_verdict_is_kept_for_the_day_and_a_dirty_one_never() {
        let dir = aterm_tempfile::tempdir().expect("state");
        let home = dir.path().join("home");
        let rustup = home.join(".rustup");
        let docs = rustup.join("toolchains/stable/share/doc/rust");
        std::fs::create_dir_all(docs.join("html")).unwrap();
        let file = dir.path().join("doctor/spotlight");
        let measured = std::cell::Cell::new(0);
        let clean = || {
            measured.set(measured.get() + 1);
            Ok(Spotlit {
                docs_kept: None,
                builds: Vec::new(),
                rustup: Some((
                    "~/.rustup".to_owned(),
                    Pages {
                        docs: Vec::new(),
                        other: vec![("the rustc component".to_owned(), 6)],
                    },
                )),
            })
        };
        let today = (file.clone(), day_key(&home, &rustup, 20_000).unwrap());
        let first = measure_kept(Some(&today), clean).unwrap();
        assert_eq!(
            measure_kept(Some(&today), clean).unwrap(),
            first,
            "kept whole"
        );
        assert_eq!(measured.get(), 1, "the second run measured nothing");
        let tomorrow = (file.clone(), day_key(&home, &rustup, 20_001).unwrap());
        let _ = measure_kept(Some(&tomorrow), clean);
        assert_eq!(measured.get(), 2, "a new day measures again");
        std::fs::rename(docs.join("html"), docs.join("html.noindex")).unwrap();
        let set_aside = (file.clone(), day_key(&home, &rustup, 20_001).unwrap());
        assert_ne!(set_aside.1, tomorrow.1, "the set-aside changed the key");
        let _ = measure_kept(Some(&set_aside), clean);
        assert_eq!(measured.get(), 3);
        let dirty = || {
            measured.set(measured.get() + 1);
            Ok(Spotlit {
                docs_kept: None,
                builds: vec![("~/w/target".to_owned(), 3)],
                rustup: None,
            })
        };
        let other_day = (file, day_key(&home, &rustup, 30_000).unwrap());
        let _ = measure_kept(Some(&other_day), dirty);
        let _ = measure_kept(Some(&other_day), dirty);
        assert_eq!(
            measured.get(),
            5,
            "a verdict with something to fix is measured every run"
        );
        let _ = measure_kept(None, clean);
        assert_eq!(measured.get(), 6, "no state folder: measured");
    }

    #[cfg(unix)]
    #[test]
    fn component_manifests_leave_nonfiles_and_oversized_text_unclassified() {
        use std::os::unix::ffi::OsStrExt as _;

        let scratch = aterm_tempfile::tempdir().expect("private toolchain");
        let rustlib = scratch.path().join("lib/rustlib");
        std::fs::create_dir_all(&rustlib).unwrap();
        let good = rustlib.join("manifest-rustc");
        std::fs::write(&good, "file:share/doc/rust/COPYRIGHT.html\n").unwrap();
        std::fs::create_dir(rustlib.join("manifest-directory")).unwrap();
        std::os::unix::fs::symlink(&good, rustlib.join("manifest-link")).unwrap();
        std::fs::write(rustlib.join("manifest-invalid"), [0xff]).unwrap();
        std::fs::File::create(rustlib.join("manifest-large"))
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let fifo = rustlib.join("manifest-pipe");
        let fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a live NUL-terminated path inside our exclusive scratch tree.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let found = read_manifests(scratch.path());
        assert_eq!(
            found,
            [(
                "rustc".to_owned(),
                "file:share/doc/rust/COPYRIGHT.html\n".to_owned()
            )]
        );
        assert_eq!(
            installed_by(&found, "share/doc/rust/COPYRIGHT.html").as_deref(),
            Some("rustc")
        );
    }

    #[test]
    fn a_crate_file_is_counted_under_the_folder_that_holds_its_build() {
        for (path, root) in [
            (
                "/U/a/Developer/fix/target/verify/tippy/debug/deps/libx-1.rmeta",
                "/U/a/Developer/fix/target",
            ),
            (
                "/U/a/Developer/fix/target/debug/deps/libx-1.rmeta",
                "/U/a/Developer/fix/target",
            ),
            (
                "/U/a/trust/build/aarch64-apple-darwin/stage1-rustc/aarch64-apple-darwin/release/deps/librustc-1.rmeta",
                "/U/a/trust/build",
            ),
            (
                "/U/a/Library/Application Support/aterm/pkg/store/trust/9192/lib/rustlib/aarch64-apple-darwin/lib/libstd-1.rmeta",
                "/U/a/Library/Application Support/aterm/pkg/store/trust/9192",
            ),
            ("/U/a/out/lane/debug/deps/libx-1.rmeta", "/U/a/out/lane"),
            ("/U/a/loose/libx.rmeta", "/U/a/loose"),
            ("/U/a/w/target", "/U/a/w"),
        ] {
            assert_eq!(build_root(path), root, "{path}");
        }
        assert_eq!(tilde(Path::new("/U/a"), "/U/a/x"), "~/x");
        assert_eq!(tilde(Path::new("/U/a"), "/U/ab/x"), "/U/ab/x");
    }

    #[test]
    fn only_rust_docs_pages_count_as_rust_docs() {
        let root = "/U/a/.rustup";
        let manifests = |toolchain: &str| -> Vec<(String, String)> {
            match toolchain {
                "stable-aarch64-apple-darwin" => vec![
                    (
                        "rustc-aarch64-apple-darwin".into(),
                        "file:bin/rustc\nfile:share/doc/rust/COPYRIGHT.html\n".into(),
                    ),
                    (
                        "cargo-aarch64-apple-darwin".into(),
                        "dir:share/doc/cargo\n".into(),
                    ),
                ],
                _ => vec![],
            }
        };
        let paths = [
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html/std/index.html",
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html/book/ch01.html",
            "/U/a/.rustup/toolchains/1.97.1-aarch64-apple-darwin/share/doc/rust/html/index.html",
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/COPYRIGHT.html",
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/cargo/x.html",
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html.html",
            "/U/a/.rustup/toolchains/1.97.1-aarch64-apple-darwin/share/doc/rust/COPYRIGHT.html",
            "/U/a/.rustup/tmp/report.html",
            "/U/a/.rustup/tmp/other.html",
            "/elsewhere/toolchains/x/share/doc/rust/html/a.html",
        ];
        let found = pages(
            root,
            paths,
            |toolchain, rest| installed_by(&manifests(toolchain), rest),
            |folder| tilde(Path::new("/U/a"), folder),
        );
        assert_eq!(
            found.docs,
            vec![
                ("stable-aarch64-apple-darwin".to_owned(), 2),
                ("1.97.1-aarch64-apple-darwin".to_owned(), 1),
            ]
        );
        assert_eq!(
            found.other,
            vec![
                ("~/.rustup/tmp".to_owned(), 2),
                ("/elsewhere/toolchains/x/share/doc/rust/html".to_owned(), 1),
                ("the cargo-aarch64-apple-darwin component".to_owned(), 1),
                ("the rustc-aarch64-apple-darwin component".to_owned(), 1),
                (
                    "~/.rustup/toolchains/1.97.1-aarch64-apple-darwin/share/doc/rust".to_owned(),
                    1
                ),
                (
                    "~/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust".to_owned(),
                    1
                ),
            ]
        );
    }

    #[test]
    fn the_row_names_what_spotlight_holds_and_the_remedy() {
        let clean = Spotlit {
            docs_kept: None,
            builds: vec![],
            rustup: None,
        };
        assert_eq!(
            row(&Ok(clean)),
            (
                true,
                "Spotlight indexes no Rust build output or rust-docs under ~".into()
            )
        );
        let licenses = Spotlit {
            docs_kept: None,
            builds: vec![],
            rustup: Some((
                "~/.rustup".into(),
                Pages {
                    docs: vec![],
                    other: vec![("the rustc-aarch64-apple-darwin component".into(), 6)],
                },
            )),
        };
        assert_eq!(
            row(&Ok(licenses)),
            (
                true,
                "Spotlight indexes no Rust build output or rust-docs under ~\n\
                 6 other HTML files indexed in ~/.rustup, not rust-docs: \
                 the rustc-aarch64-apple-darwin component 6"
                    .into()
            )
        );
        let lit = Spotlit {
            docs_kept: None,
            builds: vec![
                ("$HOME/trust/build".into(), 7900),
                ("~/Developer/fix/target".into(), 254),
                ("~/x/target".into(), 9),
                ("~/y/target".into(), 2),
                ("~/z/target".into(), 1),
            ],
            rustup: Some((
                "~/.rustup".into(),
                Pages {
                    docs: vec![("stable-aarch64-apple-darwin".into(), 84_915)],
                    other: vec![("the rustc-aarch64-apple-darwin component".into(), 2)],
                },
            )),
        };
        let (clean, fact) = row(&Ok(lit));
        assert!(!clean);
        let lines: Vec<&str> = fact.lines().collect();
        assert_eq!(
            lines[0],
            "Spotlight indexes 8166 crate builds (.rmeta) under ~: $HOME/trust/build 7900, \
             ~/Developer/fix/target 254, ~/x/target 9, 2 more folders"
        );
        assert!(lines[1].contains("*.noindex"), "{fact}");
        assert_eq!(
            lines[2],
            "84915 rust-docs pages indexed in ~/.rustup (stable-aarch64-apple-darwin 84915): \
             bare `aterm doctor` keeps them out of Spotlight"
        );
        assert_eq!(
            lines[3],
            "2 other HTML files indexed in ~/.rustup, not rust-docs: \
             the rustc-aarch64-apple-darwin component 2"
        );
        let docs_only = Spotlit {
            docs_kept: None,
            builds: vec![],
            rustup: Some((
                "~/.rustup".into(),
                Pages {
                    docs: vec![("stable-aarch64-apple-darwin".into(), 3)],
                    other: vec![],
                },
            )),
        };
        assert_eq!(row(&Ok(docs_only.clone())).1.lines().count(), 1);
        assert!(!row(&Ok(docs_only)).0);
        assert_eq!(
            row(&Err("a test binary".into())),
            (false, "not measured (a test binary)".into())
        );
    }

    /// The line that removes rust-docs names init's Spotlight group by the id init takes it by.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_rust_docs_remedy_names_inits_spotlight_group() {
        assert_eq!(INIT_SPOTLIGHT, aterm_machines::init_spotlight::ID);
    }
}
