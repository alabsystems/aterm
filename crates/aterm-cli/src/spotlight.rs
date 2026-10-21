// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm doctor`'s `spotlight:` row: what Spotlight's own index holds, under the home,
//! of Rust build output and of rustup's offline docs (`rust-docs`) — files nobody
//! searches, which Spotlight indexes as fast as a build writes them (`mds_stores` was
//! measured near two cores on a checkout's `target/`). The count is Spotlight's
//! (`mdfind`), never a walk of the disk: a crate's `.rmeta` stands for its build (each
//! folder holding one also holds its objects and fingerprints), and an `.html` under a
//! toolchain's `share/doc/rust/html` for its `rust-docs`. Any other `.html` in rustup's
//! home is named by the component whose manifest lists it, else by its folder
//! ([`pages`]). Read-only, macOS only, and nothing measured in a test binary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long one Spotlight query may take (each answers in about a second).
const QUERY: Duration = Duration::from_secs(10);

/// Where a toolchain keeps its `rust-docs` pages, under the toolchain's folder.
const RUST_DOCS: &str = "share/doc/rust/html/";

/// What Spotlight's index holds under the home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spotlit {
    /// Each folder holding indexed crate files (`*.rmeta`), `~`-relative, with its count,
    /// most first ([`build_root`]).
    pub builds: Vec<(String, usize)>,
    /// rustup's home, `~`-relative, and the `*.html` indexed in it; `None` when there is
    /// no rustup home or nothing in it is indexed.
    pub rustup: Option<(String, Pages)>,
}

/// The `*.html` Spotlight indexes in rustup's home, each list most first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Pages {
    /// `rust-docs` pages (a toolchain's `share/doc/rust/html`), per toolchain.
    pub docs: Vec<(String, usize)>,
    /// Every other page, per what it is: `the <component> component` whose manifest
    /// lists it, else the folder it is in.
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
    Some(measure(&home))
}

fn measure(home: &Path) -> Result<Spotlit, String> {
    let crates = mdfind(home, "*.rmeta")?;
    let mut builds = Tally::default();
    for path in crates.split('\0').filter(|p| !p.is_empty()) {
        builds.add(tilde(home, &build_root(path)));
    }
    let rustup: PathBuf = std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".rustup"), Into::into);
    // Spotlight answers with real paths: query and strip the canonical folder.
    let rustup_home = match std::fs::canonicalize(&rustup) {
        Ok(real) if real.is_dir() => {
            let listed = mdfind(&real, "*.html")?;
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

    /// Most first, ties by name.
    fn most_first(mut self) -> Vec<(String, usize)> {
        self.0
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.0
    }
}

/// Sorts Spotlight's `*.html` paths under rustup's home `root`: a page under a
/// toolchain's `share/doc/rust/html` is that toolchain's `rust-docs`; any other is named
/// by `listed(toolchain, path in it)` — the component whose manifest lists it — else by
/// its folder (`shown` renders a folder for the row).
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

/// A toolchain's component manifests (`lib/rustlib/manifest-<component>`), each with its
/// `file:`/`dir:` entries; unreadable ones are left out, so their files go by folder.
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

/// A component's text is optional classification evidence, so a malformed
/// manifest must not block the doctor or consume unbounded memory. Prove the
/// opened handle regular, refusing final links and nonblocking on Unix.
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

/// The component whose manifest lists `rest` (a path inside the toolchain), as a
/// `file:` entry or under a `dir:` one.
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

/// `mdfind -onlyin <dir>` for files named `pattern`: the paths, NUL-separated.
fn mdfind(dir: &Path, pattern: &str) -> Result<String, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/bin/mdfind");
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

/// The folder a crate file's build lives in, as a person would delete or rename it: a
/// toolchain's sysroot (the path before `/lib/rustlib/`), else the first `target` folder
/// on the path (a checkout's, with every lane inside it), else the first `build` folder
/// (another repository's, as Trust's bootstrap writes), else the folder above the
/// profile that holds `deps/`, else the file's own folder.
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

/// `path` with the home written `~`.
fn tilde(home: &Path, path: &str) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// The three largest of `counts`, `<name> <n>` each, then how many `more` there are.
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

/// The row's fact and whether it is clean, pure over the count: clean when Spotlight
/// indexes no crate builds and no `rust-docs`; other HTML in rustup's home is named and
/// never makes the row a note (no remedy removes a component's own files).
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
            "{} rust-docs pages indexed in {home} ({}): `aterm init` removes rust-docs",
            total(&pages.docs),
            largest(&pages.docs, "toolchains")
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

    /// A crate file is counted under the folder that would be deleted or renamed: a
    /// checkout's `target` (every lane inside it), another repository's `build`, a
    /// toolchain's sysroot, else the folder above its profile.
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
            // A crate named like a folder is not one.
            ("/U/a/w/target", "/U/a/w"),
        ] {
            assert_eq!(build_root(path), root, "{path}");
        }
        assert_eq!(tilde(Path::new("/U/a"), "/U/a/x"), "~/x");
        assert_eq!(tilde(Path::new("/U/a"), "/U/ab/x"), "/U/ab/x");
    }

    /// Only a toolchain's `share/doc/rust/html` counts as `rust-docs`; any other page in
    /// rustup's home is named by the component whose manifest lists it (a `file:` or
    /// under a `dir:`), else by its folder.
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
            // Named like the docs folder, but beside it: not rust-docs.
            "/U/a/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html.html",
            "/U/a/.rustup/toolchains/1.97.1-aarch64-apple-darwin/share/doc/rust/COPYRIGHT.html",
            "/U/a/.rustup/tmp/report.html",
            "/U/a/.rustup/tmp/other.html",
            // Outside the root (Spotlight answered another spelling): its folder.
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

    /// No crate builds and no rust-docs is the one clean answer, which still names any
    /// other indexed HTML; build folders name the three largest and how many more,
    /// rust-docs name `aterm init`, and an unmeasured count says why.
    #[test]
    fn the_row_names_what_spotlight_holds_and_the_remedy() {
        let clean = Spotlit {
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
        // This Mac after `aterm init`: rustc's own license pages, nothing to remove.
        let licenses = Spotlit {
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
             `aterm init` removes rust-docs"
        );
        assert_eq!(
            lines[3],
            "2 other HTML files indexed in ~/.rustup, not rust-docs: \
             the rustc-aarch64-apple-darwin component 2"
        );
        let docs_only = Spotlit {
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
}
