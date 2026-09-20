// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC MENU'S APP SIDE (round 19, SPEC19 §9: "the menu, reworked to
//! align"). The bar rows live in [`crate::menu`] (`FABRIC_MENU`, the View
//! presence checkables, Window ▸ Set Role…); the dispatch arm in
//! `app_input.rs` hands each one here. Nothing in this module is a second
//! mechanism: every row is the menu-bar face of something the wire already
//! does —
//!
//! * **Inbox…** is `inbox --peek --meta` on the focused session, rendered as a
//!   Markdown tab. METADATA ONLY, by construction: the `--meta` form prints no
//!   body, and the file is written `0600`.
//! * **Hold This Session / Lift Hold** are the `hold` verb with
//!   [`crate::fabric::HoldIssuer::Owner`] — the LOCAL origin, the only one the
//!   Owner may name — so a FLEET hold is exactly as untouchable from the menu
//!   as from `aterm ctl hold`: the rows grey with the fleet's reason and a
//!   click that slipped past the grey is answered `ERR denied` and shown.
//! * **Fabric Status… / Turn Fabric On… / Turn Fabric Off…** run `aterm fabric
//!   [on|off]` as a CHILD of this process — the round-13 verb, untouched — on
//!   a thread, pointed at THIS instance's control socket, and open what it
//!   printed as a tab. `on`/`off` ask first (the platform's confirm sheet).
//! * **Presence Band / Presence Rim** flip the live bit at the click and queue
//!   the `[presence]` leaf's durable write through the one serialized config
//!   lane (`App::queue_presence_write`), the way Serious Mode persists.
//!
//! Headless (no OS window, the test harness) every row COMPOSES and writes
//! nothing outside the process: the fabric plan is kept on the App, no child
//! runs, no sheet is shown.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config_notice::ConfigNotice;
use crate::menu::{FrontHold, MenuAction};
use crate::platform::AppRt as _;
use crate::presence::HoldFact;
use crate::{App, WindowId};

/// The `[presence]` leaf keys the two View checkables write — the registered
/// settings keys (`prefs::NESTED_LEAVES`), so the config lane admits them.
pub(crate) const PRESENCE_BAND_KEY: &str = crate::prefs::EDIT_PRESENCE_BAND;
pub(crate) const PRESENCE_RIM_KEY: &str = crate::prefs::EDIT_PRESENCE_RIM;

/// The human name of a presence leaf key — the notice's subject.
pub(crate) fn presence_key_label(key: &str) -> &'static str {
    if key == PRESENCE_RIM_KEY {
        "Presence Rim"
    } else {
        "Presence Band"
    }
}

/// A presence leaf key's RESOLVED bit in `config` (absent ⇒ on).
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
                "Runs `aterm fabric on`: starts the broker under launchd, writes the \
                 rendezvous file, arms every running aterm and proves the round trip. \
                 Sessions on this machine can then message each other and be halted \
                 from the fleet.",
                "Turn On",
            )),
            Self::Off => Some((
                "Turn the fabric off?",
                "Runs `aterm fabric off`: stops the broker and undoes the parts of \
                 `aterm fabric on` that change behaviour; the node identity is kept. \
                 Sessions lose their mail lane until it is turned on again.",
                "Turn Off",
            )),
        }
    }
}

/// What a Fabric-menu row asked `aterm` to do: the verb, its argv, and the
/// file its output goes to (opened as a tab when the child ends). Kept on the
/// App (`last_fabric_plan`) so the tests and a `chrome`-side read can see it.
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
    let unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
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

/// The Inbox… tab's text: a heading, what the listing is (and is not), then
/// the `inbox --peek --meta` reply verbatim in a fenced block. The reply's
/// rows carry id, offset, time, sender, kind, trust and the flags — the
/// `--meta` form prints no body, which is the whole point of this surface.
pub(crate) fn inbox_markdown(sid: &str, listing: &str) -> String {
    let mut s = String::with_capacity(listing.len() + 256);
    s.push_str(&format!("# Inbox \u{2014} {sid}\n\n"));
    s.push_str(
        "Metadata only (`inbox --peek --meta`): who wrote, what kind, how trusted, \
         when \u{2014} never the text of a message. Peeked: nothing is marked listed.\n\n",
    );
    s.push_str("```text\n");
    s.push_str(listing.trim_end());
    s.push_str("\n```\n");
    s
}

/// The Fabric Status… / On / Off tab's text: the command, how it ended, then
/// its stdout and stderr in fenced blocks.
fn fabric_markdown(
    plan: &FabricPlan,
    status: &Result<Option<i32>, String>,
    stdout: &str,
    stderr: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", plan.verb.words()));
    s.push_str(&format!("{}\n\n", fabric_status_words(plan.verb, status)));
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

/// One line saying how the child ended — the notice and the tab's subtitle.
fn fabric_status_words(verb: FabricVerb, status: &Result<Option<i32>, String>) -> String {
    match status {
        Ok(Some(0)) => format!("{}: done.", verb.words()),
        Ok(Some(code)) => format!("{} exited {code} \u{2014} see the tab.", verb.words()),
        Ok(None) => format!("{} was killed by a signal.", verb.words()),
        Err(e) => format!("{} could not run: {e}", verb.words()),
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

    /// Set one presence bit LIVE: publish it, re-project every window, and
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
            self.config_notice = ConfigNotice::new(
                vec![format!(
                    "{} was not saved: {error}",
                    presence_key_label(key)
                )],
                Instant::now(),
            );
            self.request_redraw_all_windows();
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
    /// session: the `hold` verb as the Owner, LOCAL origin. The session's own
    /// fabric leaf changes under the same guard and records the same timeline
    /// event a wire `hold` does; the presence band is re-projected here
    /// directly (the fabric wake also arrives, and the refresh is
    /// change-gated). A refusal — a fleet hold stands — is shown, never
    /// silent.
    pub(crate) fn menu_hold(&mut self, wid: WindowId, on: bool) {
        use crate::fabric::{HOLD_DENIED, HoldIssuer, cmd_hold};
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
        let reply = cmd_hold(&self.store, &rest, HoldIssuer::Owner);
        self.refresh_presence_session(session, false);
        self.publish_front_hold(wid);
        self.palette_refresh_live();
        let words = if reply == HOLD_DENIED {
            Some(
                "This session is held by the fleet: the hold is the bridge's to lift, not this window's."
                    .to_string(),
            )
        } else {
            reply
                .strip_prefix("ERR ")
                .map(|err| format!("Hold: {}", err.trim_end()))
        };
        if let Some(words) = words {
            self.config_notice = ConfigNotice::new(vec![words], Instant::now());
            self.request_redraw_all_windows();
        }
    }

    /// Inbox…: the focused session's `inbox --peek --meta` as a Markdown tab
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
            Err(e) => self.menu_notice(format!("Inbox: could not write {}: {e}", path.display())),
        }
    }

    /// Open a file this menu wrote as a Markdown tab on `wid` (the same
    /// admission every Open Markdown… takes), remembering it for the tests.
    fn open_menu_document(&mut self, wid: WindowId, path: PathBuf) {
        self.last_menu_document = Some(path.clone());
        let uri = match crate::native_document_host::path_to_file_uri(&path) {
            Ok(uri) => uri,
            Err(e) => {
                self.menu_notice(format!("could not open {}: {e}", path.display()));
                return;
            }
        };
        if let Err(e) =
            self.request_document_tab_in_window(wid, crate::native_app::AppKind::Markdown, &uri)
        {
            self.menu_notice(format!("could not open {}: {e}", path.display()));
        }
    }

    fn menu_notice(&mut self, words: String) {
        self.config_notice = ConfigNotice::new(vec![words], Instant::now());
        self.request_redraw_all_windows();
    }

    /// Fabric Status… / Turn Fabric On… / Turn Fabric Off…: confirm when the
    /// verb changes something, compose the plan, and — on a real window — run
    /// `aterm fabric …` as a child on its own thread, pointed at THIS
    /// instance's control socket, opening its output as a tab when it ends.
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
                    self.menu_notice(format!(
                        "No confirmation dialog on this platform: run `{}` in a shell.",
                        verb.words()
                    ));
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
        let sock = crate::proxy::self_sock_path();
        let spawned = std::thread::Builder::new()
            .name("aterm-fabric-menu".to_string())
            .spawn(move || {
                // Waits on a CLI child the human is not blocked on.
                crate::qos::set_self(crate::qos::Role::Background);
                let cli = crate::ledger_key::aterm_cli();
                let mut cmd = std::process::Command::new(&cli);
                cmd.args(&plan.argv);
                if let Some(sock) = sock {
                    cmd.env("ATERM_CONTROL_SOCK", sock);
                }
                let (status, stdout, stderr) = match cmd.output() {
                    Ok(out) => (
                        Ok(out.status.code()),
                        String::from_utf8_lossy(&out.stdout).into_owned(),
                        String::from_utf8_lossy(&out.stderr).into_owned(),
                    ),
                    Err(e) => (
                        Err(format!("cannot run {}: {e}", cli.display())),
                        String::new(),
                        String::new(),
                    ),
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
            });
        if let Err(e) = spawned {
            self.menu_notice(format!("{}: cannot spawn its thread: {e}", verb.words()));
        }
    }

    /// The child ended (`Wake::FabricCli`): say how, and open its output as a
    /// tab on the window that asked — or on the front window if that one is
    /// gone.
    pub(crate) fn complete_fabric_cli(&mut self, outcome: FabricCliOutcome) {
        let words = fabric_status_words(outcome.plan.verb, &outcome.status);
        self.config_notice = ConfigNotice::new(vec![words], Instant::now());
        self.request_redraw_all_windows();
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
    //! SPEC19 §9's tests for the Fabric menu's App side: Hold/Lift greyed with
    //! the fleet reason under a fleet hold (and live in the right order
    //! otherwise), Inbox… opening a metadata-only tab, Set Role… writing the
    //! role, the presence toggles gating the projection and queueing their
    //! durable write, and the fabric commands composing without running.

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

    /// THE HEADLESS HOLD TEST (SPEC19 §9): under a FLEET hold both halt rows
    /// are disabled — on the palette mirror AND on the native validate's
    /// projection — and carry the fleet's reason; a menu click that reaches
    /// `menu_hold` anyway is refused and the hold stands. Under no hold, Hold
    /// is live and a click holds LOCALLY (the band says so); then Lift is the
    /// live one and a click lifts.
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
            assert!(
                r.label
                    .contains("fleet hold: main broken, cannot be lifted here"),
                "{a:?}: {}",
                r.label
            );
        }
        // A click that slipped past the grey: refused, shown, the hold stands.
        app.menu_hold(wid, false);
        assert!(ctx.fabric.hold().is_some_and(|h| h.origin == "fleet"));
        assert!(
            app.config_notice
                .as_ref()
                .is_some_and(|n| n.lines.iter().any(|l| l.contains("held by the fleet"))),
            "the refusal is said"
        );
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        app.on_presence_wake(&ctx.self_id, false);
        app.config_notice = None;

        // Hold This Session: a LOCAL hold, the band's stop rim, Lift now live.
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
        assert_eq!(
            row(&app, wid, MenuAction::LiftHold).label,
            "Lift Hold (This Session)"
        );
        assert!(app.config_notice.is_none(), "no notice on success");

        // Lift Hold: gone, and the rows swap back.
        app.menu_hold(wid, false);
        assert!(ctx.fabric.hold().is_none());
        assert_ne!(app.presence_level(wid), Level::Hold);
        assert_eq!(app.front_hold_fact(wid), None);
        assert!(row(&app, wid, MenuAction::HoldSession).enabled);
        assert!(!row(&app, wid, MenuAction::LiftHold).enabled);
        let _ = sid;
    }

    /// Inbox…: a delivered task appears in the tab's file by id, sender, kind
    /// and trust — and its TEXT does not. The tab is a Markdown document on
    /// the window; the row is greyed with no session.
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
        assert!(text.starts_with(&format!("# Inbox \u{2014} {}", ctx.self_id.as_str())));
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
        // (The native checkmark's static is asserted in `menu.rs`'s own test:
        // every App in this test binary publishes through it, so it cannot be
        // read back reliably from here while the others run.)

        // Rim off: none painted; the a11y level still says hold.
        app.user_toggle_presence(MenuAction::TogglePresenceRim);
        assert!(!app.presence_rim_on());
        assert_eq!(app.presence_report(wid).unwrap().1, "none");
        assert!(
            app.presence_overlay(wid, std::time::Instant::now())
                .is_none()
        );

        // Headless has no event-loop proxy: the durable write cannot be queued
        // and the notice says so; the live flip stands.
        assert!(
            app.config_notice
                .as_ref()
                .is_some_and(|n| n.lines.iter().any(|l| l.contains("was not saved"))),
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
        assert_eq!(presence_key_label(PRESENCE_RIM_KEY), "Presence Rim");
    }

    /// The fabric commands compose the documented argv and, headless, run
    /// nothing: no child, no sheet, no file.
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
        assert_eq!(FabricVerb::On.words(), "aterm fabric on");
        assert!(FabricVerb::Status.confirmation().is_none());
        assert!(FabricVerb::Off.confirmation().is_some());
        assert_eq!(
            fabric_status_words(FabricVerb::On, &Ok(Some(1))),
            "aterm fabric on exited 1 \u{2014} see the tab."
        );
    }

    /// The Inbox… document's text is the listing, verbatim, fenced.
    #[test]
    fn inbox_markdown_fences_the_listing_verbatim() {
        let md = inbox_markdown(
            "s-abc",
            "OK 1 hold=0\nmsg 1 off=1 t=5 from=x kind=note trust=agent\n",
        );
        assert!(md.starts_with("# Inbox \u{2014} s-abc\n"));
        assert!(md.ends_with(
            "```text\nOK 1 hold=0\nmsg 1 off=1 t=5 from=x kind=note trust=agent\n```\n"
        ));
    }
}
