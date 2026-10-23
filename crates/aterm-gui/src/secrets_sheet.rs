// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS window-attached `NSAlert` sheet for joining a secrets vault.

use aterm_objc::{CGPoint, CGRect, CGSize, Id, Obj, RcBlock, Sel, class, sel};
use aterm_secrets::SecretBytes;

use crate::appkit::{self, MainThread};
use crate::secrets_join::{Choice, VaultWake};
use crate::{App, Wake};

/// Active secrets-vault join sheet attached to a window.
pub(crate) struct SecretsSheet {
    pub(crate) id: u64,
    window: crate::WindowId,
    field: Obj,
    url: Obj,
    panel: Obj,
    parent: Obj,
    _keys: crate::alert_keys::PasswordKeyWatch,
    _input_fence: crate::askpass::PromptInputFence<'static>,
}

impl Drop for SecretsSheet {
    fn drop(&mut self) {
        // SAFETY: retained AppKit objects on the main thread; -setStringValue: copies
        // and -endSheet:returnCode: ends an attached sheet.
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

/// The sheet's words.
const TITLE: &str = "Connect secrets vault";

fn informative() -> String {
    "Connect this Mac to your secrets vault. Leave the password blank to join an open vault at once, or to wait for approval from another Mac.".to_owned()
}

impl App {
    /// Builds and attaches the secrets-vault join sheet to the focused window.
    pub(crate) fn raise_secrets_sheet(&mut self) -> Result<u64, &'static str> {
        let Some(mtm) = MainThread::new() else {
            return Err("headless");
        };
        let Some(proxy) = self.proxy.clone() else {
            return Err("no event loop");
        };
        let focused = self
            .windows
            .iter()
            .filter(|(_, ws)| crate::os_window_focused(ws))
            .find_map(|(wid, ws)| {
                ws.os_window
                    .as_ref()
                    .map(|w| crate::platform::ns_window_of(w))
                    .filter(|id| !id.is_null())
                    // SAFETY: a live NSWindow borrowed on the main thread; `-window` is +0,
                    // retained into an owning `Obj`.
                    .and_then(|id| unsafe { Obj::retain(id) })
                    .map(|ns_window| (*wid, ns_window))
            });
        let Some((wid, ns_window)) = focused else {
            return Err("no focused window");
        };
        // SAFETY: `-attachedSheet` on a live NSWindow, main thread; +0, compared only.
        if !unsafe { appkit::send_id(ns_window.id(), sel!(attachedSheet)) }.is_null() {
            return Err("the window shows a sheet already");
        }
        let Some(app) = appkit::existing_application(mtm) else {
            return Err("no AppKit application");
        };
        let (
            Some(message),
            Some(informative),
            Some(join),
            Some(not_now),
            Some(never),
            Some(hint),
            Some(escape),
        ) = (
            appkit::nsstring(TITLE),
            appkit::nsstring(&informative()),
            appkit::nsstring("Apply"),
            appkit::nsstring("Not Now"),
            appkit::nsstring("Don\u{2019}t Ask Again"),
            appkit::nsstring("Join password (optional)"),
            appkit::nsstring("\u{1b}"),
        )
        else {
            return Err("could not build the sheet text");
        };
        let id = self.secrets_join.mint_id();
        let completion = move |response: isize| {
            let _ = proxy.send_event(Wake::SecretsVault(VaultWake::Answered {
                id,
                choice: Choice::of_response(response),
            }));
        };
        // SAFETY: AppKit invokes the block as void(^)(NSModalResponse) matching (isize)->().
        let Some(handler) = (unsafe { RcBlock::new1(completion) }) else {
            return Err("could not build the sheet handler");
        };
        let _: MainThread = mtm;
        // SAFETY: `+[NSAlert new]` is +1, adopted by `Obj::from_owned`.
        let Some(alert) =
            (unsafe { Obj::from_owned(appkit::send_id(class(c"NSAlert").as_id(), sel!(new))) })
        else {
            return Err("could not create the alert");
        };
        // SAFETY: `+[NSSecureTextField alloc]` then `-initWithFrame:` is +1.
        let field = unsafe {
            let frame = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 300.0,
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
            return Err("could not create the password field");
        };
        // SAFETY: owned native text fields in a shared accessory, on the main thread.
        let (url, accessory) = unsafe {
            let frame = |y, height| CGRect {
                origin: CGPoint { x: 0.0, y },
                size: CGSize {
                    width: 390.0,
                    height,
                },
            };
            let url = Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSTextField")),
                sel!(initWithFrame:),
                frame(40.0, 26.0),
            ))
            .ok_or("could not create URL field")?;
            let accessory = Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSView")),
                sel!(initWithFrame:),
                frame(0.0, 70.0),
            ))
            .ok_or("could not create vault form")?;
            let initial = appkit::nsstring(aterm_secrets::ALAB_DEFAULT_URL)
                .ok_or("could not create default URL")?;
            appkit::send_v_id(url.id(), sel!(setStringValue:), initial.id());
            appkit::send_v_rect(field.id(), sel!(setFrame:), frame(0.0, 26.0));
            appkit::send_v_id(accessory.id(), sel!(addSubview:), url.id());
            appkit::send_v_id(accessory.id(), sel!(addSubview:), field.id());
            (url, accessory)
        };
        // SAFETY: standard NSAlert and event-queue calls on the main thread, as the askpass
        // sheet's: text setters copy, `-addButtonWithTitle:` returns the +0 button the alert
        // owns, `-setAccessoryView:` retains the accessory, and
        // `-beginSheetModalForWindow:completionHandler:` copies the block. The field and
        // panel are retained for the App to hold.
        let (panel, input_fence) = crate::askpass::PROMPT_INPUT.present(|| unsafe {
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
                return Err("too many queued key events");
            }
            appkit::send_v_id(field.id(), sel!(setPlaceholderString:), hint.id());
            appkit::send_v_id(alert.id(), sel!(setMessageText:), message.id());
            appkit::send_v_id(alert.id(), sel!(setInformativeText:), informative.id());
            appkit::send_v_id(alert.id(), sel!(setAccessoryView:), accessory.id());
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), join.id());
            let later = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), not_now.id());
            if !later.is_null() {
                appkit::send_v_id(later, sel!(setKeyEquivalent:), escape.id());
            }
            let _ = appkit::send_id_id(alert.id(), sel!(addButtonWithTitle:), never.id());
            let Some(panel) = Obj::retain(appkit::send_id(alert.id(), sel!(window))) else {
                return Err("the alert reported no panel");
            };
            let input = std::sync::Arc::new(crate::askpass::PasswordInput::new());
            let Some(keys) =
                crate::alert_keys::watch_password_keys(&panel, &ns_window, &accessory, input)
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
        let (panel, keys) = panel?;
        self.secrets_join.native = Some(SecretsSheet {
            id,
            window: wid,
            field,
            url,
            panel,
            parent: ns_window,
            _keys: keys,
            _input_fence: input_fence,
        });
        Ok(id)
    }

    /// Consumes the answered sheet `id` and returns its password when `read` is true.
    pub(crate) fn take_secrets_sheet(&mut self, id: u64, read: bool) -> Option<SecretBytes> {
        self.take_secrets_sheet_with_source(id, read)
            .and_then(|(_, password)| password)
    }

    pub(crate) fn take_secrets_sheet_with_source(
        &mut self,
        id: u64,
        read: bool,
    ) -> Option<(String, Option<SecretBytes>)> {
        let sheet = self.secrets_join.native.take_if(|sheet| sheet.id == id)?;
        if !read {
            return None;
        }
        // SAFETY: the retained URL control is read on the application main thread.
        let source =
            unsafe { appkit::nsstring_to_rust(appkit::send_id(sheet.url.id(), sel!(stringValue))) }
                .trim()
                .to_owned();
        let typed = crate::askpass_sheet::read_secure_field(&sheet.field);
        let password = typed
            .filter(|typed| !typed.is_empty())
            .map(|typed| SecretBytes::from(typed.as_bytes()));
        drop(sheet);
        Some((source, password))
    }

    /// Withdraws any active secrets sheet on `wid` so it can be re-asked later.
    pub(crate) fn withdraw_secrets_sheet_window(&mut self, wid: crate::WindowId) {
        if let Some(sheet) = self
            .secrets_join
            .native
            .take_if(|sheet| sheet.window == wid)
        {
            aterm_log::info!(
                "secrets vault: sheet {} withdrawn from its window",
                sheet.id
            );
            drop(sheet);
            self.secrets_join.withdrawn();
        }
    }
}
