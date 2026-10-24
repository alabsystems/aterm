// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tooling cache's share of the reclaim.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Planned, RECLAIM_IDLE, Unused};

const DEPTH: usize = 2;
const STAGE: &str = ".provision-reclaim-";

/// The tooling cache under `home`, on macOS's rule (`aterm_uds::host::user_cache`):
/// `~/Library/Caches/aterm/tooling.noindex`.
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    aterm_uds::host::user_cache("macos", Some(home), None, aterm_uds::host::TOOLING)
        .unwrap_or_else(|| home.join("Library/Caches/aterm/tooling.noindex"))
}

fn last_used(path: &Path, depth: usize) -> Option<SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_file()
        && let Ok(read) = meta.accessed()
    {
        newest = newest.max(read);
    }
    if meta.is_dir() && depth > 0 {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            if let Some(used) = last_used(&entry.path(), depth - 1) {
                newest = newest.max(used);
            }
        }
    }
    Some(newest)
}

// Scratch rows are reclaimed only when both EXEC_STAMP and RAN exist as regular files, since
// elapsed time or a reboot cannot prove an image ran.
fn why(entry: &Path, now: SystemTime) -> Option<Unused> {
    let name = entry.file_name()?.to_str()?;
    if !std::fs::symlink_metadata(entry).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    if let Some(original) = name.strip_prefix(STAGE) {
        return (!original.starts_with(aterm_uds::host::TOOLING_SCRATCH))
            .then_some(Unused::Unfinished);
    }
    if name.starts_with(aterm_uds::host::TOOLING_SCRATCH) {
        let given_up = [aterm_uds::deadline::EXEC_STAMP, aterm_uds::deadline::RAN]
            .iter()
            .all(|stamp| std::fs::symlink_metadata(entry.join(stamp)).is_ok_and(|m| m.is_file()));
        return given_up.then_some(Unused::GivenUp);
    }
    let idle = now
        .duration_since(last_used(entry, DEPTH)?)
        .unwrap_or(Duration::ZERO);
    let after = RECLAIM_IDLE;
    (idle >= after).then_some(Unused::Tooling { idle, after })
}

fn entries(root: &Path) -> Vec<PathBuf> {
    if !std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// What [`reclaim`] would take from the tooling cache `root` at `now`, read and never acted on.
#[must_use]
pub fn plan(root: &Path, now: SystemTime) -> Vec<Planned> {
    entries(root)
        .into_iter()
        .filter_map(|entry| {
            why(&entry, now).map(|why| Planned {
                path: entry.clone(),
                why,
                paths: vec![entry],
                held: false,
            })
        })
        .collect()
}

/// Remove what nobody uses from the tooling cache `root` at `now` ([`why`]), `record` asked first
/// for each (`false` keeps it); each removed whole is returned with why.
pub fn reclaim(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let mut removed = Vec::new();
    for entry in entries(root) {
        let Some(why) = why(&entry, now) else {
            continue;
        };
        if !record(&entry, why) {
            continue;
        }
        let stage = if matches!(why, Unused::Unfinished | Unused::GivenUp) {
            entry.clone()
        } else {
            let Some(name) = entry.file_name() else {
                continue;
            };
            let mut staged = std::ffi::OsString::from(STAGE);
            staged.push(name);
            let stage = root.join(staged);
            if std::fs::rename(&entry, &stage).is_err() {
                continue;
            }
            stage
        };
        match std::fs::remove_dir_all(&stage) {
            Ok(()) => removed.push((entry, why)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed.push((entry, why)),
            Err(_) => {}
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[cfg(unix)]
    fn file(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// Set `path`'s access and modification times to `at`, without following a link.
    #[cfg(unix)]
    fn stamp(path: &Path, at: SystemTime) {
        let file = std::fs::File::open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
            .unwrap();
    }

    /// Every file and folder of `dir`, to [`DEPTH`] below it, last touched at `at`.
    #[cfg(unix)]
    fn age_tree(dir: &Path, at: SystemTime) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                age_tree(&entry.path(), at);
            }
            stamp(&entry.path(), at);
        }
        stamp(dir, at);
    }

    /// The idle rule over the tooling cache: a row unused a month goes, one a run used
    /// stays (an exec's access time is use), a completed row's stage is finished, and a
    /// link is never followed. Scratches with no exec stamp stay even when old or already
    /// staged: no age proves their copied image finished running. NEGATIVE CONTROL: the same completed
    /// row with one file's access time set yesterday (as an exec sets it) stays.
    #[cfg(unix)]
    #[test]
    fn a_tool_unused_a_month_goes_and_one_a_run_used_stays() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let month = now - RECLAIM_IDLE - DAY;
        for row in ["pwsh-7.6.7", "fish-4.9.3", "binaryen-133"] {
            file(&root.join(row).join("bin/tool"));
            file(&root.join(row).join("stamp.json"));
            age_tree(&root.join(row), month);
        }
        let fish = root.join("fish-4.9.3/bin/tool");
        std::fs::File::open(&fish)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(now - DAY))
            .unwrap();
        file(&root.join(".provision-pwsh-abc/download/x.tar.gz"));
        age_tree(&root.join(".provision-pwsh-abc"), now - 2 * DAY);
        let unknown = [
            ".provision-fish-unknown",
            ".provision-reclaim-.provision-fish-staged-unknown",
        ];
        for scratch in unknown {
            file(&root.join(scratch).join("tree/bin/fish"));
            age_tree(&root.join(scratch), month);
        }
        file(&root.join(".provision-fish-new/src/Cargo.toml"));
        file(&root.join(".provision-reclaim-old/bin/tool"));
        let outside = dir.path().join("elsewhere");
        file(&outside.join("bin/tool"));
        age_tree(&outside, month);
        std::os::unix::fs::symlink(&outside, root.join("linked-1.0")).unwrap();

        let planned: Vec<(String, Unused)> = plan(&root, now)
            .into_iter()
            .map(|p| {
                (
                    p.path.file_name().unwrap().to_string_lossy().into_owned(),
                    p.why,
                )
            })
            .collect();
        let names: Vec<&str> = planned.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [".provision-reclaim-old", "binaryen-133", "pwsh-7.6.7"],
            "{planned:#?}"
        );
        assert!(planned.iter().any(|(n, w)| n == "pwsh-7.6.7"
            && matches!(w, Unused::Tooling { after, .. } if *after == RECLAIM_IDLE)));
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |p, why| {
            asked.push((p.to_path_buf(), why));
            !p.ends_with("binaryen-133")
        });
        assert_eq!(asked.len(), 3, "{asked:#?}");
        assert!(
            asked.iter().all(|(path, _)| {
                !path.ends_with(".provision-pwsh-abc")
                    && unknown.iter().all(|scratch| !path.ends_with(scratch))
            }),
            "scratch retention precedes the intent callback: {asked:#?}"
        );
        let gone: Vec<&Path> = removed.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(gone.len(), 2, "{removed:#?}");
        for kept in [
            "fish-4.9.3/bin/tool",
            "binaryen-133/bin/tool",
            ".provision-fish-new",
            ".provision-pwsh-abc/download/x.tar.gz",
        ] {
            assert!(root.join(kept).exists(), "{kept} stays");
        }
        for scratch in unknown {
            assert_eq!(
                std::fs::read(root.join(scratch).join("tree/bin/fish")).unwrap(),
                b"x",
                "the actual uncertain image stays at its original path"
            );
            assert!(
                !root.join(format!("{STAGE}{scratch}")).exists(),
                "an uncertain image is not renamed either"
            );
        }
        for went in ["pwsh-7.6.7", ".provision-reclaim-old"] {
            assert!(!root.join(went).exists(), "{went} goes");
        }
        assert!(
            !root.join(".provision-reclaim-pwsh-7.6.7").exists(),
            "the stage went with it"
        );
        assert!(
            outside.join("bin/tool").exists(),
            "a link is never followed"
        );
        assert!(root.join("linked-1.0").exists(), "nor removed");
    }

    /// Only an actual run proof admits a given-up scratch. Old, malformed and pre-reboot
    /// exec stamps without proof stay; so do live scratches and non-file/link markers.
    #[cfg(unix)]
    #[test]
    fn a_given_up_scratch_goes_only_after_its_run_is_proven() {
        use aterm_uds::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN};
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("tooling.noindex");
        let now = SystemTime::now();
        let kept = [
            (".provision-fish-spent", Some("")),
            (".provision-fish-rebooted", Some("18446744073709551615")),
            (".provision-fish-fresh", Some("0")),
            (".provision-fish-live", None),
            (".provision-fish-proof-live", None),
            (".provision-fish-proof-dir", Some("0")),
            (".provision-fish-proof-link", Some("0")),
            (".provision-fish-stamp-link", None),
            (".provision-fish-vetoed", Some("0")),
        ];
        let proven = ".provision-fish-proven";
        for (name, stamp) in kept.into_iter().chain([(proven, Some("0"))]) {
            file(
                &root
                    .join(name)
                    .join("target.noindex/release/build/x/build-script-build"),
            );
            if let Some(at) = stamp {
                std::fs::write(root.join(name).join(EXEC_STAMP), at).unwrap();
            }
            age_tree(&root.join(name), now - RECLAIM_IDLE - DAY);
        }
        std::fs::write(
            root.join(".provision-fish-fresh").join(EXEC_STAMP),
            aterm_uds::process::awake_ms()
                .map(|ms| ms.to_string())
                .unwrap_or_default(),
        )
        .unwrap();
        for name in [
            proven,
            ".provision-fish-proof-live",
            ".provision-fish-stamp-link",
            ".provision-fish-vetoed",
        ] {
            std::fs::write(root.join(name).join(RAN), b"").unwrap();
        }
        std::fs::create_dir(root.join(".provision-fish-proof-dir").join(RAN)).unwrap();
        let outside = dir.path().join("outside-proof");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".provision-fish-proof-link").join(RAN))
            .unwrap();
        std::os::unix::fs::symlink(
            &outside,
            root.join(".provision-fish-stamp-link").join(EXEC_STAMP),
        )
        .unwrap();

        let names = |planned: Vec<Planned>| {
            planned
                .into_iter()
                .map(|p| {
                    assert_eq!(p.why, Unused::GivenUp);
                    p.path.file_name().unwrap().to_string_lossy().into_owned()
                })
                .collect::<Vec<_>>()
        };
        let expected = vec![proven, ".provision-fish-vetoed"];
        assert_eq!(names(plan(&root, now)), expected);
        assert_eq!(
            names(plan(&root, now + RECLAIM_IDLE + FIRST_EXEC_HANG)),
            expected,
            "even another month and 600 seconds cannot replace a run proof"
        );
        let mut asked = Vec::new();
        let removed = reclaim(&root, now, &mut |path, why| {
            assert_eq!(why, Unused::GivenUp);
            asked.push(path.file_name().unwrap().to_string_lossy().into_owned());
            !path.ends_with(".provision-fish-vetoed")
        });
        assert_eq!(asked, expected, "unproved images never reach the callback");
        let gone: Vec<String> = removed
            .iter()
            .map(|(p, why)| {
                assert_eq!(*why, Unused::GivenUp, "{}", p.display());
                p.file_name().unwrap().to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(gone, [proven], "{removed:#?}");
        assert!(!root.join(proven).exists());
        for (name, _) in kept {
            assert_eq!(
                std::fs::read(
                    root.join(name)
                        .join("target.noindex/release/build/x/build-script-build")
                )
                .unwrap(),
                b"x",
                "{name} stays at its original path"
            );
            assert!(!root.join(format!("{STAGE}{name}")).exists());
        }
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn the_root_is_the_resolvers_tooling_cache() {
        let home = Path::new("/Users//someone");
        assert_eq!(
            root(home),
            Path::new("/Users//someone/Library/Caches/aterm/tooling.noindex")
        );
        assert!(plan(&home.join("nowhere-at-all"), SystemTime::now()).is_empty());
    }
}
