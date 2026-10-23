// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One ES module run in a headless Chromium over the DevTools protocol
//! ([`super::cdp`]): the wasm lanes' drivers (`perf/wasm/*.mjs`, the toy's
//! `drive.mjs`) and the glue tests' checks run in V8 inside the browser the
//! site ships to, not in node (docs/DESIGN-init-system-2026-10-20.md, S10).
//!
//! A loopback server ([`super::serve`]) serves a directory the caller laid,
//! into which [`Chrome::start`] writes one blank page, [`PAGE`]. Each run
//! opens a fresh tab at that page, imports one module of the directory and
//! awaits its exported `run(args)`. What the page printed to its console is
//! the run's log, one line each; what `run` answered comes back as JSON. An
//! exception the page throws outside that promise (a timer's, a finalizer's,
//! a rejection nobody handles) fails the run, as it failed node's process.
//!
//! The browser is the caller's ([`crate::wasm_page::browser`]:
//! `ATERM_BROWSER`, else the pinned chrome-headless-shell exactly as `verify
//! provision` placed it). It runs on a private profile with WebGL2 on
//! SwiftShader (nothing here presents, so no GPU is needed) and with
//! `--expose-gc`, so a driver that waits on a finalizer can collect. Only
//! [`Chrome::start`] answers [`Refused::Unavailable`]: once the browser has
//! answered, a tab it cannot open or a pipe it closes is a finding.
//!
//! No budget bounds a run: the wait for its answer is a hang detector that
//! starts again at each line the page prints. A driver that runs for minutes
//! prints between its sections and yields there (a page's console lines are
//! sent between its tasks), and one silent for a minute is hung.

use std::path::Path;
use std::time::Instant;

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
use std::os::fd::OwnedFd;

use super::cdp::{self, Cdp};
use super::{Running, Stop, chromium, log_stdio, log_tail, serve};
use crate::wasm_page::{Ran, Refused};

/// The blank page each run opens, written into the served directory: a
/// document of the server's origin, so the modules it imports are
/// same-origin.
pub(crate) const PAGE: &str = "aterm-page.html";

const PAGE_HTML: &str = "<!doctype html>\n<meta charset=\"utf-8\">\n<title>aterm page</title>\n";

/// The console line a run's page prints once `run` has settled.
const SETTLED: &str = "aterm-page: settled";

/// A headless Chromium serving one directory.
pub(crate) struct Chrome {
    cdp: Cdp,
    running: Running,
    server: serve::Server,
    product: String,
    /// The browser's profile and log, removed once the browser is gone
    /// (fields drop in order).
    _dir: aterm_tempfile::TempDir,
}

impl Chrome {
    /// Launch `browser` on a fresh profile under `scratch`, serving `root`
    /// (into which [`PAGE`] is written).
    pub(crate) fn start(
        browser: &chromium::Browser,
        root: &Path,
        scratch: &Path,
    ) -> Result<Self, Refused> {
        let unavailable = |what: String| Refused::Unavailable(what);
        std::fs::write(root.join(PAGE), PAGE_HTML).map_err(|e| {
            unavailable(format!(
                "could not write {}: {e}",
                root.join(PAGE).display()
            ))
        })?;
        std::fs::create_dir_all(scratch)
            .map_err(|e| unavailable(format!("could not create {}: {e}", scratch.display())))?;
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-page-")
            .tempdir_in(scratch)
            .map_err(|e| {
                unavailable(format!(
                    "no profile directory in {}: {e}",
                    scratch.display()
                ))
            })?;
        let profile = dir.path().join("profile");
        let log = dir.path().join("browser.log");
        let (out, err) = log_stdio(&log).map_err(|stop| match stop {
            Stop::CouldNotRun(why) | Stop::Fail(why) => unavailable(why),
        })?;
        let pipe = || std::io::pipe().map_err(|e| unavailable(format!("no pipe: {e}")));
        let ((cmd_read, cmd_write), (out_read, out_write)) = (pipe()?, pipe()?);
        let mut cmd = LaneCommand::new(&browser.path);
        cmd.args(chromium::launch_args(
            browser,
            &profile,
            chromium::Angle::SwiftShader,
            &["--js-flags=--expose-gc"],
        ))
        .stdin(ChildStdio::Null)
        .stdout(out)
        .stderr(err);
        let child = spawn_with_two_fds(cmd, OwnedFd::from(cmd_read), OwnedFd::from(out_write))
            .map_err(|e| unavailable(format!("could not start {}: {e}", browser.path.display())))?;
        let running = Running(child);
        let mut cdp = Cdp::new(cmd_write, out_read);
        let version = cdp
            .call(None, "Browser.getVersion", json!({}))
            .map_err(|e| {
                unavailable(format!(
                    "{} did not answer: {e}\n{}",
                    browser.path.display(),
                    log_tail(&log)
                ))
            })?;
        let product = version
            .get("product")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        // Isolated: `performance.now()` at 5 µs, not 100 µs, for the bench.
        let server = serve::Server::start_isolated(root).map_err(unavailable)?;
        Ok(Self {
            cdp,
            running,
            server,
            product,
            _dir: dir,
        })
    }

    /// The browser's own name and version (`HeadlessChrome/155.0.8059.84`).
    pub(crate) fn product(&self) -> &str {
        &self.product
    }

    /// The URL of `rel`, a path under the served directory, spelled as a URL
    /// path (any escape already made).
    fn url(&self, rel: &str) -> String {
        format!("{}/{rel}", self.server.origin())
    }

    /// Open a fresh tab at [`PAGE`], import the module at `module` (a path
    /// under the served directory) and await its `run(args)`: its answer and
    /// the page's console, or why there is none. A browser that answered
    /// [`Chrome::start`] and cannot open the tab now is a finding: it died or
    /// hung under an earlier run.
    pub(crate) fn run(&mut self, module: &str, args: &Value) -> Result<Ran, Refused> {
        let target = self
            .cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .map_err(|e| Refused::Failed(format!("{module}: no tab: {e}")))?;
        let target_id = target
            .get("targetId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let ran = self.run_in(&target_id, module, args);
        let _ = self
            .cdp
            .call(None, "Target.closeTarget", json!({"targetId": target_id}));
        ran
    }

    fn run_in(&mut self, target_id: &str, module: &str, args: &Value) -> Result<Ran, Refused> {
        let attached = self
            .cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .map_err(|e| Refused::Failed(format!("{module}: no tab: {e}")))?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for method in ["Inspector.enable", "Page.enable", "Runtime.enable"] {
            self.cdp
                .call(Some(&session), method, json!({}))
                .map_err(|e| Refused::Failed(format!("{module}: no tab: {e}")))?;
        }
        let fail = |cdp: &mut Cdp, why: String| {
            let console = console_lines(cdp.take_console(&session));
            Refused::Failed(if console.is_empty() {
                why
            } else {
                format!("{why}\n  its console:\n    {}", console.join("\n    "))
            })
        };
        let nav = self
            .cdp
            .call(
                Some(&session),
                "Page.navigate",
                json!({"url": self.url(PAGE)}),
            )
            .map_err(|e| fail(&mut self.cdp, e))?;
        if let Some(err) = nav.get("errorText").and_then(Value::as_str) {
            return Err(fail(&mut self.cdp, format!("{PAGE}: {err}")));
        }
        self.cdp
            .wait_event(&session, "Page.loadEventFired")
            .map_err(|e| fail(&mut self.cdp, e))?;
        let kickoff = kickoff(&self.url(module), args)
            .map_err(|e| Refused::Failed(format!("could not encode the arguments: {e}")))?;
        let started = self.cdp.call(
            Some(&session),
            "Runtime.evaluate",
            json!({"expression": kickoff, "returnByValue": true}),
        );
        started
            .and_then(|r| cdp::evaluated(&r))
            .map_err(|e| fail(&mut self.cdp, format!("{module} did not start: {e}")))?;
        let t0 = Instant::now();
        self.cdp
            .wait_console(&session, SETTLED)
            .map_err(|e| fail(&mut self.cdp, format!("{module}: {e}")))?;
        let settled = self
            .cdp
            .call(
                Some(&session),
                "Runtime.evaluate",
                json!({"expression": "window.atermRun", "awaitPromise": true, "returnByValue": true}),
            )
            .and_then(|r| cdp::evaluated(&r));
        let console = console_lines(self.cdp.take_console(&session));
        let answer = settled.map_err(|e| {
            Refused::Failed(format!(
                "{module}'s answer could not be read: {e}\n  its console:\n    {}",
                console.join("\n    ")
            ))
        })?;
        if answer.get("ok").and_then(Value::as_bool) != Some(true) {
            let error = answer
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("it threw");
            let mut why = format!(
                "{module} threw after {:.1} s: {error}",
                t0.elapsed().as_secs_f64()
            );
            if !console.is_empty() {
                why.push_str(&format!("\n  its console:\n    {}", console.join("\n    ")));
            }
            return Err(Refused::Failed(why));
        }
        let uncaught: Vec<&str> = console
            .iter()
            .map(String::as_str)
            .filter(|l| l.starts_with(cdp::UNCAUGHT))
            .collect();
        if !uncaught.is_empty() {
            return Err(Refused::Failed(format!(
                "{module} answered, but the page threw outside its run ({}):\n  its console:\n    {}",
                uncaught.join("; "),
                console.join("\n    ")
            )));
        }
        Ok(Ran {
            value: answer.get("value").cloned().unwrap_or(Value::Null),
            console,
        })
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.cdp.call(None, "Browser.close", json!({}));
        let waited = Instant::now();
        while waited.elapsed() < cdp::HANG_DETECTOR && self.running.ended().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // `running` kills and reaps a browser that did not close; `dir`
        // goes after it.
    }
}

/// The expression that starts `module`'s `run(args)` and returns at once:
/// the settled answer, `{ ok, value }` or `{ ok: false, error }`, is left in
/// `window.atermRun`, and [`SETTLED`] printed once it is.
fn kickoff(module: &str, args: &Value) -> Result<String, String> {
    let module = aterm_json::to_string(module).map_err(|e| e.to_string())?;
    let args = aterm_json::to_string(args).map_err(|e| e.to_string())?;
    let settled = aterm_json::to_string(SETTLED).map_err(|e| e.to_string())?;
    Ok(format!(
        "(() => {{ window.atermRun = import({module}).then((m) => m.run({args})).then(\
         (value) => ({{ ok: true, value: value === undefined ? null : value }}), \
         (e) => ({{ ok: false, error: String((e && e.stack) || e) }})).finally(\
         () => console.debug({settled})); return true; }})()"
    ))
}

/// `cdp::Cdp::take_console`'s lines with a `console.log` or `console.error`
/// line's kind dropped: the drivers' two output channels read as text.
fn console_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            ["console.error: ", "console.log: "]
                .iter()
                .find_map(|kind| line.strip_prefix(kind).map(str::to_string))
                .unwrap_or(line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kickoff names the module and its arguments as JSON literals, so a
    /// quote in either is data, and settles into `window.atermRun`.
    #[test]
    fn the_kickoff_names_its_module_and_arguments_as_literals() {
        let k = kickoff("http://127.0.0.1:1/a'b\".mjs", &json!({"x": "</script>"})).unwrap();
        assert!(
            k.contains(r#"import("http://127.0.0.1:1/a'b\".mjs")"#),
            "{k}"
        );
        assert!(k.contains(r#"m.run({"x":"</script>"})"#), "{k}");
        assert!(
            k.contains("window.atermRun = ") && k.contains(&format!("{SETTLED:?}")),
            "{k}"
        );
    }

    #[test]
    fn a_drivers_lines_read_as_text_and_other_kinds_keep_theirs() {
        assert_eq!(
            console_lines(vec![
                "console.error: wasm-smoke: ok".into(),
                "console.log: GL_LIFETIME_ALL_RETIRED".into(),
                "console.warn: careful".into(),
                "uncaught: Error: x".into(),
            ]),
            [
                "wasm-smoke: ok",
                "GL_LIFETIME_ALL_RETIRED",
                "console.warn: careful",
                "uncaught: Error: x"
            ]
        );
    }

    /// A real browser: a module's answer comes back by value with its
    /// console; one that throws is a finding naming the throw, with what it
    /// printed first, and so is one that answers after a timer of its threw
    /// or a rejection it left went unhandled (node's process failed on
    /// both); and the tab before it leaves nothing for the next.
    #[test]
    fn a_module_answers_by_value_and_a_throw_is_a_finding() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atv-page-")
            .tempdir()
            .expect("scratch");
        std::fs::write(
            dir.path().join("ok.mjs"),
            "export async function run(args) { console.error('from', args.who); \
             await new Promise((r) => setTimeout(r, 0)); \
             return { sum: args.a + 1, gc: typeof gc, isolated: crossOriginIsolated } }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("throws.mjs"),
            "export async function run() { console.log('before'); throw new Error('planted') }\n",
        )
        .unwrap();
        // The timer that throws was set first, so it runs before the one run
        // waits on; the rejection is reported as its event is dispatched,
        // which run waits for.
        std::fs::write(
            dir.path().join("timer.mjs"),
            "export async function run() { setTimeout(() => { throw new Error('a timer threw') }, 0); \
             await new Promise((r) => setTimeout(r, 0)); return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("dangling.mjs"),
            "export async function run() { \
             const seen = new Promise((r) => addEventListener('unhandledrejection', \
             () => setTimeout(r, 0), { once: true })); \
             Promise.reject(new Error('nobody handles this')); await seen; return 1 }\n",
        )
        .unwrap();
        let mut chrome = crate::wasm_page::start_for_test(dir.path());
        let ok = chrome
            .run("ok.mjs", &json!({"a": 41, "who": "rust"}))
            .expect("an answer");
        assert_eq!(
            ok.value,
            json!({"sum": 42, "gc": "function", "isolated": true})
        );
        assert_eq!(ok.console, ["from rust"]);
        let threw = chrome.run("throws.mjs", &json!({})).expect_err("a throw");
        let Refused::Failed(why) = threw else {
            panic!("a throw is a finding: {threw:?}")
        };
        assert!(
            why.contains("Error: planted") && why.contains("before"),
            "{why}"
        );
        for (module, thrown) in [
            ("timer.mjs", "a timer threw"),
            ("dangling.mjs", "nobody handles this"),
        ] {
            let threw = chrome.run(module, &json!({})).expect_err(module);
            let Refused::Failed(why) = threw else {
                panic!("{module}: a throw outside run is a finding: {threw:?}")
            };
            assert!(
                why.contains("threw outside its run") && why.contains(thrown),
                "{module}: {why}"
            );
        }
        let missing = chrome
            .run("missing.mjs", &json!({}))
            .expect_err("no module");
        assert!(matches!(missing, Refused::Failed(_)), "{missing:?}");
        assert!(chrome.product().contains("Chrome"), "{}", chrome.product());
        let again = chrome
            .run("ok.mjs", &json!({"a": 0, "who": "again"}))
            .unwrap();
        assert_eq!(again.console, ["from again"]);
        // A browser that died after it answered: the next run is a finding,
        // never the machine's answer (which a lane reads as COULD NOT RUN).
        chrome.running.0.kill().expect("the test's own browser");
        let _ = chrome.running.0.wait();
        let dead = chrome
            .run("ok.mjs", &json!({"a": 0, "who": "dead"}))
            .expect_err("no browser");
        assert!(matches!(dead, Refused::Failed(_)), "{dead:?}");
    }
}
