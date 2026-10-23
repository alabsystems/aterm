// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless Chromium page runner for wasm verification lanes.

use std::path::{Path, PathBuf};

use aterm_json::Value;

#[cfg(unix)]
pub(crate) use crate::browser::chromium::Browser;
#[cfg(unix)]
pub(crate) use crate::browser::page::Chrome as Page;

/// Why a run gave no answer.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The machine, not the tree: no browser, or it did not start or answer
    /// its first call. Once a browser has answered, nothing it does is this.
    Unavailable(String),
    /// A finding: the page failed to load, threw, or hung, or the browser
    /// died or stopped answering under it.
    Failed(String),
}

/// The browser the drivers run in, found before anything is built.
#[cfg(unix)]
pub(crate) fn browser() -> Result<Browser, Refused> {
    crate::browser::chromium::pinned().map_err(Refused::Unavailable)
}

impl Refused {
    /// The sentence, whichever kind (a test's failure message).
    #[cfg(all(test, unix))]
    pub(crate) fn why(&self) -> &str {
        match self {
            Refused::Unavailable(why) | Refused::Failed(why) => why,
        }
    }

    /// As a lane's verdict: no browser is COULD NOT RUN naming what provides
    /// one, a page that failed is a FAIL.
    pub(crate) fn verdict(self, what: &str) -> crate::perf::LaneRun {
        match self {
            Refused::Unavailable(why) => crate::perf::LaneRun::could_not_run(
                format!("{what} runs in the pinned chrome-headless-shell, and none ran: {why}"),
                browser_fix(),
            ),
            Refused::Failed(why) => crate::perf::LaneRun::fail(format!("{what}: {why}")),
        }
    }
}

/// What provides the browser the drivers run in: the `chrome-headless-shell`
/// row's remedy.
pub(crate) fn browser_fix() -> String {
    aterm_uds::host::tool("chrome-headless-shell").remedy()
}

/// A run's answer and its page's console.
#[derive(Debug)]
pub(crate) struct Ran {
    /// What `run` answered, by value.
    pub value: Value,
    /// Console lines captured in order.
    pub console: Vec<String>,
}

impl Ran {
    /// The console, one line each.
    pub(crate) fn log(&self) -> String {
        let mut out = self.console.join("\n");
        out.push('\n');
        out
    }
}

/// No browser off unix: verify drives one over fds 3 and 4.
#[cfg(not(unix))]
pub(crate) enum Browser {}

#[cfg(not(unix))]
pub(crate) fn browser() -> Result<Browser, Refused> {
    Err(Refused::Unavailable(
        "verify drives a browser on unix only".to_string(),
    ))
}

#[cfg(not(unix))]
pub(crate) enum Page {}

#[cfg(not(unix))]
impl Page {
    pub(crate) fn start(browser: &Browser, _root: &Path, _scratch: &Path) -> Result<Self, Refused> {
        match *browser {}
    }

    pub(crate) fn product(&self) -> &str {
        match *self {}
    }

    pub(crate) fn run(&mut self, _module: &str, _args: &Value) -> Result<Ran, Refused> {
        match *self {}
    }
}

/// The perf drivers beside this crate (`perf/wasm/*.mjs`).
pub(crate) fn drivers_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/wasm")
}

/// The font every driver's terminal is built on: the tree's, so no answer
/// depends on the box.
pub(crate) fn font(root: &Path) -> PathBuf {
    root.join("crates/aterm-render/assets/DejaVuSansMono.ttf")
}

/// Where [`lay_drivers`] puts the drivers and the font in a site.
pub(crate) const DRIVERS: &str = "drivers";
pub(crate) const FONT: &str = "font.ttf";

/// Copy the perf drivers into `site/drivers/` and the font to
/// `site/font.ttf`.
pub(crate) fn lay_drivers(root: &Path, site: &Path) -> Result<(), String> {
    let into = site.join(DRIVERS);
    std::fs::create_dir_all(&into)
        .map_err(|e| format!("could not create {}: {e}", into.display()))?;
    let from = drivers_dir();
    for entry in std::fs::read_dir(&from).map_err(|e| format!("{}: {e}", from.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "mjs") {
            let name = path.file_name().unwrap_or_default();
            std::fs::copy(&path, into.join(name))
                .map_err(|e| format!("could not copy {}: {e}", path.display()))?;
        }
    }
    let font = font(root);
    std::fs::copy(&font, site.join(FONT))
        .map_err(|e| format!("could not copy {}: {e}", font.display()))?;
    Ok(())
}

/// A test's browser serving `root`, its profile under `root`. No browser is
/// the machine's answer, never a finding: it panics with verify's COULD NOT
/// RUN sentinel, naming what provides one.
#[cfg(all(test, unix))]
pub(crate) fn start_for_test(root: &Path) -> Page {
    match browser().and_then(|b| Page::start(&b, root, &root.join(".browser"))) {
        Ok(page) => page,
        Err(Refused::Unavailable(why)) => panic!(
            "{}: the generated glue runs in a headless Chromium, and none started ({why}); {}",
            crate::libtest::COULD_NOT_RUN_SENTINEL,
            browser_fix()
        ),
        Err(Refused::Failed(why)) => panic!("the browser failed: {why}"),
    }
}
