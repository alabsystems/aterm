// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS `sudo` askpass password sheet attached to the asking session's active window.

use std::sync::mpsc::Sender;
use std::time::Instant;

use aterm_objc::{CGPoint, CGRect, CGSize, Id, Obj, RcBlock, Sel, class, sel};

use crate::appkit::{self, MainThread};
use crate::askpass::AskpassAnswer;
use crate::{App, Wake};

/// Whether any askpass sheet is open (read by `hwkey`).
pub(crate) fn sheets_open() -> bool {
    crate::askpass::PROMPT_INPUT.with_post(|| ()).is_err()
}

/// One outstanding password sheet and the channel back to the waiting helper.
pub(crate) struct AskpassSheet {
    pub id: u64,
    ticket: std::sync::Arc<crate::askpass::AskpassTicket>,
    window: crate::WindowId,
    session: u64,
    owned: Option<std::sync::Arc<crate::cpu_policy_authorization::Operation>>,
    /// When it went up, for the untouched-sheet backstop (`App::refuse_untouched_askpass_sheet`).
    raised: Instant,
    /// Whether a person has touched it (`crate::askpass::PasswordInput::touched`).
    input: std::sync::Arc<crate::askpass::PasswordInput>,
    reply: Sender<AskpassAnswer>,
    /// What AppKit shows of it, dropped last, as its key watch and input fence always were.
    chrome: SheetChrome,
}

/// The AppKit side of a password sheet: its secure field, its panel and the window it hangs
/// from, and the disclosure, key watch and input fence that live as long as it does.
enum SheetChrome {
    Shown {
        field: Obj,
        panel: Obj,
        parent: Obj,
        _details: aterm_objc::Retained<PasswordDetailsTarget>,
        _keys: crate::alert_keys::PasswordKeyWatch,
        _input_fence: crate::askpass::PromptInputFence<'static>,
    },
    /// A headless test's sheet, which no window shows: the update lane's hold and refusal read
    /// only the rest of [`AskpassSheet`].
    #[cfg(test)]
    Headless,
}

impl Drop for AskpassSheet {
    fn drop(&mut self) {
        let (field, panel, parent) = match &self.chrome {
            SheetChrome::Shown {
                field,
                panel,
                parent,
                ..
            } => (field, panel, parent),
            #[cfg(test)]
            SheetChrome::Headless => return,
        };
        unsafe {
            if let Some(empty) = appkit::nsstring("") {
                appkit::send_v_id(field.id(), sel!(setStringValue:), empty.id());
            }
            if appkit::send_id(panel.id(), sel!(sheetParent)) == parent.id() {
                appkit::send_v_id_isize(
                    parent.id(),
                    sel!(endSheet:returnCode:),
                    panel.id(),
                    appkit::consts::NS_ALERT_SECOND_BUTTON_RETURN,
                );
            }
        }
    }
}

#[cfg(test)]
impl AskpassSheet {
    /// A sheet raised at `raised` for `session` in `window` that no window shows, answering on
    /// `reply`, and its input, which a test touches as a person would.
    pub(crate) fn headless_for_test(
        window: crate::WindowId,
        session: u64,
        raised: Instant,
        reply: Sender<AskpassAnswer>,
    ) -> (Self, std::sync::Arc<crate::askpass::PasswordInput>) {
        let input = std::sync::Arc::new(crate::askpass::PasswordInput::new());
        let sheet = Self {
            id: 1,
            ticket: std::sync::Arc::new(crate::askpass::AskpassTicket::new(
                1,
                raised + crate::askpass::ASKPASS_DEADLINE,
            )),
            window,
            session,
            owned: None,
            raised,
            input: std::sync::Arc::clone(&input),
            reply,
            chrome: SheetChrome::Headless,
        };
        (sheet, input)
    }
}

struct PasswordDetails {
    alert: Obj,
    text: String,
    open: std::cell::Cell<bool>,
}

aterm_objc::declare_class! {
    /// The password sheet's disclosure; it never reads or submits the password.
    struct PasswordDetailsTarget: NSObject {
        const NAME: &str = "ATermPasswordDetailsTarget";
        type Ivars = PasswordDetails;

        @sel(toggleDetails:)
        fn toggle_details(&self, sender: Id) {
            let state = self.ivars();
            let open = !state.open.get();
            let text = if open { state.text.as_str() } else { "" };
            let title = if open { "Hide Details" } else { "Details…" };
            let (Some(text), Some(title)) = (appkit::nsstring(text), appkit::nsstring(title)) else {
                return;
            };
            // SAFETY: the target owns this live NSAlert; sender is its NSButton.
            // All access and NSAlert's relayout happen on AppKit's main thread.
            unsafe {
                appkit::send_v_id(state.alert.id(), sel!(setInformativeText:), text.id());
                appkit::send_v_id(sender, sel!(setTitle:), title.id());
                appkit::send_v(state.alert.id(), sel!(layout));
            }
            state.open.set(open);
        }
    }
}

/// Keep the password and disclosure together in NSAlert's accessory. Details start collapsed.
fn password_accessory(
    mtm: MainThread,
    alert: &Obj,
    field: &Obj,
    text: String,
) -> Option<(Obj, aterm_objc::Retained<PasswordDetailsTarget>)> {
    let target = PasswordDetailsTarget::alloc_init(
        mtm,
        PasswordDetails {
            alert: alert.clone_retained(),
            text,
            open: std::cell::Cell::new(false),
        },
    )?;
    let title = appkit::nsstring("Details…")?;
    // SAFETY: newly allocated AppKit views, built on the main thread. The view retains
    // its children; the sheet retains the action target because NSButton does not.
    unsafe {
        let view = Obj::from_owned(appkit::send_id_rect(
            appkit::alloc(class(c"NSView")),
            sel!(initWithFrame:),
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 340.0,
                    height: 60.0,
                },
            },
        ))?;
        let button = Obj::from_owned(appkit::send_id_rect(
            appkit::alloc(class(c"NSButton")),
            sel!(initWithFrame:),
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 120.0,
                    height: 24.0,
                },
            },
        ))?;
        appkit::send_v_id(button.id(), sel!(setTitle:), title.id());
        appkit::send_v_bool(button.id(), sel!(setBordered:), false);
        appkit::send_v_isize(
            button.id(),
            sel!(setAlignment:),
            appkit::consts::NS_TEXT_ALIGNMENT_LEFT,
        );
        appkit::send_v_id(button.id(), sel!(setTarget:), target.as_id());
        appkit::send_v_sel(button.id(), sel!(setAction:), sel!(toggleDetails:));
        appkit::send_v_id(view.id(), sel!(addSubview:), field.id());
        appkit::send_v_id(view.id(), sel!(addSubview:), button.id());
        Some((view, target))
    }
}

impl App {
    /// Attribute, gate, and raise the askpass sheet, or reply with a refusal.
    pub(crate) fn present_askpass_sheet(&mut self, request: crate::askpass::AskpassRequest) {
        let crate::askpass::AskpassRequest {
            ticket,
            attribute_from,
            owned,
            exact,
            command,
            purpose,
            invoker,
            reply,
        } = request;
        if !ticket.is_live(Instant::now()) {
            let _ = reply.send(AskpassAnswer::Refused(
                "request expired or withdrawn".into(),
            ));
            return;
        }

        let (wid, session) = if let Some(operation) = &owned {
            if !self.cpu_policy_origin_live(operation) {
                ticket.cancel();
                let _ = reply.send(AskpassAnswer::Refused(
                    "originating resource panel is gone".into(),
                ));
                return;
            }
            operation.origin()
        } else {
            let sessions: Vec<(u64, i32)> = self.pool.iter().map(|s| (s.id, s.pid)).collect();
            let session = crate::consent_observer::session_of_pid(
                attribute_from,
                &sessions,
                crate::session_status::program::parent_of,
                crate::consent_observer::live_pgid_of,
            );
            let Some(session) = session else {
                let _ = reply.send(AskpassAnswer::Refused("unattributed".into()));
                return;
            };

            let Some(wid) = self.window_of_session(session) else {
                let _ = reply.send(AskpassAnswer::Refused(
                    "no window hosts that session".into(),
                ));
                return;
            };
            (wid, session)
        };
        let now = Instant::now();
        if let Some(rem) = self.askpass_backoff.quiet_for(wid.0, now) {
            let _ = reply.send(AskpassAnswer::Refused(format!(
                "a recent cancel; retry in {}s",
                rem.as_secs() + 1
            )));
            return;
        }
        if self.askpass_sheet.is_some() {
            let _ = reply.send(AskpassAnswer::Refused(
                "a password prompt is already open".into(),
            ));
            return;
        }
        if !self.is_visible_session(session) {
            let _ = reply.send(AskpassAnswer::Refused(
                "not-frontmost; focus the tab and retry".into(),
            ));
            return;
        }
        self.build_askpass_sheet(
            wid,
            session,
            (&command, &invoker, exact, purpose.as_deref()),
            owned,
            ticket,
            reply,
        );
    }

    /// Build and attach the askpass sheet, or reply with a refusal on failure.
    fn build_askpass_sheet(
        &mut self,
        wid: crate::WindowId,
        session: u64,
        (command, invoker, exact, purpose): (&str, &str, bool, Option<&str>),
        owned: Option<std::sync::Arc<crate::cpu_policy_authorization::Operation>>,
        ticket: std::sync::Arc<crate::askpass::AskpassTicket>,
        reply: Sender<AskpassAnswer>,
    ) {
        let refuse = |reply: Sender<AskpassAnswer>, why: &str| {
            let _ = reply.send(AskpassAnswer::Refused(why.to_string()));
        };
        let Some(mtm) = MainThread::new() else {
            return refuse(reply, "headless");
        };
        let Some(proxy) = self.proxy.clone() else {
            return refuse(reply, "no event loop");
        };
        let ns_window = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|w| crate::platform::ns_window_of(w))
            .filter(|id| !id.is_null())
            // SAFETY: a live NSWindow borrowed on the main thread; `-window`
            // returned +0, retained into an owning `Obj` for the sheet.
            .and_then(|id| unsafe { Obj::retain(id) });
        let Some(ns_window) = ns_window else {
            return refuse(reply, "the window is not on screen");
        };
        let Some(app) = appkit::existing_application(mtm) else {
            return refuse(reply, "no AppKit application");
        };

        let id = ticket.id;
        let (message, details) = if owned.is_some() {
            ("Authorize CPU diagnostics".to_owned(),
             "Read CPU scheduling information and save a diagnostic report. Authorization applies to this collection only.".to_owned())
        } else if exact {
            (
                purpose
                    .unwrap_or("Enter your system sudo password to allow this change.")
                    .to_owned(),
                format!(
                    "Session {session}\n\n{command}\n\nAsked by:\n{invoker}\n\nYour password authorizes this one command, not later ones. A shell or script can go on to run anything."
                ),
            )
        } else {
            (
                "Enter your system sudo password to allow this command.".to_owned(),
                format!(
                    "{command}\n\nRun by:\n{invoker}\n\nAllowing lets any sudo from that shell run as root without asking again for about 5 minutes (sudo's own cache), not only the command above."
                ),
            )
        };
        let (Some(message), Some(informative), Some(allow), Some(cancel)) = (
            appkit::nsstring(&message),
            appkit::nsstring(""),
            appkit::nsstring("Allow"),
            appkit::nsstring("Cancel"),
        ) else {
            return refuse(reply, "could not build the sheet text");
        };

        let completion = move |response: isize| {
            let _ = proxy.send_event(Wake::AskpassAnswered {
                id,
                proceed: response == appkit::consts::NS_ALERT_FIRST_BUTTON_RETURN,
            });
        };
        // SAFETY: AppKit invokes the block as `void(^)(NSModalResponse)`; the
        // `(isize)->()` prototype matches (`NSModalResponse` is `NSInteger`),
        // and nothing unwinds out of it.
        let Some(handler) = (unsafe { RcBlock::new1(completion) }) else {
            return refuse(reply, "could not build the sheet handler");
        };

        // An alert whose icon draws with no IconServices call on the main thread.
        let Some(alert) = crate::app_icon::alert(mtm) else {
            return refuse(reply, "could not create the alert");
        };
        // SAFETY: `+[NSSecureTextField alloc]` then `-initWithFrame:` is +1.
        let field = unsafe {
            let frame = CGRect {
                origin: CGPoint { x: 0.0, y: 32.0 },
                size: CGSize {
                    width: 340.0,
                    height: 24.0,
                },
            };
            Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSSecureTextField")),
                sel!(initWithFrame:),
                frame,
            ))
        };
        let Some(field) = field else {
            return refuse(reply, "could not create the password field");
        };

        let Some((accessory, details_target)) = password_accessory(mtm, &alert, &field, details)
        else {
            return refuse(reply, "could not create the details disclosure");
        };

        let input = std::sync::Arc::new(crate::askpass::PasswordInput::new());
        // SAFETY: standard NSAlert setters on the main thread; text setters
        // copy, `-addButtonWithTitle:` returns +0, `-setAccessoryView:` retains
        // the field, and `-beginSheetModalForWindow:completionHandler:` copies
        // the block. The field and panel are retained for the App to hold.
        let (panel, input_fence) = crate::askpass::PROMPT_INPUT.present(|| unsafe {
            if !ticket.is_live(Instant::now()) {
                return Err("request expired or withdrawn");
            }
            let past = appkit::send_id(class(c"NSDate").as_id(), sel!(distantPast));
            if past.is_null() {
                return Err("could not establish the key-queue deadline");
            }
            let mode = Id::from_ptr(aterm_objc::cf::kCFRunLoopDefaultMode.cast_mut().cast());
            let next: unsafe extern "C-unwind" fn(Id, Sel, usize, Id, Id, aterm_objc::Bool) -> Id =
                aterm_objc::checked_msg();
            let mut drained = false;
            for _ in 0..16_384 {
                if next(
                    app,
                    sel!(nextEventMatchingMask:untilDate:inMode:dequeue:),
                    (1 << 10) | (1 << 11) | (1 << 12),
                    past,
                    mode,
                    aterm_objc::Bool::YES,
                )
                .is_null()
                {
                    drained = true;
                    break;
                }
            }
            if !drained {
                return Err("too many queued key events; retry after input settles");
            }
            if !ticket.is_live(Instant::now()) {
                return Err("request expired or withdrawn");
            }
            appkit::send_v_id(alert.id(), sel!(setMessageText:), message.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), informative.id());
            appkit::send_v_id(alert.id(), sel!(setAccessoryView:), accessory.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), allow.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), cancel.id());
            let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)));
            let Some(panel) = panel else {
                return Err("the alert reported no panel");
            };
            let Some(keys) = crate::alert_keys::watch_password_keys(
                &panel,
                &ns_window,
                &field,
                std::sync::Arc::clone(&input),
            ) else {
                return Err("could not guard the password field's input");
            };
            let begin_sheet: unsafe extern "C-unwind" fn(Id, Sel, Id, aterm_objc::BlockPtr) =
                aterm_objc::checked_msg();
            begin_sheet(
                alert.id(),
                sel!(beginSheetModalForWindow:completionHandler:),
                ns_window.id(),
                handler.as_block_ptr(),
            );
            let _ = appkit::send_bool_id(panel.id(), sel!(makeFirstResponder:), Id::NIL);
            Ok((panel, keys))
        });
        let (panel, keys) = match panel {
            Ok(result) => result,
            Err(why) => return refuse(reply, why),
        };

        self.askpass_sheet = Some(AskpassSheet {
            id,
            ticket,
            window: wid,
            session,
            owned,
            raised: Instant::now(),
            input,
            reply,
            chrome: SheetChrome::Shown {
                field,
                panel,
                parent: ns_window,
                _details: details_target,
                _keys: keys,
                _input_fence: input_fence,
            },
        });
    }

    /// Read the answered secure field once, reply with the secret or cancel, and update backoff.
    pub(crate) fn answer_askpass_sheet(&mut self, id: u64, proceed: bool) {
        if self.askpass_sheet.as_ref().is_none_or(|s| s.id != id) {
            return;
        }
        let sheet = self.askpass_sheet.take().expect("checked just above");
        let now = Instant::now();
        if !sheet.ticket.is_live(now)
            || sheet
                .owned
                .as_ref()
                .is_some_and(|op| !self.cpu_policy_origin_live(op))
        {
            sheet.ticket.cancel();
            return;
        }
        if proceed {
            let field = match &sheet.chrome {
                SheetChrome::Shown { field, .. } => Some(field),
                #[cfg(test)]
                SheetChrome::Headless => None,
            };
            let Some(secret) = field.and_then(read_secure_field) else {
                let _ = sheet.reply.send(AskpassAnswer::Refused(
                    "the password is invalid or longer than 1023 UTF-8 bytes".into(),
                ));
                return;
            };
            if !sheet.ticket.is_live(Instant::now())
                || sheet
                    .owned
                    .as_ref()
                    .is_some_and(|op| !self.cpu_policy_origin_live(op))
            {
                sheet.ticket.cancel();
                return;
            }
            if secret.is_empty() {
                let _ = sheet.reply.send(AskpassAnswer::Cancel);
                self.askpass_backoff.on_cancel(sheet.window.0, now);
            } else {
                if sheet.owned.as_ref().is_some_and(|op| !op.release_secret()) {
                    sheet.ticket.cancel();
                    return;
                }
                let _ = sheet.reply.send(AskpassAnswer::Secret(secret));
                self.askpass_backoff.on_answer(sheet.window.0);
            }
        } else {
            let _ = sheet.reply.send(AskpassAnswer::Cancel);
            self.askpass_backoff.on_cancel(sheet.window.0, now);
        }
    }

    /// Whether aterm's own password sheet stands, and if it does, whether it has stood untouched
    /// for [`crate::askpass::ASKPASS_UNTOUCHED_BACKSTOP`].
    pub(crate) fn askpass_sheet_untouched_overdue(&self, now: Instant) -> Option<bool> {
        self.askpass_sheet.as_ref().map(|sheet| {
            crate::askpass::untouched_sheet_overdue(sheet.raised, sheet.input.touched(), now)
        })
    }

    /// Refuse aterm's own password sheet when an update waits on it and it has stood untouched
    /// for [`crate::askpass::ASKPASS_UNTOUCHED_BACKSTOP`]: an unanswered prompt is recorded and
    /// the rest goes on. The `sudo` that asked is told why and may ask again once the window's
    /// decline spacing passes; a sheet a person has touched is never refused here. Returns
    /// whether it refused one.
    pub(crate) fn refuse_untouched_askpass_sheet(&mut self, now: Instant) -> bool {
        let Some(sheet) = self.askpass_sheet.take_if(|sheet| {
            crate::askpass::untouched_sheet_overdue(sheet.raised, sheet.input.touched(), now)
        }) else {
            return false;
        };
        let backstop = crate::askpass::ASKPASS_UNTOUCHED_BACKSTOP.as_secs();
        aterm_log::info!(
            "update: session {}'s password prompt stood untouched for {backstop}s while an \
             update waited; aterm refused it and the update goes on",
            sheet.session
        );
        let _ = sheet.reply.send(AskpassAnswer::Refused(format!(
            "nobody answered it within {backstop}s while an update waited; ask again"
        )));
        self.askpass_backoff.on_cancel(sheet.window.0, now);
        true
    }

    pub(crate) fn withdraw_askpass_sheet(&mut self, id: u64) {
        if let Some(sheet) = self.askpass_sheet.take_if(|sheet| sheet.id == id) {
            sheet.ticket.cancel();
        }
    }

    pub(crate) fn withdraw_askpass_window(&mut self, wid: crate::WindowId) {
        crate::cpu_policy_authorization::cancel_window(wid);
        if let Some(sheet) = self.askpass_sheet.take_if(|sheet| sheet.window == wid) {
            sheet.ticket.cancel();
        }
    }

    pub(crate) fn withdraw_askpass_session(&mut self, session: u64) {
        crate::cpu_policy_authorization::cancel_session(session);
        if let Some(sheet) = self.askpass_sheet.take_if(|sheet| sheet.session == session) {
            sheet.ticket.cancel();
        }
    }
}

/// Extract the secure field's text directly into guarded UTF-16 and UTF-8 buffers.
pub(crate) fn read_secure_field(field: &Obj) -> Option<aterm_uds::sudo::Secret> {
    // SAFETY: the caller owns this retained NSSecureTextField on the main
    // thread. Its NSString is borrowed through this synchronous read; the exact
    // counted range writes into initialized, bounded, scrubbed UTF-16 storage.
    unsafe {
        let value = appkit::send_id(field.id(), sel!(stringValue));
        if value.is_null() {
            return None;
        }
        let length: unsafe extern "C-unwind" fn(Id, Sel) -> usize = aterm_objc::checked_msg();
        let len = length(value, sel!(length));
        let mut units = aterm_uds::sudo::SecretUtf16::new(len)?;
        let get: unsafe extern "C-unwind" fn(Id, Sel, *mut u16, aterm_objc::NSRange) =
            aterm_objc::checked_msg();
        get(
            value,
            sel!(getCharacters:range:),
            units.as_mut_units().as_mut_ptr(),
            aterm_objc::NSRange {
                location: 0,
                length: len,
            },
        );
        aterm_uds::sudo::Secret::from_utf16(units.as_units())
    }
}

/// AppKit driver: opening and closing Details preserves the secure field and the alert.
/// This uses a local inert alert and never makes an authorization request.
pub(crate) fn check_password_disclosure(mtm: MainThread) -> Result<(), &'static str> {
    let alert = crate::app_icon::alert(mtm).ok_or("no alert")?;
    // SAFETY: NSSecureTextField allocation and initialization on the main thread.
    let field = unsafe {
        Obj::from_owned(appkit::send_id(
            appkit::alloc(class(c"NSSecureTextField")),
            sel!(init),
        ))
    }
    .ok_or("no field")?;
    let (view, target) = password_accessory(mtm, &alert, &field, "Exact command and folder".into())
        .ok_or("no accessory")?;
    let password = appkit::nsstring("inert test value").ok_or("no text")?;
    // SAFETY: the accessory owns the field and button, and the alert owns the accessory.
    unsafe {
        appkit::send_v_id(alert.id(), sel!(setAccessoryView:), view.id());
        appkit::send_v_id(field.id(), sel!(setStringValue:), password.id());
        let children = appkit::send_id(view.id(), sel!(subviews));
        let child: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = aterm_objc::checked_msg();
        let button = child(children, sel!(objectAtIndex:), 1);
        for expected in ["Exact command and folder", ""] {
            appkit::send_v_id(button, sel!(performClick:), Id::NIL);
            let actual =
                appkit::nsstring_to_rust(appkit::send_id(alert.id(), sel!(informativeText)));
            if actual != expected {
                return Err("disclosure did not toggle");
            }
            if appkit::nsstring_to_rust(appkit::send_id(field.id(), sel!(stringValue)))
                != "inert test value"
            {
                return Err("disclosure changed password input");
            }
        }
    }
    if target.ivars().open.get() {
        return Err("details did not close");
    }
    Ok(())
}

#[cfg(test)]
#[path = "askpass_sheet_tests.rs"]
mod tests;
