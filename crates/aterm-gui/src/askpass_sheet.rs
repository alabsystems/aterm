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
    field: Obj,
    panel: Obj,
    parent: Obj,
    reply: Sender<AskpassAnswer>,
    _keys: crate::alert_keys::PasswordKeyWatch,
    _input_fence: crate::askpass::PromptInputFence<'static>,
}

impl Drop for AskpassSheet {
    fn drop(&mut self) {
        unsafe {
            if let Some(empty) = appkit::nsstring("") {
                appkit::send_v_id(self.field.id(), sel!(setStringValue:), empty.id());
            }
            if appkit::send_id(self.panel.id(), sel!(sheetParent)) == self.parent.id() {
                appkit::send_v_id_isize(
                    self.parent.id(),
                    sel!(endSheet:returnCode:),
                    self.panel.id(),
                    appkit::consts::NS_ALERT_SECOND_BUTTON_RETURN,
                );
            }
        }
    }
}

impl App {
    /// Attribute, gate, and raise the askpass sheet, or reply with a refusal.
    pub(crate) fn present_askpass_sheet(&mut self, request: crate::askpass::AskpassRequest) {
        let crate::askpass::AskpassRequest {
            ticket,
            attribute_from,
            command,
            invoker,
            reply,
        } = request;
        if !ticket.is_live(Instant::now()) {
            let _ = reply.send(AskpassAnswer::Refused(
                "request expired or withdrawn".into(),
            ));
            return;
        }

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
        self.withdraw_secrets_sheet_window(wid);
        self.build_askpass_sheet(wid, session, &command, &invoker, ticket, reply);
    }

    /// Build and attach the sheet. On any AppKit failure it replies a refusal;
    /// on success it stores the sheet and raises the open count.
    fn build_askpass_sheet(
        &mut self,
        wid: crate::WindowId,
        session: u64,
        command: &str,
        invoker: &str,
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
        let message = format!(
            "sudo needs your password in tab for session {session}.\nThe command sudo was \
             given (as the kernel records it):"
        );
        let informative = format!(
            "{command}\n\nRun by:\n{invoker}\n\nAllowing lets any sudo from that shell run as root without asking again for about 5 minutes (sudo's own cache), not only the command above."
        );
        let (Some(message), Some(informative), Some(allow), Some(cancel)) = (
            appkit::nsstring(&message),
            appkit::nsstring(&informative),
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

        let _: MainThread = mtm;

        // SAFETY: `+[NSAlert new]` is +1, adopted by `Obj::from_owned`.
        let Some(alert) =
            (unsafe { Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new))) })
        else {
            return refuse(reply, "could not create the alert");
        };
        // SAFETY: `+[NSSecureTextField alloc]` then `-initWithFrame:` is +1.
        let field = unsafe {
            let frame = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
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
            appkit::send_v_id(alert.id(), sel!(setAccessoryView:), field.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), allow.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), cancel.id());
            let panel = Obj::retain(appkit::send_id(alert.id(), sel!(window)));
            let Some(panel) = panel else {
                return Err("the alert reported no panel");
            };
            let input = std::sync::Arc::new(crate::askpass::PasswordInput::new());
            let Some(keys) =
                crate::alert_keys::watch_password_keys(&panel, &ns_window, &field, input)
            else {
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
            field,
            panel,
            parent: ns_window,
            reply,
            _keys: keys,
            _input_fence: input_fence,
        });
    }

    /// Read the answered secure field once, reply with the secret or cancel, and update backoff.
    pub(crate) fn answer_askpass_sheet(&mut self, id: u64, proceed: bool) {
        if self.askpass_sheet.as_ref().is_none_or(|s| s.id != id) {
            return;
        }
        let sheet = self.askpass_sheet.take().expect("checked just above");
        let now = Instant::now();
        if !sheet.ticket.is_live(now) {
            return;
        }
        if proceed {
            let Some(secret) = read_secure_field(&sheet.field) else {
                let _ = sheet.reply.send(AskpassAnswer::Refused(
                    "the password is invalid or longer than 1023 UTF-8 bytes".into(),
                ));
                return;
            };
            if !sheet.ticket.is_live(Instant::now()) {
                return;
            }
            if secret.is_empty() {
                let _ = sheet.reply.send(AskpassAnswer::Cancel);
                self.askpass_backoff.on_cancel(sheet.window.0, now);
            } else {
                let _ = sheet.reply.send(AskpassAnswer::Secret(secret));
                self.askpass_backoff.on_answer(sheet.window.0);
            }
        } else {
            let _ = sheet.reply.send(AskpassAnswer::Cancel);
            self.askpass_backoff.on_cancel(sheet.window.0, now);
        }
    }

    pub(crate) fn withdraw_askpass_sheet(&mut self, id: u64) {
        if let Some(sheet) = self.askpass_sheet.take_if(|sheet| sheet.id == id) {
            sheet.ticket.cancel();
        }
    }

    pub(crate) fn withdraw_askpass_window(&mut self, wid: crate::WindowId) {
        if let Some(sheet) = self.askpass_sheet.take_if(|sheet| sheet.window == wid) {
            sheet.ticket.cancel();
        }
    }

    pub(crate) fn withdraw_askpass_session(&mut self, session: u64) {
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
