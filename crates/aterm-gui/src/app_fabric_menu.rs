// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Fabric menu's App side. The bar rows live in [`crate::menu`]
//! (`FABRIC_MENU`, the View presence checkables, Window ▸ Set Role…); the
//! dispatch arm in `app_input.rs` hands each one here. Each row fronts
//! something the wire already does:
//!
//! * **Inbox**: `inbox --peek --meta` on the focused session as a Markdown tab;
//!   metadata only, the file written `0600`.
//! * **Hold This Session / Lift Hold**: the `hold` verb as
//!   [`crate::fabric::HoldIssuer::Owner`], so a fleet hold is as untouchable
//!   here as from `aterm ctl hold`: the rows grey with the fleet's reason, a
//!   Release click past the grey is answered `ERR denied`, and a Hold click
//!   past it sets the owner's own hold beside the fleet's (each origin is its
//!   own slot), which outlasts the fleet's lift.
//! * **Fabric Status / Turn Fabric On… / Turn Fabric Off…**: `aterm fabric
//!   [on|off]` as a child on a thread, its output opened as a tab; `on`/`off`
//!   confirm first.
//! * **Presence Band / Presence Rim**: flip the live bit and queue the
//!   `[presence]` write through the config lane (`App::queue_presence_write`).
//!
//! Headless (no OS window), the fabric commands keep their plan on the App and
//! run no child and show no sheet.

use crate::menu::{FrontHold, MenuAction};
use crate::platform::AppRt as _;
use crate::presence::HoldFact;
use crate::{App, WindowId};
use std::path::{Path, PathBuf};

/// The `[presence]` leaf keys the two View checkables write — the registered
/// settings keys (`prefs::NESTED_LEAVES`), so the config lane admits them.
pub(crate) const PRESENCE_BAND_KEY: &str = crate::prefs::EDIT_PRESENCE_BAND;
pub(crate) const PRESENCE_RIM_KEY: &str = crate::prefs::EDIT_PRESENCE_RIM;

/// The human name of a presence leaf key — the row's subject.
pub(crate) fn presence_key_label(key: &str) -> &'static str {
    if key == PRESENCE_RIM_KEY {
        "Presence Rim"
    } else {
        "Presence Band"
    }
}

/// A presence leaf key's resolved bit in `config` (absent ⇒ on).
pub(crate) fn presence_key_resolve(key: &str, config: &crate::app_config::Config) -> bool {
    if key == PRESENCE_RIM_KEY {
        config.presence_rim_enabled()
    } else {
        config.presence_band_enabled()
    }
}

/// Which `aterm fabric` command a Fabric-menu row runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FabricVerb {
    /// `aterm fabric` — the status screen.
    Status,
    /// `aterm fabric on` — behind a confirmation.
    On,
    /// `aterm fabric off` — behind a confirmation.
    Off,
}

impl FabricVerb {
    /// The verb's word in a message key (`fabric.<verb>`): one row per verb
    /// on the band, so a rerun replaces its own outcome.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// The CLI's argv after the program.
    pub(crate) fn argv(self) -> Vec<String> {
        let mut v = vec!["fabric".to_string()];
        match self {
            Self::Status => {}
            Self::On => v.push("on".to_string()),
            Self::Off => v.push("off".to_string()),
        }
        v
    }

    /// The verb as a row's title names it: `Fabric status`, `Fabric On`,
    /// `Fabric Off`.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Status => "Fabric status",
            Self::On => "Fabric On",
            Self::Off => "Fabric Off",
        }
    }

    /// The command as a human types it.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Status => "aterm fabric",
            Self::On => "aterm fabric on",
            Self::Off => "aterm fabric off",
        }
    }

    /// The confirmation `on`/`off` ask before running: title, body, and the
    /// proceed button's label. `None` for the status read, which changes
    /// nothing.
    const fn confirmation(self) -> Option<(&'static str, &'static str, &'static str)> {
        match self {
            Self::Status => None,
            Self::On => Some((
                "Turn the fabric on?",
                "Sessions on this machine can message each other, and the fleet can halt them.",
                "Turn On",
            )),
            Self::Off => Some((
                "Turn the fabric off?",
                "Sessions stop getting messages until you turn it on again.",
                "Turn Off",
            )),
        }
    }
}

/// What a Fabric-menu row asked `aterm` to do: the verb, its argv, and the
/// file its output goes to (opened as a tab when the child ends). Kept on the
/// App (`last_fabric_plan`) for the tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FabricPlan {
    pub(crate) verb: FabricVerb,
    pub(crate) argv: Vec<String>,
    pub(crate) out: PathBuf,
}

impl FabricPlan {
    fn new(verb: FabricVerb) -> Self {
        let kind = match verb {
            FabricVerb::Status => "fabric-status",
            FabricVerb::On => "fabric-on",
            FabricVerb::Off => "fabric-off",
        };
        Self {
            verb,
            argv: verb.argv(),
            out: menu_document_path(kind, ""),
        }
    }
}

/// The child's end, as `Wake::FabricCli` carries it back to the main loop.
#[derive(Debug)]
pub(crate) struct FabricCliOutcome {
    pub(crate) wid: WindowId,
    pub(crate) plan: FabricPlan,
    /// The exit code (`None` when killed by a signal), or why it could not run.
    pub(crate) status: Result<Option<i32>, String>,
}

/// Where a Fabric-menu document goes: the temp dir, one file per gesture
/// (`aterm-<kind>[-<sid>]-<unix seconds>.md`), so a tab left open on an
/// earlier one is never rewritten under it. `sid` is filtered to its safe
/// characters, the ledger key's rule.
pub(crate) fn menu_document_path(kind: &str, sid: &str) -> PathBuf {
    let unix_s = aterm_time::unix_secs().unwrap_or(0);
    let safe: String = sid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let name = if safe.is_empty() {
        format!("aterm-{kind}-{unix_s}.md")
    } else {
        format!("aterm-{kind}-{safe}-{unix_s}.md")
    };
    std::env::temp_dir().join(name)
}

/// Write `text` to `path`, created `0600` on unix (a listing of who wrote to a
/// session, or a fabric status, is the owner's to read).
pub(crate) fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(text.as_bytes())
}

/// The Inbox tab's text: a heading, what the listing is, then the
/// `inbox --peek --meta` reply verbatim in a fenced block (id, offset, time,
/// sender, kind, trust and flags; no body).
pub(crate) fn inbox_markdown(sid: &str, listing: &str) -> String {
    let mut s = String::with_capacity(listing.len() + 256);
    s.push_str(&format!("# Inbox \u{2014} {sid}\n\n"));
    s.push_str(
        "Who wrote, what kind, how trusted, when \u{2014} never the text of a message. \
         Opening this list leaves the session's inbox as it was.\n\n",
    );
    s.push_str("```text\n");
    s.push_str(listing.trim_end());
    s.push_str("\n```\n");
    s
}

/// The Show Identity tab's text: a heading, then either the `identities <name>`
/// reply verbatim in a fenced block, or the sentence for a session with none.
pub(crate) fn identity_markdown(sid: &str, identity: Option<&str>, reply: Option<&str>) -> String {
    let mut s = String::with_capacity(256 + reply.map_or(0, str::len));
    match identity {
        Some(name) => {
            s.push_str(&format!("# Identity \u{2014} {name}\n\n"));
            s.push_str(&format!(
                "Session {sid} runs under the agent identity `{name}`: its agents keep \
                 their homes (and logins) in the identity's own directory. Read-only.\n\n"
            ));
            s.push_str("```text\n");
            s.push_str(reply.unwrap_or("").trim_end());
            s.push_str("\n```\n");
        }
        None => {
            s.push_str("# Identity \u{2014} none\n\n");
            s.push_str(&format!(
                "Session {sid} carries no agent identity: it runs under your own agent \
                 configuration. File \u{25b8} New Tab With Identity\u{2026} (or `spawn \
                 identity=<name>`) starts a session that has one.\n"
            ));
        }
    }
    s
}

/// The Fabric Status / On / Off tab's text: the command, how it ended, then
/// its stdout and stderr in fenced blocks.
fn fabric_markdown(
    plan: &FabricPlan,
    status: &Result<Option<i32>, String>,
    stdout: &str,
    stderr: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", plan.verb.words()));
    s.push_str(&format!("{}\n\n", fabric_status_words(status)));
    if !stdout.trim().is_empty() {
        s.push_str("```text\n");
        s.push_str(stdout.trim_end());
        s.push_str("\n```\n\n");
    }
    if !stderr.trim().is_empty() {
        s.push_str("stderr:\n\n```text\n");
        s.push_str(stderr.trim_end());
        s.push_str("\n```\n");
    }
    s
}

/// Why `aterm fabric …` did not end cleanly, in a few words — the band row's
/// reason: `exited 2`, `killed by a signal`, or the spawn's own error.
fn fabric_failure_reason(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(code)) => format!("exited {code}"),
        Ok(None) => "killed by a signal".to_string(),
        Err(e) => e.clone(),
    }
}

/// Why the `aterm` binary did not start: its path and the OS error. No verb of
/// its own — the tab's "Could not run:" and the band's "Couldn't run …" say it.
fn spawn_failure(cli: &std::path::Path, e: &std::io::Error) -> String {
    format!("{}: {e}", cli.display())
}

/// The output tab's line under its heading (the command): how it ended, and
/// nothing the heading or the page itself already says.
fn fabric_status_words(status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(0)) => "Done.".to_string(),
        Ok(Some(code)) => format!("Exited {code}."),
        Ok(None) => "Killed by a signal.".to_string(),
        Err(e) => format!("Could not run: {e}"),
    }
}

impl App {
    /// The View ▸ Presence Band bit as applied.
    pub(crate) fn presence_band_on(&self) -> bool {
        self.presence_band_on
    }

    /// The View ▸ Presence Rim bit as applied.
    pub(crate) fn presence_rim_on(&self) -> bool {
        self.presence_rim_on
    }

    /// Adopt `[presence]` from the config the App holds — on load and on every
    /// reload — publishing the bits for the native menu's checkmarks and
    /// re-projecting every window when one moved.
    pub(crate) fn adopt_presence_config(&mut self) {
        let band = self.config.presence_band_enabled();
        let rim = self.config.presence_rim_enabled();
        crate::menu::set_presence_toggles(band, rim);
        if band == self.presence_band_on && rim == self.presence_rim_on {
            return;
        }
        self.presence_band_on = band;
        self.presence_rim_on = rim;
        self.refresh_presence_all_windows();
    }

    /// Set one presence bit live: publish it, re-project every window, and
    /// re-resolve any open palette's checkmark.
    pub(crate) fn set_presence_bit(&mut self, key: &str, on: bool) {
        if key == PRESENCE_RIM_KEY {
            self.presence_rim_on = on;
        } else {
            self.presence_band_on = on;
        }
        crate::menu::set_presence_toggles(self.presence_band_on, self.presence_rim_on);
        self.refresh_presence_all_windows();
        self.palette_refresh_live();
    }

    /// View ▸ Presence Band / Presence Rim: flip the bit now and persist it.
    /// A write the config lane cannot even queue (no event-loop proxy, a
    /// closed lane) is said on the notice; the live flip stands for this run.
    pub(crate) fn user_toggle_presence(&mut self, action: MenuAction) {
        let (key, current) = match action {
            MenuAction::TogglePresenceBand => (PRESENCE_BAND_KEY, self.presence_band_on),
            MenuAction::TogglePresenceRim => (PRESENCE_RIM_KEY, self.presence_rim_on),
            _ => return,
        };
        let desired = !current;
        self.set_presence_bit(key, desired);
        if let Err(error) = self.queue_presence_write(key, desired) {
            self.post_message(crate::message_reporters::presence_not_saved(
                key,
                presence_key_label(key),
                &error.to_string(),
                false,
            ));
        }
    }

    /// The standing hold on `wid`'s front session, as the presence band
    /// prints it — `None` with no front terminal or no hold.
    pub(crate) fn front_hold_fact(&self, wid: WindowId) -> Option<HoldFact> {
        let session = self.front_terminal(wid)?.session;
        let hold = self.pool.get(session)?.ctx.fabric.hold()?;
        Some(HoldFact {
            fleet: hold.origin == "fleet",
            reason: hold.reason,
        })
    }

    /// Publish `wid`'s front hold for the native menu's synchronous validate.
    pub(crate) fn publish_front_hold(&mut self, wid: WindowId) {
        let fact = self.front_hold_fact(wid);
        let reason = fact.as_ref().map_or("", |h| h.reason.as_str());
        crate::menu::set_front_hold(
            match &fact {
                None => FrontHold::None,
                Some(h) if h.fleet => FrontHold::Fleet,
                Some(_) => FrontHold::Local,
            },
            reason,
        );
    }

    /// Hold This Session (`on`) / Lift Hold (`off`) for `wid`'s focused
    /// session: the `hold` verb as the Owner, local origin, with the same guard
    /// and timeline event a wire `hold` has. The presence band is re-projected
    /// here (the fabric wake also arrives; the refresh is change-gated). A
    /// refusal under a fleet hold is shown.
    ///
    /// Never waits on a committing update: the Commit's fence
    /// ([`crate::fabric::SessionFabric::fence_hold`]) is raised and lifted only
    /// on this thread, and a `HeldCommit` keeps it raised across the run-loop
    /// turns that dispatch this press, so a wait could only end at its bound
    /// with the window frozen. The press is answered at once: `ERR busy`
    /// ([`crate::fabric::HOLD_BUSY`]), nothing moved, said on the band.
    pub(crate) fn menu_hold(&mut self, wid: WindowId, on: bool) {
        use crate::fabric::{HOLD_BUSY, HOLD_DENIED, HoldIssuer, cmd_hold_within};
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        let Some(sid) = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
        else {
            return;
        };
        let rest = if on {
            format!("{sid} on reason=menu")
        } else {
            format!("{sid} off")
        };
        let reply = cmd_hold_within(
            &self.store,
            &rest,
            HoldIssuer::Owner,
            Some(std::time::Duration::ZERO),
        );
        self.refresh_presence_session(session, false);
        self.publish_front_hold(wid);
        self.palette_refresh_live();
        // Under a fleet hold, Hold fills the owner's own slot beside it (a
        // success, unsaid) and Release with nothing of the owner's to lift is
        // `HOLD_DENIED`, a failure on the band; a bridge refusal is a failure
        // either way.
        let refused = if reply == HOLD_DENIED {
            let reason = self
                .pool
                .get(session)
                .and_then(|s| s.ctx.fabric.hold())
                .map(|h| h.reason)
                .unwrap_or_default();
            Some(crate::message_reporters::fleet_hold(on, &reason))
        } else if reply == HOLD_BUSY {
            Some(crate::message_reporters::hold_refused(
                on,
                "aterm is switching to its new version right now; nothing changed",
            ))
        } else {
            reply
                .strip_prefix("ERR ")
                .map(|err| crate::message_reporters::hold_refused(on, err.trim_end()))
        };
        match refused {
            Some(msg) if msg.hold == aterm_messages::Hold::LogOnly => {
                self.record_message(msg);
            }
            Some(msg) => {
                self.post_message(msg);
            }
            None => {}
        }
    }

    /// Inbox: the focused session's `inbox --peek --meta` as a Markdown tab
    /// on `wid`. Nothing with no focused session (the row is greyed then).
    pub(crate) fn open_session_inbox_tab(&mut self, wid: WindowId) {
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return;
        };
        let sid = ctx.self_id.as_str().to_string();
        let listing = crate::fabric::cmd_inbox(&ctx, "--peek --meta");
        let text = inbox_markdown(&sid, &listing);
        let path = menu_document_path("inbox", &sid);
        match write_private(&path, &text) {
            Ok(()) => self.open_menu_document(wid, path),
            Err(e) => self.menu_failure(
                "inbox",
                "Couldn't open the inbox",
                &[&e.to_string(), &path.display().to_string()],
            ),
        }
    }

    /// Window ▸ Show Identity: the focused session's agent identity, read-only,
    /// as a Markdown tab: the `identities <name>` reply when the session wears
    /// one, else a sentence saying it runs under the user's own agent
    /// configuration.
    pub(crate) fn open_session_identity_tab(&mut self, wid: WindowId) {
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        let Some(sid) = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
        else {
            return;
        };
        let identity = self.identity_of_local(session);
        let reply = identity
            .as_deref()
            .map(|name| crate::agent_identity::cmd_identities(&self.store, name));
        let text = identity_markdown(&sid, identity.as_deref(), reply.as_deref());
        let path = menu_document_path("identity", &sid);
        match write_private(&path, &text) {
            Ok(()) => self.open_menu_document(wid, path),
            Err(e) => self.menu_failure(
                "identity",
                "Show Identity did not open",
                &[&e.to_string(), &path.display().to_string()],
            ),
        }
    }

    /// Open a file this menu wrote as a Markdown tab on `wid` (the same
    /// admission every Open Markdown… takes), remembering it for the tests.
    fn open_menu_document(&mut self, wid: WindowId, path: PathBuf) {
        self.last_menu_document = Some(path.clone());
        let uri = match crate::native_document_host::path_to_file_uri(&path) {
            Ok(uri) => uri,
            Err(e) => {
                self.menu_failure(
                    "document",
                    "Couldn't open the document",
                    &[&e.to_string(), &path.display().to_string()],
                );
                return;
            }
        };
        if let Err(e) =
            self.request_document_tab_in_window(wid, crate::native_app::AppKind::Markdown, &uri)
        {
            self.menu_failure(
                "document",
                "Couldn't open the document",
                &[&e.to_string(), &path.display().to_string()],
            );
        }
    }

    /// A fabric menu item that could not do its thing: a terse failure and its
    /// cause (then the path) on the band, keyed by `verb`.
    fn menu_failure(&mut self, verb: &str, title: &str, lines: &[&str]) {
        self.post_message(crate::message_reporters::fabric_failure(verb, title, lines));
    }

    /// Fabric Status / Turn Fabric On… / Turn Fabric Off…: confirm when the
    /// verb changes something, compose the plan, and — on a real window — run
    /// `aterm fabric …` as a child on its own thread (it walks every local
    /// instance itself), opening its output as a tab when it ends.
    /// Headless composes the plan and runs nothing.
    pub(crate) fn run_fabric_cli(&mut self, wid: WindowId, verb: FabricVerb) {
        let real_window = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_some());
        if real_window && let Some((title, body, proceed)) = verb.confirmation() {
            match self.apprt.confirm(title, body, proceed) {
                Some(true) => {}
                Some(false) => return,
                None => {
                    self.menu_failure(
                        verb.key(),
                        &format!("Couldn't start {}", verb.title()),
                        &[
                            "no confirmation dialog on this platform",
                            &format!("run `{}` in a shell", verb.words()),
                        ],
                    );
                    return;
                }
            }
        }
        let plan = FabricPlan::new(verb);
        self.last_fabric_plan = Some(plan.clone());
        if !real_window {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-fabric-menu".to_string(),
            crate::qos::Role::Background,
            move || {
                // Waits on a CLI child the human is not blocked on.
                let cli = crate::ledger_key::aterm_cli();
                // `aterm fabric` walks every local instance itself (as `aterm
                // ctl instances` does), so the child gets no socket. `aterm.exe`
                // is a console image: on Windows `spawnfd::command` gives it a
                // windowless console, not the visible one a bare child would
                // open (`ledger_key::launch`).
                let mut cmd = aterm_uds::spawnfd::command(&cli);
                cmd.args(&plan.argv);
                let (status, stdout, stderr) = match cmd.output() {
                    Ok(out) => (
                        Ok(out.status.code()),
                        String::from_utf8_lossy(&out.stdout).into_owned(),
                        String::from_utf8_lossy(&out.stderr).into_owned(),
                    ),
                    Err(e) => (Err(spawn_failure(&cli, &e)), String::new(), String::new()),
                };
                let text = fabric_markdown(&plan, &status, &stdout, &stderr);
                let status = match write_private(&plan.out, &text) {
                    Ok(()) => status,
                    Err(e) => Err(format!("could not write {}: {e}", plan.out.display())),
                };
                let _ = proxy.send_event(crate::Wake::FabricCli(Box::new(FabricCliOutcome {
                    wid,
                    plan,
                    status,
                })));
            },
        );
        if let Err(e) = spawned {
            self.menu_failure(
                verb.key(),
                &format!("Couldn't start {}", verb.title()),
                &[&e.to_string()],
            );
        }
    }

    /// The child ended (`Wake::FabricCli`): report it on the band (a clean exit
    /// is a success row, anything else a warning) and open its output as a tab
    /// on the window that asked, or the front window if that one is gone.
    pub(crate) fn complete_fabric_cli(&mut self, outcome: FabricCliOutcome) {
        let verb = outcome.plan.verb;
        let ok = matches!(outcome.status, Ok(Some(0)));
        // A clean exit is a record (the output tab opens anyway); anything else
        // is a failure: a terse title, the reason, and where the output is. The
        // title is `Fabric On finished`, never the command's transcript line
        // (`aterm fabric on: done.`), which stays in the output tab.
        let (title, reason) = if ok {
            (format!("{} finished", verb.title()), String::new())
        } else {
            (
                format!("Couldn't run {}", verb.title()),
                fabric_failure_reason(&outcome.status),
            )
        };
        let mut msg = crate::message_reporters::fabric_status(verb.key(), &title, &reason, ok);
        if !ok && outcome.plan.out.is_file() {
            msg = msg.line("output is in the new tab");
        }
        self.post_message(msg);
        let wid = if self.windows.contains_key(&outcome.wid) {
            Some(outcome.wid)
        } else {
            self.frontmost_window
        };
        if let Some(wid) = wid
            && outcome.plan.out.is_file()
        {
            self.open_menu_document(wid, outcome.plan.out);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the Fabric menu's App side.

    use super::*;
    use crate::menu::MenuAction;
    use crate::presence::Level;
    use crate::session_timeline::MetaField;
    use std::sync::Arc;

    fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.frontmost_window = Some(wid);
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        (app, wid, sid, ctx)
    }

    fn row(app: &App, wid: WindowId, a: MenuAction) -> crate::palette::PaletteRow {
        app.palette_snapshot(wid)
            .rows()
            .iter()
            .find(|r| r.action == a)
            .cloned()
            .unwrap_or_else(|| panic!("{a:?} has a row"))
    }

    /// Under a fleet hold both halt rows are disabled (palette mirror and native
    /// validate) with the fleet's reason; a Release click reaching `menu_hold` is
    /// refused, and a Hold click fills the owner's own slot beside the fleet's.
    /// Under no hold, Hold is live and holds locally; then Lift is live and lifts.
    #[test]
    fn hold_and_lift_are_disabled_with_the_fleet_reason_under_a_fleet_hold() {
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.refresh_presence_window(wid);
        assert!(row(&app, wid, MenuAction::HoldSession).enabled);
        assert!(!row(&app, wid, MenuAction::LiftHold).enabled);
        assert_eq!(app.front_hold_fact(wid), None);

        // The fleet halts the session.
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "main%20broken".into(),
                origin: "fleet".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Hold);
        assert!(
            app.front_hold_fact(wid)
                .is_some_and(|h| h.fleet && h.reason == "main%20broken")
        );
        for a in [MenuAction::HoldSession, MenuAction::LiftHold] {
            let r = row(&app, wid, a);
            assert!(!r.enabled, "{a:?} under a fleet hold");
            assert!(r.label.contains("main broken"), "{a:?}: {}", r.label);
        }
        // A Release click that slipped past the grey: refused, shown, the
        // hold stands — there is nothing of the owner's to lift.
        app.menu_hold(wid, false);
        assert!(ctx.fabric.hold().is_some_and(|h| h.origin == "fleet"));
        // A Hold click past the grey fills the owner's own slot and leaves the
        // fleet's as it was, so the session stays held when the fleet lifts: a
        // success, unsaid.
        app.menu_hold(wid, true);
        assert_eq!(ctx.fabric.holds().origins(), "fleet,local");
        assert!(ctx.fabric.hold().is_some_and(|h| h.origin == "fleet"));
        assert!(
            !app.messages
                .log()
                .records()
                .any(|r| r.title == "Session already held"),
            "nothing was refused"
        );
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        app.on_presence_wake(&ctx.self_id, false);
        let refusal = app
            .messages
            .live_by_key(crate::message_reporters::KEY_FABRIC_HOLD)
            .map(|l| l.id)
            .expect("the refusal row");
        assert!(app.resolve_message(refusal, aterm_messages::Outcome::Ok));

        // Hold This Session: a local hold, the band's stop rim, Lift now live.
        app.menu_hold(wid, true);
        let hold = ctx.fabric.hold().expect("held");
        assert_eq!(
            (hold.origin.as_str(), hold.reason.as_str()),
            ("local", "menu")
        );
        assert_eq!(app.presence_level(wid), Level::Hold);
        assert!(
            app.front_hold_fact(wid)
                .is_some_and(|h| !h.fleet && h.reason == "menu")
        );
        assert!(!row(&app, wid, MenuAction::HoldSession).enabled);
        assert!(row(&app, wid, MenuAction::LiftHold).enabled);
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::KEY_FABRIC_HOLD)
                .is_none(),
            "no message on success"
        );

        // Lift Hold: gone, and the rows swap back.
        app.menu_hold(wid, false);
        assert!(ctx.fabric.hold().is_none());
        assert_ne!(app.presence_level(wid), Level::Hold);
        assert_eq!(app.front_hold_fact(wid), None);
        assert!(row(&app, wid, MenuAction::HoldSession).enabled);
        assert!(!row(&app, wid, MenuAction::LiftHold).enabled);
        let _ = sid;
    }

    /// A Hold or Lift press under a committing update's hold fence (raised and
    /// lifted only on this thread) is answered at once with the refusal shown
    /// and nothing moved: it never blocks on the fence
    /// ([`crate::fabric::fence_blocks_on_this_thread`]), where a wait could end
    /// only at [`crate::fabric::SessionFabric::fence_wait`]. Controls: the
    /// verb's own bounded wait under the same fence is counted, and with the
    /// fence lifted the same press holds the session.
    #[test]
    fn a_menu_hold_under_a_committing_update_is_refused_and_moves_nothing() {
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let _ = ctx.fabric.fence_hold(true);
        let blocks = crate::fabric::fence_blocks_on_this_thread();
        app.menu_hold(wid, true);
        assert_eq!(
            crate::fabric::fence_blocks_on_this_thread(),
            blocks,
            "the press waited on a fence only this thread lifts"
        );
        // Control: the verb's own bounded wait under the same fence is counted.
        let _ = crate::fabric::cmd_hold_within(
            &app.store,
            &format!("{} on reason=verb", ctx.self_id.as_str()),
            crate::fabric::HoldIssuer::Owner,
            Some(std::time::Duration::from_millis(1)),
        );
        assert!(
            crate::fabric::fence_blocks_on_this_thread() > blocks,
            "a bounded wait under the fence blocks, and the count sees it"
        );
        assert!(ctx.fabric.hold().is_none(), "nothing moved");
        assert!(
            app.messages
                .live_by_key(crate::message_reporters::KEY_FABRIC_HOLD)
                .is_some(),
            "the refusal is said"
        );
        ctx.fabric.lift_hold_fence();
        app.menu_hold(wid, true);
        assert!(
            ctx.fabric
                .hold()
                .is_some_and(|hold| hold.origin == "local" && hold.reason == "menu"),
            "control: unfenced, the press holds the session"
        );

        // Lift Hold under the fence: answered at once, titled for a lift.
        let _ = ctx.fabric.fence_hold(true);
        let blocks = crate::fabric::fence_blocks_on_this_thread();
        app.menu_hold(wid, false);
        assert_eq!(
            crate::fabric::fence_blocks_on_this_thread(),
            blocks,
            "the lift waited on a fence only this thread lifts"
        );
        assert!(ctx.fabric.hold().is_some(), "nothing moved");
        assert!(
            app.has_live_message("Couldn't lift the hold"),
            "the lift refusal is titled for a lift"
        );
        ctx.fabric.lift_hold_fence();
    }

    /// Show Identity: a session with no identity opens a read-only, owner-private
    /// tab saying so; for a session that wears one, the markdown fences the
    /// `identities <name>` reply verbatim.
    #[test]
    fn show_identity_opens_a_read_only_tab_for_the_focused_session() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        app.open_session_identity_tab(wid);
        let path = app
            .last_menu_document
            .clone()
            .expect("a document was written");
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(text.contains(ctx.self_id.as_str()), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the document is the owner's");
        }
        let _ = std::fs::remove_file(&path);

        let worn = super::identity_markdown(
            "s-abc",
            Some("worker"),
            Some("OK 2\nworker dir=/x sessions=1 agents=claude:present\nagent=claude\n"),
        );
        assert!(worn.ends_with(
            "```text\nOK 2\nworker dir=/x sessions=1 agents=claude:present\nagent=claude\n```\n"
        ));
    }

    /// Inbox: a delivered task appears in the tab's file by id, sender, kind and
    /// trust, and its text does not. The row is greyed with no session.
    #[test]
    fn inbox_opens_a_metadata_only_tab_and_never_a_body() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let reply = crate::fabric::cmd_deliver(
            &app.store,
            &format!(
                "{} off=7 from=h-manager kind=task trust=human text=the%20secret%20body",
                ctx.self_id.as_str()
            ),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        app.open_session_inbox_tab(wid);
        let path = app
            .last_menu_document
            .clone()
            .expect("a document was written");
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(
            text.contains("from=h-manager kind=task trust=human"),
            "{text}"
        );
        assert!(!text.contains("secret"), "no body: {text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the listing is the owner's");
        }
        assert!(
            app.active_native_view(wid).is_some(),
            "the inbox is a native document tab on the window"
        );
        let _ = std::fs::remove_file(&path);
        // Peeked: the row is still unlisted for the session's own drain.
        assert!(crate::fabric::cmd_inbox(&ctx, "--peek").contains("pending=0"));
        assert!(crate::fabric::cmd_inbox(&ctx, "").contains("from=h-manager"));
    }

    /// Set Role…: the pin's editor, one field over — the commit writes `meta
    /// role`, the presence slot carries it, and the title pin is untouched.
    #[test]
    fn set_role_writes_the_role_through_the_pins_editor() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        assert!(app.begin_active_session_meta_edit(wid, MetaField::Role));
        let edit = app
            .windows
            .get(&wid)
            .and_then(|ws| ws.rename_edit.as_ref())
            .expect("an edit is live");
        assert_eq!(edit.field, MetaField::Role);
        assert_eq!(edit.session, sid);
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("manager".into()));
        app.commit_session_rename(wid, sid, "manager");
        let meta = ctx.meta.lock().unwrap();
        assert_eq!(meta.role.as_deref(), Some("manager"));
        assert_eq!(meta.user_title, None, "the title pin is untouched");
        drop(meta);
        assert_eq!(
            app.presence
                .slot(sid)
                .and_then(|s| s.role.clone())
                .as_deref(),
            Some("manager"),
            "the band's first slot"
        );
        // The dispatch arm reaches the same editor.
        assert!(app.windows.get(&wid).unwrap().rename_edit.is_none());
        assert_eq!(
            crate::menu::MenuAction::SetRole.invoke_authority(),
            crate::menu::InvokeAuthority::WriteInput
        );
    }

    /// The presence toggles: unchecking the band hides the row (and `chrome`
    /// says so) without touching the level; unchecking the rim paints none;
    /// each click flips the live bit, publishes it for the native checkmark,
    /// and queues exactly one durable write of its `[presence]` leaf. A config
    /// with `[presence] band = false` is adopted on reload.
    #[test]
    fn the_presence_toggles_gate_the_projection_and_persist() {
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (mut app, wid, _sid, ctx) = app_with_stub();
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "pause".into(),
                origin: "local".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Hold);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(app.presence_report(wid).unwrap().1, "stop-hold");

        // Band off: no row, an empty band on `chrome`, the level unchanged.
        app.user_toggle_presence(MenuAction::TogglePresenceBand);
        assert!(!app.presence_band_on());
        assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        assert_eq!(app.presence_report(wid).unwrap().0, "");
        assert_eq!(app.presence_level(wid), Level::Hold);
        assert!(app.presence_chrome_line().contains("level=hold band=\"\""));
        assert_eq!(
            row(&app, wid, MenuAction::TogglePresenceBand).checked,
            Some(false)
        );
        // The native checkmark's static is asserted in `menu.rs`'s own test:
        // every App in this binary publishes through it, so it cannot be read
        // back reliably here.

        // Rim off: none painted; the a11y level still says hold.
        app.user_toggle_presence(MenuAction::TogglePresenceRim);
        assert!(!app.presence_rim_on());
        assert_eq!(app.presence_report(wid).unwrap().1, "none");
        assert!(
            app.presence_overlay(wid, std::time::Instant::now())
                .is_none()
        );

        // Back on: the row and the rim return.
        app.user_toggle_presence(MenuAction::TogglePresenceBand);
        app.user_toggle_presence(MenuAction::TogglePresenceRim);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(app.presence_report(wid).unwrap().1, "stop-hold");

        // A reload carrying `[presence] band = false` is adopted.
        app.config =
            aterm_toml::from_str::<crate::app_config::Config>("[presence]\nband = false\n")
                .expect("parses");
        assert!(!app.config.presence_band_enabled());
        assert!(app.config.presence_rim_enabled(), "absent ⇒ on");
        app.adopt_presence_config();
        assert!(!app.presence_band_on());
        assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        assert_eq!(
            app.presence_report(wid).unwrap().1,
            "stop-hold",
            "the rim stays"
        );
        assert!(!presence_key_resolve(PRESENCE_BAND_KEY, &app.config));
    }

    /// The fabric commands compose the documented argv and, headless, run
    /// nothing: no child, no sheet, no file. Off asks first.
    #[test]
    fn the_fabric_commands_compose_their_argv_and_run_nothing_headless() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        for (verb, argv) in [
            (FabricVerb::Status, vec!["fabric"]),
            (FabricVerb::On, vec!["fabric", "on"]),
            (FabricVerb::Off, vec!["fabric", "off"]),
        ] {
            app.run_fabric_cli(wid, verb);
            let plan = app.last_fabric_plan.clone().expect("a plan");
            assert_eq!(plan.verb, verb);
            assert_eq!(plan.argv, argv);
            assert!(!plan.out.exists(), "headless writes nothing");
            assert!(
                plan.out
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("aterm-fabric-") && n.ends_with(".md"))
            );
        }
        assert!(app.last_menu_document.is_none());
        assert!(FabricVerb::Status.confirmation().is_none());
        assert!(FabricVerb::Off.confirmation().is_some());
    }
}
