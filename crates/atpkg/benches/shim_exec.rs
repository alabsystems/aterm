// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The shim pricing instrument — what the `bin/` indirection costs on EVERY invocation of
//! every managed tool.
//!
//! # What is being priced
//!
//! `bin/` is the only directory the generated shell hook puts on `PATH`
//! (`hooks.rs`), so every typed `ay`, `trust`, `ty`… goes through a file this crate
//! wrote. That file is a `/bin/sh` script whose entire body is one `exec`
//! (`platform::sh_shim_content`) — and `/bin/sh` on macOS is bash, so a whole shell
//! interpreter is created, initialized and torn down to issue one `execve`.
//!
//! It cannot simply be deleted, and the two obvious deletions are both refused in the
//! source: a SYMLINK breaks Trust's `targo`, which refuses to authenticate when
//! `current_exe` is a symlink or is not its own `canonicalize()` (that is why the shim
//! stopped being a symlink), and putting `<build>/bin` on `PATH` directly would expose
//! every binary in a build, defeating the `SENSITIVE_SHIMS` deny-list that exists to stop
//! a tool honestly or maliciously named `git`/`sudo` from becoming reachable.
//!
//! So the question is not "can the indirection go" but "how much does THIS indirection
//! cost", and nothing in the crate could answer it. This bench answers it, and it is
//! deliberately written to stay valid across a change of dialect: it SNIFFS what kind of
//! shim the crate installed and prints it beside the number, so a future compiled stub is
//! measured by the same instrument rather than silently invalidating it.
//!
//! # Method
//!
//! Both legs spawn the SAME target through the SAME path shape, so the only difference is
//! the indirection:
//!
//! * DIRECT — spawn `<build>/bin/<tool>` itself.
//! * SHIM — spawn `<prefix>/bin/<tool>`, the file `activate::install_shims` really wrote
//!   (never a hand-rolled imitation, which is how a bench drifts from the product).
//!
//! # Reach guards (both directions)
//!
//! A failing shim is FAST, so "it got quicker" is exactly what a broken fixture looks
//! like. Before timing anything, the shim is run once with its output captured and must
//! have reached the target AND forwarded its arguments; it must not be a symlink (the
//! property the exec stub exists to preserve); and `platform::resolve_shim` must recover
//! the target from it, which is the same answer `active_builds`, `prune_stale_shims` and
//! gc are all built on. If any of those fail the bench aborts instead of reporting.

#[cfg(unix)]
mod bench {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use atpkg::Layout;

    /// The tool name the fixture exposes. Must pass `store::shim_allowed` (it is not a
    /// sensitive command), or `install_shims` would correctly refuse to write it.
    const TOOL: &str = "atpkgbenchtool";
    /// What the target prints, so the reach guard can prove argv actually arrived.
    const MARKER: &str = "atpkg-shim-ok";

    /// A real system executable to stand in for a managed tool. It must be a genuine
    /// binary: standing the target up as a script would put an interpreter in the DIRECT
    /// baseline too and quietly cancel the very cost being measured.
    fn system_echo() -> Option<PathBuf> {
        ["/bin/echo", "/usr/bin/echo"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-bench-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("bench scratch dir");
        d
    }

    /// Spawn `program arg` `n` times, discarding output, and return the total wall time.
    /// `status()` is the whole round-trip a shell pays: fork/posix_spawn, exec, wait.
    fn spawn_loop(program: &Path, n: u32) -> Duration {
        let t = Instant::now();
        for _ in 0..n {
            let st = Command::new(program)
                .arg(MARKER)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn");
            assert!(st.success(), "{} exited {st}", program.display());
        }
        t.elapsed()
    }

    /// `#!` ⇒ the interpreted dialect; anything else ⇒ a native image. Reported rather
    /// than asserted so this instrument survives the fix it exists to justify.
    fn dialect(shim: &Path) -> &'static str {
        match std::fs::read(shim) {
            Ok(b) if b.starts_with(b"#!") => "interpreted (#! script)",
            Ok(_) => "native executable",
            Err(_) => "unreadable",
        }
    }

    pub fn run() {
        let Some(echo) = system_echo() else {
            println!("atpkg shim_exec: no system `echo` binary found — skipping");
            return;
        };
        let n: u32 = std::env::var("ATPKG_BENCH_SHIM_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);

        let dir = scratch();
        let layout = Layout {
            prefix: dir.join("prefix"),
        };
        let build_dir = layout.build_dir(TOOL, 1);
        let build_bin = build_dir.join("bin");
        std::fs::create_dir_all(&build_bin).expect("build bin dir");
        let target = build_bin.join(TOOL);
        // A SYMLINK to the system binary, not a copy: the exec'd image is then the real
        // system executable (no re-signed duplicate to explain on macOS), and both legs
        // resolve the identical path, so the comparison isolates the indirection alone.
        std::os::unix::fs::symlink(&echo, &target).expect("target link");

        // The REAL shim writer — the product's own code path, not an imitation.
        let refused = atpkg::install_shims(
            &layout,
            &build_dir,
            &[TOOL.to_string()],
            atpkg::Aliases::Alab,
        )
        .expect("install_shims");
        assert!(refused.is_empty(), "the bench tool name must be admissible");
        let shim = layout.bin_dir().join(TOOL);

        // ---- reach guards -------------------------------------------------------
        let meta = std::fs::symlink_metadata(&shim).expect("the shim exists");
        assert!(
            !meta.file_type().is_symlink(),
            "REACH GUARD: the shim must not be a symlink — that is the property the exec \
             stub exists to preserve (targo refuses a symlinked current_exe)"
        );
        assert_eq!(
            atpkg::platform::resolve_shim(&shim).as_deref(),
            Some(target.as_path()),
            "REACH GUARD: the shim must resolve to the target — this is the same answer \
             active_builds / prune_stale_shims / gc are built on"
        );
        let out = Command::new(&shim)
            .arg(MARKER)
            .output()
            .expect("the shim runs");
        assert!(out.status.success(), "REACH GUARD: the shim must succeed");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            MARKER,
            "REACH GUARD: the shim must reach the target AND forward argv — a shim that \
             fails fast would otherwise be reported as a saving"
        );

        // ---- measurement --------------------------------------------------------
        // Three rounds, median, warm: the first spawn of anything pays page-cache and
        // dyld-cache costs that belong to neither leg.
        let _ = spawn_loop(&target, 20);
        let _ = spawn_loop(&shim, 20);
        let mut direct: Vec<Duration> = (0..3).map(|_| spawn_loop(&target, n)).collect();
        let mut through: Vec<Duration> = (0..3).map(|_| spawn_loop(&shim, n)).collect();
        direct.sort_unstable();
        through.sort_unstable();
        let d_us = direct[1].as_secs_f64() * 1e6 / f64::from(n);
        let s_us = through[1].as_secs_f64() * 1e6 / f64::from(n);
        let overhead = s_us - d_us;

        println!("atpkg shim_exec — per managed-tool invocation");
        println!("  shim dialect : {}", dialect(&shim));
        println!("  target       : {}", echo.display());
        println!("  n            : {n} invocations x 3 rounds (median)");
        println!();
        println!("  direct exec of the target      {d_us:>8.0} us");
        println!("  through the atpkg bin/ shim    {s_us:>8.0} us");
        println!(
            "  indirection                    {overhead:>8.0} us  ({:.2}x)",
            if d_us > 0.0 { s_us / d_us } else { 0.0 }
        );
        println!();
        // Sized honestly, because this is a per-invocation cost and nothing else: it is
        // noise for a person typing commands and it is real for a loop.
        for invocations in [100u32, 1_000, 10_000] {
            println!(
                "  {invocations:>6} invocations  =>  {:>6.2} s of indirection",
                f64::from(invocations) * overhead / 1e6
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn main() {
    #[cfg(unix)]
    bench::run();
    #[cfg(not(unix))]
    println!(
        "atpkg shim_exec: the `.cmd` dialect has the same shape (cmd.exe startup per \
         invocation) but is not measured here — Unix only."
    );
}
