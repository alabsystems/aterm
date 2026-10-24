// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Checks `aterm-x11` struct layouts and constants against the C headers.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::mem::{align_of, offset_of, size_of};

use aterm_x11::xlib::*;

struct Layout {
    c: &'static str,
    size: usize,
    align: usize,
    fields: Vec<(&'static str, usize)>,
}

macro_rules! layout {
    ($t:ty, $c:literal { $($f:ident $(= $cf:literal)?),* $(,)? }) => {
        Layout {
            c: $c,
            size: size_of::<$t>(),
            align: align_of::<$t>(),
            fields: vec![$((layout!(@name $f $(, $cf)?), offset_of!($t, $f))),*],
        }
    };
    (@name $f:ident) => { stringify!($f) };
    (@name $f:ident, $cf:literal) => { $cf };
}

fn layouts() -> Vec<Layout> {
    vec![
        layout!(XAnyEvent, "XAnyEvent" { type_ = "type", serial, send_event, display, window }),
        layout!(XKeyEvent, "XKeyEvent" {
            type_ = "type", serial, send_event, display, window, root, subwindow, time, x, y,
            x_root, y_root, state, keycode, same_screen
        }),
        layout!(XExposeEvent, "XExposeEvent" {
            type_ = "type", serial, send_event, display, window, x, y, width, height, count
        }),
        layout!(XVisibilityEvent, "XVisibilityEvent" {
            type_ = "type", serial, send_event, display, window, state
        }),
        layout!(XDestroyWindowEvent, "XDestroyWindowEvent" {
            type_ = "type", serial, send_event, display, event, window
        }),
        layout!(XUnmapEvent, "XUnmapEvent" {
            type_ = "type", serial, send_event, display, event, window, from_configure
        }),
        layout!(XMapEvent, "XMapEvent" {
            type_ = "type", serial, send_event, display, event, window, override_redirect
        }),
        layout!(XReparentEvent, "XReparentEvent" {
            type_ = "type", serial, send_event, display, event, window, parent, x, y,
            override_redirect
        }),
        layout!(XConfigureEvent, "XConfigureEvent" {
            type_ = "type", serial, send_event, display, event, window, x, y, width, height,
            border_width, above, override_redirect
        }),
        layout!(XPropertyEvent, "XPropertyEvent" {
            type_ = "type", serial, send_event, display, window, atom, time, state
        }),
        layout!(XSelectionEvent, "XSelectionEvent" {
            type_ = "type", serial, send_event, display, requestor, selection, target, property,
            time
        }),
        layout!(XClientMessageEvent, "XClientMessageEvent" {
            type_ = "type", serial, send_event, display, window, message_type, format, data
        }),
        layout!(XErrorEvent, "XErrorEvent" {
            type_ = "type", display, resourceid, serial, error_code, request_code, minor_code
        }),
        layout!(XGenericEventCookie, "XGenericEventCookie" {
            type_ = "type", serial, send_event, display, extension, evtype, cookie, data
        }),
        layout!(XEvent, "XEvent" {}),
        layout!(XModifierKeymap, "XModifierKeymap" { max_keypermod, modifiermap }),
        layout!(XPoint, "XPoint" { x, y }),
        layout!(XColor, "XColor" { pixel, red, green, blue, flags, pad }),
        layout!(XkbAnyEvent, "XkbAnyEvent" {
            type_ = "type", serial, send_event, display, time, xkb_type, device
        }),
        layout!(XkbNewKeyboardNotifyEvent, "XkbNewKeyboardNotifyEvent" {
            type_ = "type", serial, send_event, display, time, xkb_type, device, old_device,
            min_key_code, max_key_code, old_min_key_code, old_max_key_code, changed, req_major,
            req_minor
        }),
        layout!(XkbStateNotifyEvent, "XkbStateNotifyEvent" {
            type_ = "type", serial, send_event, display, time, xkb_type, device, changed, group,
            base_group, latched_group, locked_group, mods, base_mods, latched_mods, locked_mods,
            compat_state, grab_mods, compat_grab_mods, lookup_mods, compat_lookup_mods,
            ptr_buttons, keycode, event_type, req_major, req_minor
        }),
        layout!(XkbBellNotifyEvent, "XkbBellNotifyEvent" {
            type_ = "type", serial, send_event, display, time, xkb_type, device, percent, pitch,
            duration, bell_class, bell_id, name, window, event_only
        }),
        layout!(XkbStateRec, "XkbStateRec" {
            group, locked_group, base_group, latched_group, mods, base_mods, latched_mods,
            locked_mods, compat_state, grab_mods, compat_grab_mods, lookup_mods,
            compat_lookup_mods, ptr_buttons
        }),
        layout!(XIMCallback, "XIMCallback" { client_data, callback }),
        layout!(XIMText, "XIMText" { length, feedback, encoding_is_wchar, string }),
        layout!(XIMPreeditDrawCallbackStruct, "XIMPreeditDrawCallbackStruct" {
            caret, chg_first, chg_length, text
        }),
        layout!(XIMPreeditCaretCallbackStruct, "XIMPreeditCaretCallbackStruct" {
            position, direction, style
        }),
        layout!(XIMStyles, "XIMStyles" { count_styles, supported_styles }),
        layout!(XIModifierState, "XIModifierState" { base, latched, locked, effective }),
        layout!(XIButtonState, "XIButtonState" { mask_len, mask }),
        layout!(XIValuatorState, "XIValuatorState" { mask_len, mask, values }),
        layout!(XIDeviceEvent, "XIDeviceEvent" {
            type_ = "type", serial, send_event, display, extension, evtype, time, deviceid,
            sourceid, detail, root, event, child, root_x, root_y, event_x, event_y, flags,
            buttons, valuators, mods, group
        }),
        layout!(XIRawEvent, "XIRawEvent" {
            type_ = "type", serial, send_event, display, extension, evtype, time, deviceid,
            sourceid, detail, flags, valuators, raw_values
        }),
        layout!(XIEnterEvent, "XIEnterEvent" {
            type_ = "type", serial, send_event, display, extension, evtype, time, deviceid,
            sourceid, detail, root, event, child, root_x, root_y, event_x, event_y, mode, focus,
            same_screen, buttons, mods, group
        }),
        layout!(XIHierarchyInfo, "XIHierarchyInfo" {
            deviceid, attachment, use_ = "use", enabled, flags
        }),
        layout!(XIHierarchyEvent, "XIHierarchyEvent" {
            type_ = "type", serial, send_event, display, extension, evtype, time, flags, num_info,
            info
        }),
        layout!(XIAnyClassInfo, "XIAnyClassInfo" { type_ = "type", sourceid }),
        layout!(XIValuatorClassInfo, "XIValuatorClassInfo" {
            type_ = "type", sourceid, number, label, min, max, value, resolution, mode
        }),
        layout!(XIScrollClassInfo, "XIScrollClassInfo" {
            type_ = "type", sourceid, number, scroll_type, increment, flags
        }),
        layout!(XIDeviceInfo, "XIDeviceInfo" {
            deviceid, name, use_ = "use", attachment, enabled, num_classes, classes
        }),
        layout!(XcursorImage, "XcursorImage" {
            version, size, width, height, xhot, yhot, delay, pixels
        }),
    ]
}

fn constants() -> Vec<(&'static str, i64)> {
    use event_type as e;
    vec![
        ("True", i64::from(TRUE)),
        ("False", i64::from(FALSE)),
        ("KeyPress", i64::from(e::KEY_PRESS)),
        ("KeyRelease", i64::from(e::KEY_RELEASE)),
        ("Expose", i64::from(e::EXPOSE)),
        ("VisibilityNotify", i64::from(e::VISIBILITY_NOTIFY)),
        ("DestroyNotify", i64::from(e::DESTROY_NOTIFY)),
        ("UnmapNotify", i64::from(e::UNMAP_NOTIFY)),
        ("MapNotify", i64::from(e::MAP_NOTIFY)),
        ("ReparentNotify", i64::from(e::REPARENT_NOTIFY)),
        ("ConfigureNotify", i64::from(e::CONFIGURE_NOTIFY)),
        ("PropertyNotify", i64::from(e::PROPERTY_NOTIFY)),
        ("SelectionNotify", i64::from(e::SELECTION_NOTIFY)),
        ("ClientMessage", i64::from(e::CLIENT_MESSAGE)),
        ("GenericEvent", i64::from(e::GENERIC_EVENT)),
        (
            "VisibilityFullyObscured",
            i64::from(VISIBILITY_FULLY_OBSCURED),
        ),
        ("XBufferOverflow", i64::from(X_BUFFER_OVERFLOW)),
        ("XLookupChars", i64::from(X_LOOKUP_CHARS)),
        ("XLookupKeySym", i64::from(X_LOOKUP_KEYSYM)),
        ("XLookupBoth", i64::from(X_LOOKUP_BOTH)),
        ("XkbUseCoreKbd", i64::from(XKB_USE_CORE_KBD)),
        (
            "XkbNewKeyboardNotify",
            i64::from(xkb_event::NEW_KEYBOARD_NOTIFY),
        ),
        ("XkbMapNotify", i64::from(xkb_event::MAP_NOTIFY)),
        ("XkbStateNotify", i64::from(xkb_event::STATE_NOTIFY)),
        ("XkbBellNotify", i64::from(xkb_event::BELL_NOTIFY)),
        ("XIMPreeditArea", xim_style::PREEDIT_AREA as i64),
        ("XIMPreeditCallbacks", xim_style::PREEDIT_CALLBACKS as i64),
        ("XIMPreeditPosition", xim_style::PREEDIT_POSITION as i64),
        ("XIMPreeditNothing", xim_style::PREEDIT_NOTHING as i64),
        ("XIMPreeditNone", xim_style::PREEDIT_NONE as i64),
        ("XIMStatusArea", xim_style::STATUS_AREA as i64),
        ("XIMStatusCallbacks", xim_style::STATUS_CALLBACKS as i64),
        ("XIMStatusNothing", xim_style::STATUS_NOTHING as i64),
        ("XIMStatusNone", xim_style::STATUS_NONE as i64),
        ("XIMForwardChar", i64::from(xim_caret::FORWARD_CHAR)),
        ("XIMBackwardChar", i64::from(xim_caret::BACKWARD_CHAR)),
        (
            "XIMAbsolutePosition",
            i64::from(xim_caret::ABSOLUTE_POSITION),
        ),
        ("XIAllDevices", i64::from(xi::ALL_DEVICES)),
        ("XIAllMasterDevices", i64::from(xi::ALL_MASTER_DEVICES)),
        ("XIMasterPointer", i64::from(xi::MASTER_POINTER)),
        ("XIMasterKeyboard", i64::from(xi::MASTER_KEYBOARD)),
        ("XISlavePointer", i64::from(xi::SLAVE_POINTER)),
        ("XISlaveKeyboard", i64::from(xi::SLAVE_KEYBOARD)),
        ("XIFloatingSlave", i64::from(xi::FLOATING_SLAVE)),
        ("XIKeyClass", i64::from(xi::KEY_CLASS)),
        ("XIButtonClass", i64::from(xi::BUTTON_CLASS)),
        ("XIValuatorClass", i64::from(xi::VALUATOR_CLASS)),
        ("XIScrollClass", i64::from(xi::SCROLL_CLASS)),
        ("XITouchClass", i64::from(xi::TOUCH_CLASS)),
        ("XIScrollTypeVertical", i64::from(xi::SCROLL_TYPE_VERTICAL)),
        (
            "XIScrollTypeHorizontal",
            i64::from(xi::SCROLL_TYPE_HORIZONTAL),
        ),
        ("XI_DeviceChanged", i64::from(xi::DEVICE_CHANGED)),
        ("XI_KeyPress", i64::from(xi::KEY_PRESS)),
        ("XI_KeyRelease", i64::from(xi::KEY_RELEASE)),
        ("XI_ButtonPress", i64::from(xi::BUTTON_PRESS)),
        ("XI_ButtonRelease", i64::from(xi::BUTTON_RELEASE)),
        ("XI_Motion", i64::from(xi::MOTION)),
        ("XI_Enter", i64::from(xi::ENTER)),
        ("XI_Leave", i64::from(xi::LEAVE)),
        ("XI_FocusIn", i64::from(xi::FOCUS_IN)),
        ("XI_FocusOut", i64::from(xi::FOCUS_OUT)),
        ("XI_HierarchyChanged", i64::from(xi::HIERARCHY_CHANGED)),
        ("XI_RawKeyPress", i64::from(xi::RAW_KEY_PRESS)),
        ("XI_RawKeyRelease", i64::from(xi::RAW_KEY_RELEASE)),
        ("XI_RawButtonPress", i64::from(xi::RAW_BUTTON_PRESS)),
        ("XI_RawButtonRelease", i64::from(xi::RAW_BUTTON_RELEASE)),
        ("XI_RawMotion", i64::from(xi::RAW_MOTION)),
        ("XI_TouchBegin", i64::from(xi::TOUCH_BEGIN)),
        ("XI_TouchUpdate", i64::from(xi::TOUCH_UPDATE)),
        ("XI_TouchEnd", i64::from(xi::TOUCH_END)),
        ("XIMasterAdded", i64::from(xi::MASTER_ADDED)),
        ("XIMasterRemoved", i64::from(xi::MASTER_REMOVED)),
        ("XISlaveAdded", i64::from(xi::SLAVE_ADDED)),
        ("XISlaveRemoved", i64::from(xi::SLAVE_REMOVED)),
        ("XISlaveAttached", i64::from(xi::SLAVE_ATTACHED)),
        ("XISlaveDetached", i64::from(xi::SLAVE_DETACHED)),
        ("XIDeviceEnabled", i64::from(xi::DEVICE_ENABLED)),
        ("XIDeviceDisabled", i64::from(xi::DEVICE_DISABLED)),
    ]
}

fn strings() -> Vec<(&'static str, &'static str)> {
    let s = |c: &'static std::ffi::CStr| c.to_str().unwrap();
    vec![
        ("XNClientWindow", s(xn::CLIENT_WINDOW)),
        ("XNDestroyCallback", s(xn::DESTROY_CALLBACK)),
        ("XNInputStyle", s(xn::INPUT_STYLE)),
        ("XNPreeditAttributes", s(xn::PREEDIT_ATTRIBUTES)),
        ("XNPreeditCaretCallback", s(xn::PREEDIT_CARET_CALLBACK)),
        ("XNPreeditDoneCallback", s(xn::PREEDIT_DONE_CALLBACK)),
        ("XNPreeditDrawCallback", s(xn::PREEDIT_DRAW_CALLBACK)),
        ("XNPreeditStartCallback", s(xn::PREEDIT_START_CALLBACK)),
        ("XNQueryInputStyle", s(xn::QUERY_INPUT_STYLE)),
        ("XNSpotLocation", s(xn::SPOT_LOCATION)),
    ]
}

const ARCH: &str = if cfg!(target_arch = "x86_64") {
    "x86_64"
} else if cfg!(target_arch = "aarch64") {
    "aarch64"
} else {
    "other"
};

fn ours(layouts: &[Layout]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("arch".to_owned(), ARCH.to_owned());
    for l in layouts {
        out.insert(format!("size {}", l.c), l.size.to_string());
        out.insert(format!("align {}", l.c), l.align.to_string());
        for (f, off) in &l.fields {
            out.insert(format!("offset {}.{f}", l.c), off.to_string());
        }
    }
    for (c, v) in constants() {
        out.insert(format!("const {c}"), v.to_string());
    }
    for (c, v) in strings() {
        out.insert(format!("string {c}"), v.to_owned());
    }
    out
}

fn probe(layouts: &[Layout]) -> String {
    let mut c = String::from(
        "#include <stddef.h>\n#include <stdio.h>\n#include <X11/Xlib.h>\n#include <X11/Xutil.h>\n\
         #include <X11/XKBlib.h>\n#include <X11/extensions/XInput2.h>\n\
         #include <X11/Xcursor/Xcursor.h>\nint main(void) {\n\
         #if defined(__x86_64__)\n  puts(\"arch\\tx86_64\");\n\
         #elif defined(__aarch64__)\n  puts(\"arch\\taarch64\");\n#else\n  puts(\"arch\\tother\");\n#endif\n",
    );
    for l in layouts {
        c.push_str(&format!(
            "  printf(\"size {0}\\t%zu\\n\", sizeof({0}));\n  printf(\"align {0}\\t%zu\\n\", _Alignof({0}));\n",
            l.c
        ));
        for (f, _) in &l.fields {
            c.push_str(&format!(
                "  printf(\"offset {0}.{1}\\t%zu\\n\", offsetof({0}, {1}));\n",
                l.c, f
            ));
        }
    }
    for (name, _) in constants() {
        c.push_str(&format!(
            "  printf(\"const {name}\\t%lld\\n\", (long long)({name}));\n"
        ));
    }
    for (name, _) in strings() {
        c.push_str(&format!("  printf(\"string {name}\\t%s\\n\", {name});\n"));
    }
    c.push_str("  return 0;\n}\n");
    c
}

fn compiler() -> String {
    let target = if cfg!(target_arch = "x86_64") {
        "x86_64_unknown_linux_gnu"
    } else {
        "aarch64_unknown_linux_gnu"
    };
    std::env::var(format!("CC_{target}")).unwrap_or_else(|_| "cc".to_owned())
}

fn theirs(layouts: &[Layout]) -> Option<BTreeMap<String, String>> {
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let src = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&src, probe(layouts)).unwrap();
    let cc = compiler();
    let built = aterm_uds::spawnfd::command(&cc)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output();
    match built {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            eprintln!(
                "SKIP: {cc} could not build the probe (no X development headers?):\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return None;
        }
        Err(e) => {
            eprintln!("SKIP: no C compiler ({cc}: {e})");
            return None;
        }
    }
    let ran = aterm_uds::spawnfd::command(&exe)
        .output()
        .expect("the probe runs");
    assert!(ran.status.success(), "the probe exits 0");
    Some(
        String::from_utf8(ran.stdout)
            .expect("UTF-8")
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
    )
}

#[test]
fn every_layout_and_constant_is_the_headers() {
    let layouts = layouts();
    let Some(theirs) = theirs(&layouts) else {
        return;
    };
    assert_eq!(
        theirs.get("arch").map(String::as_str),
        Some(ARCH),
        "negative control: the probe was built for this architecture"
    );
    let ours = ours(&layouts);
    assert!(ours.len() > 300, "the checks are whole: {}", ours.len());
    let mut wrong = Vec::new();
    for (k, v) in &ours {
        match theirs.get(k) {
            Some(t) if t == v => {}
            Some(t) => wrong.push(format!("{k}: ours {v}, the headers {t}")),
            None => wrong.push(format!("{k}: the probe printed nothing")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    let shifted = ours
        .iter()
        .find(|(k, _)| k.as_str() == "offset XKeyEvent.keycode")
        .map(|(_, v)| v.parse::<usize>().unwrap() + 4)
        .unwrap()
        .to_string();
    assert_ne!(theirs.get("offset XKeyEvent.keycode"), Some(&shifted));
}
