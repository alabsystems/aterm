// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable Messages and FaceTime sign-out for local initialization.

use crate::items::{Ctx, Item};
use crate::model::{PValue, Probe, User};
use crate::plist::{self, Entries};
use crate::port::{AccessibilityWalk, AutomationConsent, Cmd, RunFailed, SessionReach, System};
use aterm_types::macos_setup::{Selection, Setting};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const STORE: &str = "Library/IdentityServices/Persistence/accounts.data";
const GZIP: &str = aterm_uds::os_path!("gzip");
const PLUTIL: &str = aterm_uds::os_path!("plutil");
const OSASCRIPT: &str = aterm_uds::os_path!("osascript");
const OPEN: &str = aterm_uds::os_path!("open");

type AccountItem = (Setting, Box<dyn Item>);

#[derive(Clone, Copy)]
enum Service {
    Messages,
    FaceTime,
}

impl Service {
    const fn name(self) -> &'static str {
        match self {
            Self::Messages => "Messages",
            Self::FaceTime => "FaceTime",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::FaceTime => "facetime",
        }
    }

    const fn identifier(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.madrid",
            Self::FaceTime => "com.apple.ess",
        }
    }

    const fn domain(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.imservice.ids.iMessage",
            Self::FaceTime => "com.apple.imservice.ids.FaceTime",
        }
    }

    /// The app's bundle id, which `open -b` brings forward.
    const fn bundle(self) -> &'static str {
        match self {
            Self::Messages => "com.apple.MobileSMS",
            Self::FaceTime => "com.apple.FaceTime",
        }
    }

    /// Where its Sign Out is, as a person finds it (English names).
    const fn path(self) -> &'static str {
        match self {
            Self::Messages => "Messages \u{25b8} Settings\u{2026} \u{25b8} iMessage",
            Self::FaceTime => "FaceTime \u{25b8} Settings\u{2026} \u{25b8} General",
        }
    }
}

/// How often a run looks again, in this process: for Accessibility once macOS asked for it,
/// and for the account to deregister once Sign Out was clicked.
const POLL: Duration = Duration::from_millis(250);

/// How long a run at a terminal waits for a person: to switch Accessibility on once macOS
/// asked, to answer macOS's Automation question, and to click Sign Out by hand where
/// Accessibility stays off. A run with no terminal waits for none of them.
const PATIENCE: Duration = Duration::from_secs(2 * 60);

/// How long System Events, started for macOS to answer whether it may be controlled
/// ([`System::automation_consent`] reads nothing of a target that is not running), takes at most
/// to be read.
const SYSTEM_EVENTS_START: Duration = Duration::from_secs(5);

/// System Events' bundle id, which `open -b` starts (a background app: nothing comes on screen).
const SYSTEM_EVENTS: &str = "com.apple.systemevents";

/// How long the clicks and the deregistration that follows them may take.
const SIGN_OUT_DEADLINE: Duration = Duration::from_secs(60);

/// Why the sign-out clicked nothing while another macOS dialog or page this process started was
/// up: its clicks never come up beside one. It is a wait that passes by itself ([`LOOKS_AGAIN`]):
/// the worker looks again the moment that dialog ends, and on its own schedule.
pub(crate) const DIALOG_UP: &str = "another macOS dialog or page of aterm's was up, so nothing was \
                                   clicked; aterm looks again by itself";

/// Why it waits for macOS's Automation permission to control System Events, which the person
/// (or a standing answer) refused: System Settings lists that switch.
const AUTOMATION_OFF: &str = "macOS Automation permission to control System Events is off: \
                              switch it on in System Settings \u{25b8} Privacy & Security \
                              \u{25b8} Automation, then run `aterm init --sign-out`";

/// How the words of a sign-out that waits on what passes by itself end ([`looks_again`]): a
/// locked screen, another login, the keys in use, or a macOS question nobody answered yet. It
/// clicked nothing, counts no failed try, and its next pass looks again on its own schedule.
const LOOKS_AGAIN: &str = "aterm looks again by itself";

/// Why it waits while macOS has had no answer to its Automation question for System Events, or
/// none aterm could read: no System Settings switch exists for it yet, so the words name none,
/// and the window asks the question ([`aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED`]).
fn automation_unanswered(consent: AutomationConsent) -> String {
    let what = if consent == AutomationConsent::Unanswered {
        "macOS has had no answer yet to whether aterm may control System Events"
    } else {
        "macOS's answer to whether aterm may control System Events has not been read"
    };
    format!(
        "{what}, which the sign-out's clicks go through; {}, and {LOOKS_AGAIN}",
        aterm_types::macos_setup::StageRow::AUTOMATION_UNANSWERED
    )
}

/// Why a sign-out waits when a coding agent started the run: its macOS questions, the app it
/// brings forward and its clicks would land on the screen of a person who asked for none of
/// them (owner ruling 2026-11-09).
pub(crate) fn agents_run() -> String {
    "a coding agent started this run, not a person; signing out waits for you".into()
}

/// Why a sign-out waits when the aterm window refused its Accessibility walk: the run's tab is
/// not the one in front of the person ([`AccessibilityWalk::Behind`]).
pub(crate) fn tab_behind() -> String {
    "this run's tab is not the one in front of you; signing out waits for you".into()
}

/// Why a sign-out put nothing on the screen and waits for the person to start it
/// ([`waits_for_the_person`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waits {
    /// A coding agent started the run ([`System::agent_ancestor`]).
    AgentsRun,
    /// The run's tab was not the one in front ([`AccessibilityWalk::Behind`]).
    TabBehind,
}

/// Whether `row` is a sign-out that put nothing on the screen because the run was not the
/// person's to show them (a coding agent's, [`agents_run`]; a tab behind, [`tab_behind`]), and
/// which: it asked and clicked nothing, and waits for the person to start it.
#[must_use]
pub fn waits_for_the_person(row: &crate::model::ItemReport) -> Option<Waits> {
    let Some(aterm_converge::StoodAside::Waiting(why)) = &row.stood_aside else {
        return None;
    };
    if !signs_out(&row.id) {
        return None;
    }
    if *why == agents_run() {
        Some(Waits::AgentsRun)
    } else if *why == tab_behind() {
        Some(Waits::TabBehind)
    } else {
        None
    }
}

/// Why a sign-out of `service` clicked nothing for `user`'s session (`why`,
/// [`SessionReach::waits`]).
fn session_wait(service: Service, why: &str) -> String {
    format!(
        "{why}, so nothing was clicked in {}: it signs out once this login's screen is unlocked \
         with the keyboard and pointer at rest, and {LOOKS_AGAIN}",
        service.name()
    )
}

/// Why `user`'s session cannot take a sign-out's clicks now ([`System::session_reach`]), or
/// `None`. At a person's own terminal the person who ran the command is there and asked for the
/// clicks, so their own keys there hold nothing; a lock or another login still does. A coding
/// agent's terminal is no person's: the keys in use are the person's, typing elsewhere, and the
/// clicks wait for them to rest as an unattended run's do.
fn session_waits(sys: &dyn System, user: &User) -> Option<&'static str> {
    match sys.session_reach(user) {
        SessionReach::KeysInUse if sys.has_tty() && sys.agent_ancestor().is_none() => None,
        reach => reach.waits(),
    }
}

/// Why a sign-out of `service` stands aside where the app macOS holds responsible for this run
/// has no Accessibility, which its clicks need: System Settings lists that switch, and the
/// grant's access-record line wakes the run once it is on.
fn no_accessibility(sys: &dyn System, service: Service) -> aterm_converge::StoodAside {
    aterm_converge::StoodAside::Permission(format!(
        "{} has no Accessibility, which clicking {}'s Sign Out needs: switch it on in System \
         Settings \u{25b8} Privacy & Security \u{25b8} Accessibility, then run `aterm init \
         --sign-out`",
        responsible(sys),
        service.name()
    ))
}

/// Whether `row` is a sign-out that stood aside on what passes by itself ([`LOOKS_AGAIN`]):
/// its next try is a quiet look on a schedule of its own, never a failed try.
#[must_use]
pub fn looks_again(row: &crate::model::ItemReport) -> bool {
    signs_out(&row.id)
        && matches!(
            &row.stood_aside,
            Some(aterm_converge::StoodAside::Waiting(why)) if why.ends_with(LOOKS_AGAIN)
        )
}

/// The one Apple event that asks macOS's Automation question for System Events before any app
/// opens. It has to reach System Events: AppleScript answers an application's `name`, `id`,
/// `version` and `running` itself, sending nothing, so asking for one of those asks macOS
/// nothing.
const AUTOMATION_ASK: &str = "tell application id \"com.apple.systemevents\" to count processes";

/// The run's Accessibility request and Automation answer, shared by its sign-out items: each is
/// asked for (or read) once, and neither wait repeats for the second app.
#[derive(Default)]
struct Consent {
    asked: Mutex<bool>,
    automation: Mutex<Option<AutomationConsent>>,
}

struct SignOut {
    service: Service,
    user: User,
    /// Why the last apply stood aside before signing out, if it did.
    blocked: Mutex<Option<aterm_converge::StoodAside>>,
    /// Whether the last probe found the account signed in, for System State.
    reading: crate::init_observe::LastReading,
    /// The run's one Accessibility request, shared with its other sign-out.
    consent: std::sync::Arc<Consent>,
}

/// The two account items for each selected local user.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    selected_items(ctx, Selection::all())
}

/// What every sign-out's id holds between its service and its user ([`signs_out`]).
const SIGNED_OUT: &str = ".signed-out@";

/// Whether the item `id` is one of the Messages and FaceTime sign-outs.
#[must_use]
pub fn signs_out(id: &str) -> bool {
    id.starts_with("init.") && id.contains(SIGNED_OUT)
}

/// Build only the selected account operations.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn selected_items(ctx: &Ctx, selected: Selection) -> Result<Vec<Box<dyn Item>>, String> {
    Ok(selected_tagged_items(ctx, selected)?
        .into_iter()
        .map(|(_, item)| item)
        .collect())
}

/// [`selected_items`], each sign-out with the setting whose System State row it reports to,
/// tagged as it is built. One call builds every sign-out of a pass: they share its one
/// Accessibility request.
///
/// # Errors
/// A selected user has no absolute home directory.
pub fn selected_tagged_items(ctx: &Ctx, selected: Selection) -> Result<Vec<AccountItem>, String> {
    let mut items: Vec<AccountItem> = Vec::new();
    let consent = std::sync::Arc::new(Consent::default());
    for user in &ctx.users {
        if !Path::new(&user.home).is_absolute() {
            return Err("account sign-out needs an absolute user home".into());
        }
        for (service, setting) in [
            (Service::Messages, Setting::Messages),
            (Service::FaceTime, Setting::FaceTime),
        ] {
            if !selected.contains(setting) {
                continue;
            }
            items.push((
                setting,
                Box::new(SignOut {
                    service,
                    user: user.clone(),
                    blocked: Mutex::new(None),
                    reading: crate::init_observe::LastReading::default(),
                    consent: std::sync::Arc::clone(&consent),
                }),
            ));
        }
    }
    Ok(items)
}

impl Item for SignOut {
    fn id(&self) -> String {
        format!("init.{}{SIGNED_OUT}{}", self.service.id(), self.user.name)
    }

    fn family(&self) -> &'static str {
        "init"
    }

    fn title(&self) -> String {
        format!("{} signed out ({})", self.service.name(), self.user.name)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let registered = registered(sys, &self.user, self.service);
        // Signed in, whether or not this run may sign it out now.
        self.reading
            .set(matches!(registered, Ok(true)).then_some(crate::init_observe::Reading::SignedIn));
        match registered {
            Err(why) => Probe::Unusable { why },
            Ok(false) => Probe::Ok {
                now: "signed out (no persisted, active, or online account)".into(),
            },
            Ok(true) if !ctx.disrupt => Probe::Deferred {
                why: "signing out opens the app's account settings; use --now".into(),
            },
            Ok(true) if ctx.console.as_ref().map(|u| u.uid) != Some(self.user.uid) => {
                Probe::Blocked {
                    why: "sign-out needs this user's active console session".into(),
                }
            }
            Ok(true) => Probe::Change {
                from: "account remains registered (including disabled accounts)".into(),
                to: "signed out".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.stand(None);
        // A person stage (init-on-by-default design §6.4; owner ruling 2026-11-09): its macOS
        // questions, the app it brings forward and its clicks put things on the person's screen,
        // so a run a coding agent started (with a terminal, which would ask macOS below) asks
        // and clicks nothing; the person starts it.
        if sys.agent_ancestor().is_some() {
            self.stand(Some(aterm_converge::StoodAside::Waiting(agents_run())));
            return Ok(());
        }
        let tty = sys.has_tty();
        // With no terminal nothing here asks macOS anything or opens an app for a person: the
        // session, the Accessibility grant and macOS's Automation answer are only read, before
        // the dialog slot, so a dialog of aterm's that is up (the window's own Automation
        // question among them) never holds this worker; the slot is taken for the clicks alone.
        if !tty {
            if self.stands_for_the_session(sys) {
                return Ok(());
            }
            if !accessibility_ready(sys)? {
                self.stand(Some(no_accessibility(sys, self.service)));
                return Ok(());
            }
            if self.stands_for_automation(sys)? {
                return Ok(());
            }
        }
        // macOS's Accessibility and Automation dialogs, an app left open for the person's
        // click, and the clicks themselves each put something of this process's on screen:
        // only one is up at a time (System State design §6). At a terminal the sign-out holds
        // the one slot from its first question to its last click, waiting for it as the person
        // there does; with none it takes the slot only if it is free, and a dialog up is a wait
        // the worker looks at again the moment it ends ([`DIALOG_UP`]).
        let Some(_dialog) = sys.macos_dialog(tty) else {
            self.stand(Some(aterm_converge::StoodAside::Waiting(DIALOG_UP.into())));
            return Ok(());
        };
        if tty {
            if !self.accessible(sys)? {
                // The window's desk door refused the walk: nothing comes forward by hand either.
                if self.stood_aside().is_some() {
                    return Ok(());
                }
                return self.by_hand(sys);
            }
            if self.stands_for_automation(sys)? {
                return Ok(());
            }
        }
        let deadline = Instant::now() + SIGN_OUT_DEADLINE;
        let labels = localized_labels(sys, &self.user, self.service, Some(deadline))?;
        let command = limited(
            signout_cmd(&self.user, self.service, &labels),
            Some(deadline),
        )?;
        // The clicks take the front: only into this login's own screen, unlocked, with the
        // person's keys and pointer at rest, so a click never lands in their work or behind the
        // lock screen. Read right before them, and again after any wait for an update's handoff.
        if self.stands_for_the_session(sys) {
            return Ok(());
        }
        // The admitted setup runs automatically. Keep the update boundary through the click
        // sequence, so an update cannot interrupt an operation that is unsafe to repeat.
        let ran = {
            let held = sys.update_boundary().hold(
                aterm_types::update_boundary::GuardedItem::SignOutSequence,
                || session_waits(sys, &self.user).map_or(Ok(()), Err),
            );
            let _boundary = match held {
                Ok(held) => held,
                Err(why) => {
                    self.stand(Some(aterm_converge::StoodAside::Waiting(session_wait(
                        self.service,
                        why,
                    ))));
                    return Ok(());
                }
            };
            sys.run_complete_stdout(&command)
        }
        .map_err(|_| format!("{} sign-out could not run", self.service.name()))?;
        let outcome = ran.out();
        let outcome = outcome.trim();
        // A click the script could not make, or its app gone from the front, is the session's
        // doing when the session no longer reads open (locked, switched away, the person's own
        // keys): that waits, as a lock found before the clicks does, and is no failed try. Read
        // open still, it is this try's own failure.
        if !(ran.success()
            && matches!(
                outcome,
                "automation-blocked" | "accessibility-blocked" | "sign-out-requested"
            ))
            && self.stands_for_the_session(sys)
        {
            return Ok(());
        }
        match outcome {
            // macOS refused the Apple events (-1743): the Automation switch System Settings
            // lists is off.
            "automation-blocked" if ran.success() => {
                self.stand(Some(aterm_converge::StoodAside::Permission(
                    AUTOMATION_OFF.into(),
                )));
                return Ok(());
            }
            // System Events refused the clicks for want of assistive access (-25211): that is
            // Accessibility, whose switch, and whose grant's wake, are its own.
            "accessibility-blocked" if ran.success() => {
                self.stand(Some(no_accessibility(sys, self.service)));
                return Ok(());
            }
            "not-frontmost" if ran.success() => {
                return Err(format!(
                    "{} left the front while it was being signed out, and nothing more was \
                     clicked: run `aterm init --sign-out` again",
                    self.service.name()
                ));
            }
            "sign-out-requested" if ran.success() => {}
            _ => {
                return Err(format!(
                    "{} account Settings controls were unavailable; sign-out remains unverified: {}",
                    self.service.name(),
                    ran.describe()
                ));
            }
        }
        if self.deregistered(sys, SIGN_OUT_DEADLINE)? {
            return Ok(());
        }
        Err("the account remains registered after Sign Out; verify its account settings".into())
    }

    fn stood_aside(&self) -> Option<aterm_converge::StoodAside> {
        self.blocked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A session that cannot take the clicks stands the sign-out aside before anything is
    /// journaled or asked; [`Self::apply`] reads it again right before its clicks.
    fn stands_aside(&self, sys: &dyn System, _ctx: &Ctx) -> Option<aterm_converge::StoodAside> {
        session_waits(sys, &self.user)
            .map(|why| aterm_converge::StoodAside::Waiting(session_wait(self.service, why)))
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

impl SignOut {
    fn stand(&self, aside: Option<aterm_converge::StoodAside>) {
        *self.blocked.lock().unwrap_or_else(PoisonError::into_inner) = aside;
    }

    /// Whether this login's session cannot take the clicks now ([`session_waits`]): then the
    /// sign-out stands aside, waiting, and looks again by itself ([`session_wait`]).
    fn stands_for_the_session(&self, sys: &dyn System) -> bool {
        let Some(why) = session_waits(sys, &self.user) else {
            return false;
        };
        self.stand(Some(aterm_converge::StoodAside::Waiting(session_wait(
            self.service,
            why,
        ))));
        true
    }

    /// Whether macOS's Automation answer for System Events ([`Self::automation`]) keeps the
    /// clicks from going: refused, the sign-out stands aside on the switch System Settings
    /// lists; unanswered or unread, it waits, naming no switch, and looks again by itself.
    fn stands_for_automation(&self, sys: &dyn System) -> Result<bool, String> {
        let aside = match self.automation(sys)? {
            AutomationConsent::Allowed => return Ok(false),
            AutomationConsent::Refused => {
                aterm_converge::StoodAside::Permission(AUTOMATION_OFF.into())
            }
            unanswered @ (AutomationConsent::Unanswered | AutomationConsent::Unread) => {
                aterm_converge::StoodAside::Waiting(automation_unanswered(unanswered))
            }
        };
        self.stand(Some(aside));
        Ok(true)
    }

    /// Whether the app macOS holds responsible for this run holds Accessibility, which the
    /// clicks need. A run at a terminal that finds it off asks once for the run: through the
    /// aterm window's permission walk when that window is the app responsible for the run (its
    /// panel beside System Settings, [`System::walk_accessibility`]), otherwise macOS's own
    /// consent dialog; then it reads the grant again every [`POLL`] in this process for at most
    /// [`PATIENCE`], going on the moment it is on. The window's answer can hold the ask back:
    /// the owner's Not now on the walk's Accessibility page asks nothing and goes on without it,
    /// and a walk showing other pages gets no macOS dialog beside them, only the reads. A later
    /// sign-out of the same run does not ask or wait again. Anywhere else (System State, which
    /// asks for it itself) nothing is asked.
    fn accessible(&self, sys: &dyn System) -> Result<bool, String> {
        if accessibility_ready(sys)? {
            return Ok(true);
        }
        if !sys.has_tty() {
            return Ok(false);
        }
        {
            let mut asked = self
                .consent
                .asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *asked {
                return Ok(false);
            }
            *asked = true;
        }
        match sys.walk_accessibility() {
            AccessibilityWalk::Taken => say(&format!(
                "signing out of {} clicks its Sign Out, which needs Accessibility for {}: the \
                 aterm window's panel shows where to switch it on (System Settings \u{25b8} \
                 Privacy & Security \u{25b8} Accessibility), and init goes on the moment it is \
                 on (Ctrl-C stops waiting)",
                self.service.name(),
                responsible(sys)
            )),
            AccessibilityWalk::NotNow => {
                say(&format!(
                    "signing out of {} clicks its Sign Out, which needs Accessibility for {}: \
                     you said Not now to it in the aterm window, so init asks nothing for it \
                     (System State's Accessibility row opens it again)",
                    self.service.name(),
                    responsible(sys)
                ));
                return Ok(false);
            }
            AccessibilityWalk::OtherPages => say(&format!(
                "signing out of {} clicks its Sign Out, which needs Accessibility for {}: the \
                 aterm window's permission walk has other pages up beside System Settings, or \
                 had within the last minute, so init raises no macOS dialog beside them; switch \
                 it on in System Settings \u{25b8} Privacy & Security \u{25b8} Accessibility \
                 (System State's Accessibility row opens it), and init goes on the moment it is \
                 on (Ctrl-C stops waiting)",
                self.service.name(),
                responsible(sys)
            )),
            AccessibilityWalk::Behind => {
                say(&format!(
                    "signing out of {} clicks its Sign Out, which needs Accessibility for {}: \
                     this tab is not the one in front of you, so init brings nothing up for it; \
                     run `aterm init --sign-out` from the tab you are looking at",
                    self.service.name(),
                    responsible(sys)
                ));
                self.stand(Some(aterm_converge::StoodAside::Waiting(tab_behind())));
                return Ok(false);
            }
            AccessibilityWalk::NotTaken => {
                say(&format!(
                    "signing out of {} clicks its Sign Out, which needs Accessibility for {}: \
                     switch it on in macOS's dialog (System Settings \u{25b8} Privacy & \
                     Security \u{25b8} Accessibility), and init goes on the moment it is on \
                     (Ctrl-C stops waiting)",
                    self.service.name(),
                    responsible(sys)
                ));
                if request_accessibility(sys)? {
                    return Ok(true);
                }
            }
        }
        for _ in 0..ticks(PATIENCE) {
            if sys.stopped().is_some() {
                return Ok(false);
            }
            sys.sleep(POLL);
            if accessibility_ready(sys)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Where Accessibility stays off: at a terminal, the app comes forward and the person
    /// clicks Sign Out themselves (aterm cannot open its Settings without Accessibility), and
    /// the run reads the account every [`POLL`] for at most [`PATIENCE`], going on once it
    /// deregisters. Otherwise, or once that wait ends, it stands aside naming the app the grant
    /// goes to.
    fn by_hand(&self, sys: &dyn System) -> Result<(), String> {
        let refused = no_accessibility(sys, self.service);
        if !sys.has_tty() {
            self.stand(Some(refused));
            return Ok(());
        }
        let open = Cmd::new(OPEN)
            .args(["-b", self.service.bundle()])
            .as_user(&self.user)
            .in_session();
        if !sys.run(&open).is_ok_and(|ran| ran.success()) {
            self.stand(Some(refused));
            return Ok(());
        }
        say(&format!(
            "{} is open: choose {} and click Sign Out, and init goes on once it is signed out \
             (Ctrl-C stops waiting)",
            self.service.name(),
            self.service.path()
        ));
        if self.deregistered(sys, PATIENCE)? {
            return Ok(());
        }
        self.stand(Some(refused));
        Ok(())
    }

    /// macOS's Automation answer for System Events, read once for both sign-outs before any app
    /// opens and without asking ([`System::automation_consent`]); System Events, a background app,
    /// is started first where it is not running, since macOS reads nothing of a target that is
    /// not. Only a run at a terminal asks macOS's question itself, where neither answer is on
    /// record, and waits up to [`PATIENCE`] for the person there: a question nobody answered in
    /// that time is still [`AutomationConsent::Unanswered`], never a refusal.
    fn automation(&self, sys: &dyn System) -> Result<AutomationConsent, String> {
        let mut answer = self
            .consent
            .automation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(answer) = *answer {
            return Ok(answer);
        }
        let mut read = sys.automation_consent();
        if read == AutomationConsent::Unread {
            let start = Cmd::new(OPEN)
                .args(["-g", "-j", "-b", SYSTEM_EVENTS])
                .as_user(&self.user)
                .in_session();
            if sys.run(&start).is_ok_and(|ran| ran.success()) {
                for _ in 0..ticks(SYSTEM_EVENTS_START) {
                    sys.sleep(POLL);
                    read = sys.automation_consent();
                    if read != AutomationConsent::Unread {
                        break;
                    }
                }
            }
        }
        if matches!(
            read,
            AutomationConsent::Unanswered | AutomationConsent::Unread
        ) && sys.has_tty()
        {
            read = self.ask_automation(sys)?;
        }
        *answer = Some(read);
        Ok(read)
    }

    /// Ask macOS's Automation question for System Events at a terminal, waiting up to
    /// [`PATIENCE`] for the person's answer.
    fn ask_automation(&self, sys: &dyn System) -> Result<AutomationConsent, String> {
        say(&format!(
            "signing out of {} clicks its Sign Out through System Events: allow {} in macOS's \
             Automation dialog (System Settings \u{25b8} Privacy & Security \u{25b8} Automation), \
             and init goes on when it is allowed (Ctrl-C stops waiting)",
            self.service.name(),
            responsible(sys)
        ));
        let ask = Cmd::new(OSASCRIPT)
            .args(["-e", AUTOMATION_ASK])
            .as_user(&self.user)
            .in_session()
            .budget(PATIENCE);
        match sys.run(&ask) {
            Ok(ran) if ran.success() => Ok(AutomationConsent::Allowed),
            Ok(ran) if ran.err().contains("-1743") => Ok(AutomationConsent::Refused),
            // osascript's own Apple-event timeout, or the run's: nobody answered in time.
            Ok(ran) if ran.err().contains("-1712") => Ok(AutomationConsent::Unanswered),
            Err(RunFailed::TimedOut(_)) => Ok(AutomationConsent::Unanswered),
            Ok(ran) => Err(format!(
                "the Automation question for System Events did not answer: {}",
                ran.describe()
            )),
            Err(why) => Err(format!(
                "could not ask for Automation permission to control System Events: {why}"
            )),
        }
    }

    /// Whether the account deregistered within `patience`, read every [`POLL`] in this process;
    /// a stop ends the wait.
    fn deregistered(&self, sys: &dyn System, patience: Duration) -> Result<bool, String> {
        let deadline = crate::port::deadline_after(patience);
        for tick in 0..=ticks(patience) {
            // Each reading keeps its own budget; the wait ends at its deadline or its last
            // poll, whichever comes first, never in the middle of a reading.
            if !registered(sys, &self.user, self.service)? {
                return Ok(true);
            }
            if tick == ticks(patience) || Instant::now() >= deadline || sys.stopped().is_some() {
                break;
            }
            sys.sleep(POLL);
        }
        Ok(false)
    }
}

/// The polls in `patience`.
fn ticks(patience: Duration) -> u128 {
    patience.as_millis() / POLL.as_millis()
}

/// The app a grant this run needs goes to ([`System::responsible_app`]), else aterm.
fn responsible(sys: &dyn System) -> String {
    sys.responsible_app().unwrap_or_else(|| "aterm".to_owned())
}

/// One line on this run's terminal while it waits for a person. A terminal gone with its tab
/// is no error.
fn say(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn store_path(user: &User) -> PathBuf {
    Path::new(&user.home).join(STORE)
}

fn registered(sys: &dyn System, user: &User, service: Service) -> Result<bool, String> {
    let path = store_path(user);
    let persisted = match sys.stat(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err("cannot inspect the persisted account store".into()),
        Ok(s) if s.dir || s.symlink => {
            return Err("persisted account store is not a regular file".into());
        }
        Ok(_) => {
            let compressed = sys
                .read_head(&path, 2)
                .map_err(|_| "cannot read the persisted account store header".to_owned())?
                == [0x1f, 0x8b];
            let xml = if compressed {
                let gzip = Cmd::new(GZIP).args(["-dc", "--"]).arg(path).as_user(user);
                let unpacked = sys
                    .run_complete_stdout(&gzip)
                    .map_err(|_| "cannot read the complete persisted account store".to_owned())?;
                if !unpacked.success() {
                    return Err("cannot decompress the persisted account store".into());
                }
                let convert = Cmd::new(PLUTIL)
                    .args(["-convert", "xml1", "-o", "-", "--", "-"])
                    .as_user(user);
                sys.run_complete_stdout_input(&convert, &unpacked.stdout)
                    .map_err(|_| "cannot decode the complete persisted account store".to_owned())?
            } else {
                let convert = Cmd::new(PLUTIL)
                    .args(["-convert", "xml1", "-o", "-", "--"])
                    .arg(&path)
                    .as_user(user);
                sys.run_complete_stdout(&convert)
                    .map_err(|_| "cannot decode the complete persisted account store".to_owned())?
            };
            if !xml.success() {
                return Err("cannot decode the persisted account store".into());
            }
            let entries = plist::parse_domain(&xml.stdout).map_err(|_| {
                "persisted account store is not a supported property list".to_owned()
            })?;
            persisted_registration(&entries, service)?
        }
    };
    let cmd = Cmd::new("/usr/bin/defaults")
        .args(["export", service.domain(), "-"])
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|_| "cannot read the complete account service preferences".to_owned())?;
    if !ran.success() {
        return Err("cannot read account service preferences".into());
    }
    let entries = plist::parse_domain(&ran.stdout)
        .map_err(|_| "account service preferences are not a supported property list".to_owned())?;
    let mut visible = false;
    for key in ["Accounts", "ActiveAccounts", "OnlineAccounts"] {
        if let Some(value) = value(&entries, key)? {
            visible |= match value {
                PValue::Array(a) => !a.is_empty(),
                PValue::Dict(d) if key == "Accounts" => !d.is_empty(),
                _ => {
                    return Err(
                        "account service preferences have an unknown account-list format".into(),
                    );
                }
            };
        }
    }
    Ok(persisted || visible)
}

fn value<'a>(entries: &'a Entries, key: &str) -> Result<Option<&'a PValue>, String> {
    let mut found = entries.iter().filter(|(k, _)| k == key);
    let Some((_, v)) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err("account metadata contains duplicate keys".into());
    }
    v.as_ref()
        .map(Some)
        .map_err(|_| "account metadata has an unsupported value".into())
}

fn persisted_registration(entries: &Entries, service: Service) -> Result<bool, String> {
    let key = format!("com.apple.ids.service.{}", service.identifier());
    let Some(service) = value(entries, &key)? else {
        return Ok(false);
    };
    let PValue::Dict(fields) = service else {
        return Err("persisted service account metadata has an unknown format".into());
    };
    let mut accounts = None;
    let mut enabled = None;
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("Accounts", PValue::Dict(a)) if accounts.is_none() => accounts = Some(a.len()),
            ("EnabledAccounts", PValue::Array(a)) if enabled.is_none() => enabled = Some(a.len()),
            ("Accounts" | "EnabledAccounts", _) => {
                return Err("persisted service account lists have an unknown format".into());
            }
            _ => {}
        }
    }
    match (accounts, enabled) {
        (Some(a), Some(e)) if e <= a => Ok(a != 0),
        _ => Err("persisted service account lists are missing or inconsistent".into()),
    }
}

/// Whether the shared account plan found a registration that needs sign-out.
///
/// # Errors
/// The console account or the shared plan cannot be resolved.
pub fn accessibility_needed(sys: &dyn System) -> Result<bool, String> {
    accessibility_needed_selected(sys, Selection::all())
}

/// Whether a selected account needs sign-out, without reading unchecked accounts.
///
/// # Errors
/// The console account or the selected shared plan cannot be resolved.
pub fn accessibility_needed_selected(
    sys: &dyn System,
    selected: Selection,
) -> Result<bool, String> {
    let rows = crate::init::run_workstation_selected(
        sys,
        crate::init::WorkstationGroup::Accounts,
        crate::model::Mode::Plan,
        selected,
    )?;
    Ok(rows
        .iter()
        .any(|row| row.outcome == crate::model::Outcome::Planned))
}

/// Ask macOS to show its native Accessibility consent once for automatic setup, in this
/// process ([`System::request_accessibility`]), answering the trust as it stands.
///
/// # Errors
/// The trust cannot be asked for here.
pub fn request_accessibility(sys: &dyn System) -> Result<bool, String> {
    sys.request_accessibility()
        .ok_or_else(|| "could not request Accessibility permission for account sign-out".into())
}

/// Inspect Accessibility trust without requesting permission or opening an app, in this
/// process ([`System::accessibility_trusted`]).
///
/// # Errors
/// The trust cannot be read here.
pub fn accessibility_ready(sys: &dyn System) -> Result<bool, String> {
    sys.accessibility_trusted()
        .ok_or_else(|| "could not check Accessibility permission for account sign-out".into())
}

fn limited(cmd: Cmd, deadline: Option<Instant>) -> Result<Cmd, String> {
    let Some(deadline) = deadline else {
        return Ok(cmd);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("account sign-out ran out of its time".into());
    }
    let budget = cmd.budget.min(remaining);
    Ok(cmd.budget(budget))
}

#[derive(Clone)]
struct Labels {
    panes: Vec<String>,
    sign_out: Vec<String>,
}

fn labels_cmd(user: &User, service: Service) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args([
            "-l",
            "JavaScript",
            "-e",
            LOCALIZED_LABELS,
            "--",
            service.name(),
        ])
        .as_user(user)
}

fn localized_labels(
    sys: &dyn System,
    user: &User,
    service: Service,
    deadline: Option<Instant>,
) -> Result<Labels, String> {
    let answer = sys
        .run_complete_stdout(&limited(labels_cmd(user, service), deadline)?)
        .map_err(|_| {
            "could not read account Settings labels from the installed Apple app".to_owned()
        })?;
    if !answer.success() {
        return Err("the installed app's account Settings labels are unsupported".into());
    }
    let value: aterm_json::Value = aterm_json::from_slice(&answer.stdout)
        .map_err(|_| "unrecognized account Settings labels".to_owned())?;
    let read = |key: &str| -> Result<Vec<String>, String> {
        let labels = value[key]
            .as_array()
            .ok_or("account Settings label list is missing")?;
        if labels.is_empty() || labels.len() > 128 {
            return Err("account Settings label list is unsupported".into());
        }
        labels
            .iter()
            .map(|label| {
                label
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 512 && !s.contains(['\r', '\n']))
                    .map(str::to_owned)
                    .ok_or_else(|| "account Settings label is unsupported".into())
            })
            .collect()
    };
    Ok(Labels {
        panes: read("panes")?,
        sign_out: read("sign_out")?,
    })
}

const LOCALIZED_LABELS: &str = r#"ObjC.import('Foundation');
function labels(path, key) {
    const table = $.NSDictionary.dictionaryWithContentsOfFile(path);
    if (!table || table.isNil()) throw Error('label table unavailable');
    const entries = ObjC.deepUnwrap(table);
    const values = Object.values(entries).map(x => x && typeof x === 'object' ? x[key] : null)
        .filter(x => typeof x === 'string' && x.length > 0);
    if (values.length === 0) throw Error('label key unavailable');
    return [...new Set(values)].sort();
}
function run(argv) {
    if (argv[0] === 'Messages') {
        const path = '/System/Applications/Messages.app/Contents/PlugIns/MessagesAppKitBridge.bundle/Contents/Resources/MessagesAppKitBridgeLocalizable.loctable';
        return JSON.stringify({panes: labels(path, 'SETTINGS_TAB_IMESSAGE'), sign_out: labels(path, 'IMESSAGE_SIGN_OUT_BUTTON_SIGN_OUT')});
    }
    if (argv[0] === 'FaceTime') {
        const path = '/System/iOSSupport/System/Library/PrivateFrameworks/FaceTimeSettingsUI.framework/Resources/Localizable.loctable';
        return JSON.stringify({panes: labels(path, 'GENERAL_TAB_TITLE'), sign_out: labels(path, 'SIGN_OUT_BUTTON_TEXT')});
    }
    throw Error('unsupported app');
}"#;

fn signout_cmd(user: &User, service: Service, labels: &Labels) -> Cmd {
    Cmd::new(OSASCRIPT)
        .args([
            "-e",
            SIGN_OUT,
            "--",
            service.name(),
            &labels.panes.join("\n"),
            &labels.sign_out.join("\n"),
        ])
        .as_user(user)
        .in_session()
}

const SIGN_OUT: &str = r#"property stopAt : missing value
on run argv
    set appName to item 1 of argv
    set paneNames to paragraphs of item 2 of argv
    set signOutNames to paragraphs of item 3 of argv
    set deadline to (current date) + 40
    set my stopAt to deadline
    with timeout of 45 seconds
    if appName is not "Messages" and appName is not "FaceTime" then error "unsupported app"
    set settingsWindow to missing value
    set closeSettings to false
    set priorApp to missing value
    set wasRunning to true
    try
        tell application "System Events"
            set priorApp to name of first application process whose frontmost is true
            set wasRunning to exists process appName
        end tell
        tell application appName to activate
        -- Ready once its menu bar is there and it is in front: activation lands a moment after
        -- `activate` returns, and the first click's frontmost check must not race it.
        set settingsReady to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists process appName then
                    tell process appName
                        if (exists menu bar 1) and frontmost then set settingsReady to true
                    end tell
                end if
            end tell
            if settingsReady then exit repeat
            delay 0.1
        end repeat
        if settingsReady is false then
            my mustBeFront(appName)
            error "account Settings menu unavailable"
        end if
        set priorSettings to my accountSettings(appName, paneNames)
        set closeSettings to priorSettings is missing value
        if priorSettings is missing value then
            set settingsItem to missing value
            tell application "System Events"
                tell process appName
                    try
                        repeat with mi in menu items of menu 1 of menu bar item 2 of menu bar 1
                            try
                                if (value of attribute "AXMenuItemCmdChar" of mi) is "," then
                                    set settingsItem to mi
                                    exit repeat
                                end if
                            end try
                        end repeat
                    end try
                end tell
            end tell
            if settingsItem is missing value then error "account Settings menu item unavailable"
            my mustBeFront(appName)
            tell application "System Events" to click settingsItem
        end if
        repeat 120 times
            my checkDeadline(deadline)
            set settingsWindow to my accountSettings(appName, paneNames)
            if settingsWindow is not missing value then exit repeat
            delay 0.1
        end repeat
        if settingsWindow is missing value then error "account Settings window unavailable"
        set paneButton to my namedButton(settingsWindow, paneNames)
        if paneButton is missing value then error "account settings pane unavailable"
        my mustBeFront(appName)
        tell application "System Events" to click paneButton
        set signOutButton to missing value
        repeat 120 times
            my checkDeadline(deadline)
            set signOutButton to my namedButton(settingsWindow, signOutNames)
            if signOutButton is not missing value then exit repeat
            delay 0.1
        end repeat
        if signOutButton is missing value then error "account Sign Out control unavailable"
        my mustBeFront(appName)
        tell application "System Events" to click signOutButton
        set acknowledged to false
        repeat 120 times
            my checkDeadline(deadline)
            tell application "System Events"
                if exists sheet 1 of settingsWindow then
                    set confirmButton to my namedButton(sheet 1 of settingsWindow, signOutNames)
                    if confirmButton is not missing value then
                        my mustBeFront(appName)
                        click confirmButton
                        set acknowledged to true
                    end if
                else if not (exists settingsWindow) then
                    set acknowledged to true
                else if my existingButton(settingsWindow, signOutNames) is missing value then
                    set acknowledged to true
                end if
            end tell
            if acknowledged then exit repeat
            delay 0.1
        end repeat
        if acknowledged is false then error "account Sign Out did not finish"
        my closeOwnedSettings(appName, settingsWindow, closeSettings)
    on error errorText number errorNumber
        my closeOwnedSettings(appName, settingsWindow, closeSettings)
        my restoreApplications(appName, wasRunning, priorApp)
        if errorNumber is -1743 then return "automation-blocked"
        if errorNumber is -25211 then return "accessibility-blocked"
        if errorNumber is 1001 then return "not-frontmost"
        error errorText
    end try
    my restoreApplications(appName, wasRunning, priorApp)
    return "sign-out-requested"
    end timeout
end run

on checkDeadline(deadline)
    if (current date) is greater than deadline then error "account Settings deadline expired"
end checkDeadline

-- Before every click: the app aterm drives is still in front, so a click never lands in a
-- window the person brought forward meanwhile.
on mustBeFront(appName)
    tell application "System Events"
        if not (frontmost of process appName) then error "not frontmost" number 1001
    end tell
end mustBeFront

on restoreApplications(appName, wasRunning, priorApp)
    try
        with timeout of 2 seconds
            if wasRunning is false then tell application appName to quit
        end timeout
    end try
    try
        with timeout of 2 seconds
            if application priorApp is running then tell application priorApp to activate
        end timeout
    end try
end restoreApplications

on accountSettings(appName, paneNames)
    tell application "System Events"
        tell process appName
            repeat with candidateWindow in windows
                my checkDeadline(my stopAt)
                -- Inspect only toolbar controls until the account Settings window is
                -- identified; never traverse a conversation or call-history view.
                repeat with candidateToolbar in toolbars of candidateWindow
                    my checkDeadline(my stopAt)
                    set paneButton to my namedButton(candidateToolbar, paneNames)
                    if paneButton is not missing value then return candidateWindow
                end repeat
            end repeat
        end tell
    end tell
    return missing value
end accountSettings

on namedButton(containerElement, buttonNames)
    set buttonRef to my existingButton(containerElement, buttonNames)
    if buttonRef is missing value then return missing value
    tell application "System Events"
        if enabled of buttonRef then return buttonRef
    end tell
    return missing value
end namedButton

on existingButton(containerElement, buttonNames)
    tell application "System Events"
        set childrenList to {}
        try
            set childrenList to UI elements of containerElement
        end try
        repeat with elementRef in childrenList
            my checkDeadline(my stopAt)
            try
                if role of elementRef is in {"AXButton", "AXRadioButton"} then
                    try
                        if (name of elementRef as string) is in buttonNames then return elementRef
                    end try
                    try
                        if (description of elementRef as string) is in buttonNames then return elementRef
                    end try
                end if
            end try
            set nestedRef to my existingButton(elementRef, buttonNames)
            if nestedRef is not missing value then return nestedRef
        end repeat
    end tell
    return missing value
end existingButton

on closeOwnedSettings(appName, settingsWindow, closeSettings)
    if closeSettings and settingsWindow is not missing value then
        try
            with timeout of 2 seconds
            tell application "System Events"
                if (exists settingsWindow) and (frontmost of process appName) then
                    repeat with closeButton in buttons of settingsWindow
                        if subrole of closeButton is "AXCloseButton" then
                            click closeButton
                            exit repeat
                        end if
                    end repeat
                end if
            end tell
            end timeout
        end try
    end if
end closeOwnedSettings"#;

#[cfg(test)]
#[path = "init_accounts_tests.rs"]
mod tests;
