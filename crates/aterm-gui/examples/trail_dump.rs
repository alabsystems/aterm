// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab trail-diff`'s dump (`docs/design/PRISM-GLASS.md` §15.1 P0.4, §15.7.2): runs each
//! `.trail` script on the app's own frame (`aterm_gui::TrailRig`, the headless App drawn through
//! the live present onto a scripted CPU surface at an injected clock) and prints every frame's
//! stream, pixel, deadline, sound and state words (`support/trail_script.rs` has the grammar and
//! the format). Built with `--profile trail-diff`, which keeps the debug seams; by hand it is
//! `targo --unverified run -p aterm-gui --features bench-support --example trail_dump -- <script.trail>...`,
//! run from the checkout's root.

#[cfg(all(unix, debug_assertions))]
mod rig {
    pub(crate) use aterm_gui::{TrailFrame, TrailRig};

    /// The font every dump draws, so glyph pixels never move with the host: the repo's fixture
    /// face in the checkout the dump runs in, the working directory, where `lab trail-diff` runs
    /// both builds' dumps (the head's checkout, whose scripts they run).
    pub(crate) fn fixture_font() -> Result<std::path::PathBuf, String> {
        let checkout =
            std::env::current_dir().map_err(|e| format!("the working directory: {e}"))?;
        Ok(checkout.join("crates/aterm-render/tests/fixtures/jetbrains-mono.ttf"))
    }
}

#[cfg(all(unix, debug_assertions))]
#[path = "support/trail_script.rs"]
mod trail_script;

#[cfg(all(unix, debug_assertions))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code =
        trail_script::dump_main(&args, &mut std::io::stdout().lock(), &mut std::io::stderr());
    std::process::exit(code);
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!(
        "trail_dump: the dump's seams compile into a Unix build with debug assertions (--profile trail-diff)"
    );
    std::process::exit(2);
}
