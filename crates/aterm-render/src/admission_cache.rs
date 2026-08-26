// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent verdict cache for [`fontdue_admissible`] — the named largest
//! remaining cold-launch term after the lazy-parse change: the admission
//! replay's cmap walk costs ~203 ms on `Hiragino Sans GB.ttc` alone (measured,
//! see `fontdue_admissible`'s table), and its verdict is a pure function of
//! the FILE BYTES, re-derived identically on every launch of every process.
//!
//! One line per verdict, keyed `(path, collection index, mtime_ns, size)`:
//! the standard font-cache identity (fontconfig's), which trades the
//! same-mtime-same-size overwrite corner for never hashing 23 MB per face —
//! hashing would give back a third of the win. That corner is stated, not
//! hidden, and it is why BOTH lines of defence stay: a stale `true` meets
//! [`Renderer::retire_unparsable_fallback`] exactly as a wrong live verdict
//! would, and a stale `false` merely hides one fallback face until the file's
//! metadata next moves.
//!
//! FAIL-SAFE IN EVERY DIRECTION: a missing/corrupt/unwritable cache is a slow
//! launch, never a wrong one — every error path falls through to the real
//! walk, and a malformed line invalidates only itself. The file lives under
//! [`aterm_types::dirs::cache_dir`], whose contract is "deletable at any
//! time" (macOS purges Caches under disk pressure); rewrites go through a
//! sibling tempfile + rename so a torn write can never half-exist, and two
//! processes racing the rewrite is last-writer-wins on RE-DERIVABLE bytes.
//!
//! NO EVICTION, deliberately: an entry for a since-deleted font is dead
//! weight, but the file is bounded by |faces ever seen| at ~80 bytes each
//! (tens of KB on a font-hoarder's machine), lives where the OS already
//! purges under pressure, and every `REPLAY_VERSION` bump orphans it whole.
//! Pruning would cost a stat per entry per flush to reclaim kilobytes.
//!
//! `REPLAY_VERSION` is stamped in the filename: the verdicts encode
//! `fontdue_admissible`'s replay of fontdue 0.9.3's failure modes, so any
//! change to that logic (or a fontdue bump) must bump the version and thereby
//! orphan every old verdict rather than trust one across the change.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Version of the ADMISSION LOGIC these verdicts replay (not a file-format
/// version, though it doubles as one): bump alongside any change to
/// [`super::fontdue_admissible`] or to the pinned fontdue the replay mirrors.
const REPLAY_VERSION: u32 = 1;

/// `(path, collection index)` → `(mtime_ns, size, verdict)`.
type Verdicts = HashMap<(String, u32), (u128, u64, bool)>;

struct Cache {
    file: Option<PathBuf>,
    verdicts: Verdicts,
    /// Verdicts recorded since the last flush; batched so a cold discovery
    /// scan over N faces rewrites the file once, not N times.
    dirty: usize,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| {
        // Unit tests share this global across the test binary and MUST NOT
        // write scratch entries into the user's real cache file (it happened:
        // a probe.bin line landed in ~/Library/Caches). Redirect to a
        // per-process scratch path; integration tests exercising real fonts
        // via the non-test-compiled lib still share the real file, where
        // every entry they write is one production itself would.
        #[cfg(test)]
        let file = Some(
            std::env::temp_dir()
                .join(format!("aterm-admission-cache-{}", std::process::id()))
                .join(format!("global.v{REPLAY_VERSION}")),
        );
        #[cfg(not(test))]
        let file = aterm_types::dirs::cache_dir().map(|d| {
            d.join("aterm")
                .join(format!("font-admission.v{REPLAY_VERSION}"))
        });
        let verdicts = file.as_deref().map(load).unwrap_or_default();
        Mutex::new(Cache {
            file,
            verdicts,
            dirty: 0,
        })
    })
}

/// Parse the cache file; any unreadable file or malformed line yields only
/// misses (fail-safe: a miss is a re-derivation, never a wrong verdict).
fn load(path: &Path) -> Verdicts {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Verdicts::default();
    };
    let mut out = Verdicts::default();
    for line in text.lines() {
        // `index \t mtime_ns \t size \t verdict \t path` — path LAST so a
        // path containing the separator only corrupts its own line.
        let mut parts = line.splitn(5, '\t');
        let (Some(index), Some(mtime), Some(size), Some(verdict), Some(path)) = (
            parts.next().and_then(|s| s.parse::<u32>().ok()),
            parts.next().and_then(|s| s.parse::<u128>().ok()),
            parts.next().and_then(|s| s.parse::<u64>().ok()),
            parts.next().and_then(|s| match s {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            }),
            parts.next().filter(|p| !p.is_empty()),
        ) else {
            continue;
        };
        out.insert((path.to_string(), index), (mtime, size, verdict));
    }
    out
}

/// Atomic best-effort rewrite: tempfile beside the target, then rename. Every
/// error is swallowed by design — an unwritable cache is a slow launch.
fn flush(cache: &mut Cache) {
    let Some(file) = cache.file.clone() else {
        return;
    };
    let Some(parent) = file.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let tmp = file.with_extension(format!("tmp.{}", std::process::id()));
    let mut body = String::new();
    for ((path, index), (mtime, size, verdict)) in &cache.verdicts {
        body.push_str(&format!(
            "{index}\t{mtime}\t{size}\t{}\t{path}\n",
            u8::from(*verdict)
        ));
    }
    let wrote = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(body.as_bytes()))
        .is_ok();
    if wrote && std::fs::rename(&tmp, &file).is_ok() {
        cache.dirty = 0;
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The file identity the verdict is keyed to, or `None` when the path cannot
/// be statted (then there is nothing safe to key on — skip the cache).
fn identity(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((mtime, meta.len()))
}

/// Cached admission: return the recorded verdict when `(path, index)` matches
/// the file's current `(mtime, size)`, else compute via `walk`, record, and
/// batch-flush. `walk` is the real [`super::fontdue_admissible`] — passed in
/// so this module stays a pure cache with no opinion about admission itself.
///
/// The identity is statted HERE while the walked bytes were read by the
/// caller — a file swapped in that window records the old bytes' verdict
/// under the new identity. That is the module-doc identity corner in one
/// more costume, with the same two backstops.
pub(crate) fn admissible_cached(path: &str, index: u32, walk: impl FnOnce() -> bool) -> bool {
    let Some((mtime, size)) = identity(Path::new(path)) else {
        return walk();
    };
    let lock = cache();
    {
        let guard = lock.lock().expect("font admission cache lock");
        if let Some((m, s, verdict)) = guard.verdicts.get(&(path.to_string(), index))
            && *m == mtime
            && *s == size
        {
            return *verdict;
        }
    }
    // The walk runs UNLOCKED: it is the ~200 ms operation this cache exists
    // to skip, and the discovery scan runs it from parallel workers.
    let verdict = walk();
    let mut guard = lock.lock().expect("font admission cache lock");
    guard
        .verdicts
        .insert((path.to_string(), index), (mtime, size, verdict));
    guard.dirty += 1;
    // Flush on every batch boundary rather than every insert: a cold scan
    // admits faces in bursts, and the last burst's flush persists the set.
    // (A crash between bursts loses only re-derivable verdicts.)
    if guard.dirty >= 4 {
        flush(&mut guard);
    }
    verdict
}

/// Persist any unflushed verdicts (call once when a build/scan completes, so
/// tail entries smaller than the batch survive the process).
pub(crate) fn flush_pending() {
    let mut guard = cache().lock().expect("font admission cache lock");
    if guard.dirty > 0 {
        flush(&mut guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_file(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aterm-admission-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join(name)
    }

    #[test]
    fn roundtrip_and_malformed_lines_only_hurt_themselves() {
        let file = scratch_file("roundtrip");
        let mut c = Cache {
            file: Some(file.clone()),
            verdicts: Verdicts::default(),
            dirty: 0,
        };
        c.verdicts.insert(("/a/b.ttf".into(), 0), (123, 456, true));
        c.verdicts.insert(("/c/d.ttc".into(), 2), (789, 12, false));
        c.dirty = 2;
        flush(&mut c);
        assert_eq!(c.dirty, 0, "flush clears the dirty count");
        // Corrupt one line by hand; the other survives.
        let mut text = std::fs::read_to_string(&file).expect("read back");
        text.push_str("not\ta\tvalid\tline\n");
        text.push_str("9\tnot-a-number\t1\t1\t/e/f.otf\n");
        std::fs::write(&file, text).expect("rewrite");
        let loaded = load(&file);
        assert_eq!(loaded.len(), 2, "malformed lines are dropped alone");
        assert_eq!(loaded[&("/a/b.ttf".into(), 0)], (123, 456, true));
        assert_eq!(loaded[&("/c/d.ttc".into(), 2)], (789, 12, false));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn identity_mismatch_is_a_miss_and_the_walk_decides() {
        // A real file whose identity we control.
        let probe = scratch_file("probe.bin");
        std::fs::write(&probe, b"0123456789").expect("probe");
        let path = probe.to_string_lossy().into_owned();
        // Seed the global cache with a WRONG identity for this path: the
        // lookup must miss and the walk's verdict must win and re-key.
        {
            let mut guard = cache().lock().expect("lock");
            guard.verdicts.insert((path.clone(), 7), (1, 1, true));
        }
        let walked = std::cell::Cell::new(false);
        let verdict = admissible_cached(&path, 7, || {
            walked.set(true);
            false
        });
        assert!(walked.get(), "stale identity re-derives");
        assert!(!verdict, "the walk's verdict wins");
        // Second call: hit, no walk.
        let verdict = admissible_cached(&path, 7, || unreachable!("cache hit must not walk"));
        assert!(!verdict, "false verdicts cache too");
        let _ = std::fs::remove_file(&probe);
    }

    /// Manual measurement probe (the repo's law: never re-estimate, re-measure).
    /// Run: `cargo test -p aterm-render --lib admission_probe -- --ignored --nocapture`
    #[test]
    #[ignore = "manual probe — measures the real system font, prints timings"]
    fn admission_probe_cold_vs_warm() {
        let path = "/System/Library/Fonts/Hiragino Sans GB.ttc";
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("probe: {path} not present on this machine — nothing measured");
            return;
        };
        let t0 = std::time::Instant::now();
        let cold =
            super::admissible_cached(path, 0, || super::super::fontdue_admissible(&bytes, 0));
        let cold_ms = t0.elapsed().as_secs_f64() * 1e3;
        let t1 = std::time::Instant::now();
        let warm = super::admissible_cached(path, 0, || unreachable!("second call must hit"));
        let warm_ms = t1.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "probe: {path}: cold(walk)={cold_ms:.1} ms verdict={cold}; warm(hit)={warm_ms:.3} ms verdict={warm}"
        );
        assert_eq!(cold, warm);
    }

    #[test]
    fn an_unstattable_path_skips_the_cache_entirely() {
        let verdict = admissible_cached("/definitely/not/here.ttf", 0, || true);
        assert!(verdict, "no identity -> the walk decides, nothing recorded");
        let guard = cache().lock().expect("lock");
        assert!(
            !guard
                .verdicts
                .contains_key(&("/definitely/not/here.ttf".to_string(), 0)),
            "nothing safe to key on, nothing keyed"
        );
    }
}
