// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Mac close/quit question as a native `NSAlert` sheet on the window it stands over.
//!
//! `-beginSheetModalForWindow:completionHandler:` runs no nested modal loop: the sheet's answer
//! comes back through the ordinary event loop as [`crate::Wake::CloseSheetAnswered`], and
//! `App::answer_close_banner` replays it through the same generations and shutdown barriers as
//! the in-window band. The sheet lives exactly as long as the [`crate::close_confirm::PendingClose`]
//! it presents: a question that ends any other way (the wire's `confirm`, its window closing)
//! takes its sheet down with it.

use aterm_objc::{Id, Obj, RcBlock, Sel, sel};

use crate::alert_keys::{ConfirmKeyWatch, ConfirmationUp};
use crate::appkit::{self, MainThread};
use crate::quit_safety::ConfirmPrompt;

/// Taken before a Mac question stands: it counts the question as on screen while the key queue
/// drains, so no key typed or posted before the question appeared answers it.
pub(crate) struct Fence {
    up: ConfirmationUp,
    mtm: MainThread,
}

/// [`Fence`] for a question about to stand, or `None` when the key queue would not drain (a key
/// queued before the question could answer it, so the gesture is refused).
pub(crate) fn fence() -> Option<Fence> {
    let mtm = MainThread::new()?;
    let up = ConfirmationUp::hold();
    crate::hwkey::fence_queued_keys(mtm).then_some(Fence { up, mtm })
}

/// The `NSWindow` of `window`, retained.
pub(crate) fn parent_of(window: &aterm_window::window::Window) -> Option<Obj> {
    // SAFETY: `ns_window_of` answers the window's live `NSWindow` (+0) or nil, on the main thread;
    // it is retained into the `Obj`.
    unsafe { Obj::retain(crate::platform::ns_window_of(window)) }
}

/// One close/quit question on screen as a sheet; ending it is dropping it.
pub(crate) struct CloseSheet {
    parent: Obj,
    panel: Obj,
    _alert: Obj,
    _keys: Option<ConfirmKeyWatch>,
    _up: Option<ConfirmationUp>,
}

impl Drop for CloseSheet {
    fn drop(&mut self) {
        // A sheet its own buttons answered is gone already; one still outstanding (the wire
        // answered, the question was withdrawn) is ended as Cancel. Its completion handler then
        // reports a serial no question carries any more, which `App` ignores.
        if crate::alert_keys::sheet_outstanding(self.parent.id(), self.panel.id()) {
            // SAFETY: `-[NSWindow endSheet:returnCode:]` is `-(void)(NSWindow *, NSModalResponse)`
            // sent to the retained parent with its retained sheet, on the main thread where the
            // `App` holding this value drops it (`Obj` is `!Send`).
            unsafe {
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

/// Present `prompt` as a sheet on `parent`, its answer posted as
/// [`crate::Wake::CloseSheetAnswered`] with `serial`. A question the person asked (`persons`)
/// keeps `hwkey` refused while it stands; a control client's, which `confirm yes|no` answers,
/// does not, and its sheet's key watch drops any posted key that reaches it. `None` when AppKit
/// or Foundation refused a step: the question then stands in the window instead.
pub(crate) fn present(
    parent: Obj,
    prompt: &ConfirmPrompt,
    serial: u64,
    persons: bool,
    proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    fence: Fence,
) -> Option<CloseSheet> {
    let Fence { up, mtm } = fence;
    let up = persons.then_some(up);
    let (message, informative, proceed, cancel) = (
        appkit::nsstring(prompt.title)?,
        appkit::nsstring(prompt.body)?,
        appkit::nsstring(prompt.proceed)?,
        appkit::nsstring("Cancel")?,
    );
    let completion = move |response: isize| {
        let _ = proxy.send_event(crate::Wake::CloseSheetAnswered {
            serial,
            proceed: response == appkit::consts::NS_ALERT_FIRST_BUTTON_RETURN,
        });
    };
    // SAFETY: AppKit invokes the block as `void (^)(NSModalResponse)`; the `(isize) -> ()`
    // prototype matches (`NSModalResponse` is `NSInteger`), and nothing unwinds out of it.
    let handler = unsafe { RcBlock::new1(completion) }?;
    // An alert whose icon draws with no IconServices call on the main thread.
    let alert = crate::app_icon::alert(mtm)?;
    // SAFETY: standard `NSAlert` setters on the main thread (`mtm`). The text setters copy;
    // `-addButtonWithTitle:` and `-window` answer +0 and are retained. The first button added is
    // the default (Return); AppKit gives the one titled Cancel Escape.
    let (panel, accept, refuse) = unsafe {
        appkit::send_v_id(alert.id(), sel!(setMessageText:), message.id());
        appkit::send_v_id(alert.id(), sel!(setInformativeText:), informative.id());
        let accept = Obj::retain(appkit::send_id_id(
            alert.id(),
            sel!(addButtonWithTitle:),
            proceed.id(),
        ));
        let refuse = Obj::retain(appkit::send_id_id(
            alert.id(),
            sel!(addButtonWithTitle:),
            cancel.id(),
        ));
        (
            Obj::retain(appkit::send_id(alert.id(), sel!(window)))?,
            accept,
            refuse,
        )
    };
    // Return with any modifier answers Quit, Escape Cancel, and a key a control client posted
    // answers nothing (`alert_keys`).
    let keys = match (accept, refuse) {
        (Some(accept), Some(refuse)) => crate::alert_keys::watch_alert_keys(
            panel.clone_retained(),
            Some(parent.clone_retained()),
            accept,
            refuse,
        ),
        _ => None,
    };
    if keys.is_none() {
        aterm_log::warn!(
            "close/quit sheet: AppKit declined a local key monitor; the sheet keeps its stock \
             keys (Return, Escape, the mouse)"
        );
    }
    // SAFETY: sheet presentation on the main thread against the live retained parent; AppKit
    // copies the block.
    unsafe {
        let begin_sheet: unsafe extern "C-unwind" fn(Id, Sel, Id, aterm_objc::BlockPtr) =
            aterm_objc::checked_msg();
        begin_sheet(
            alert.id(),
            sel!(beginSheetModalForWindow:completionHandler:),
            parent.id(),
            handler.as_block_ptr(),
        );
    }
    Some(CloseSheet {
        parent,
        panel,
        _alert: alert,
        _keys: keys,
        _up: up,
    })
}
