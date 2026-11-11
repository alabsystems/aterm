// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/common/xkb/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;
use aterm_xkb::RuleNames;

const TR_DOTTED_I: u32 = 40 + 8;
const TR_DOTLESS_I: u32 = 23 + 8;

fn turkish() -> Option<Context> {
    let Ok(mut context) = Context::new() else {
        eprintln!("SKIP: no libxkbcommon");
        return None;
    };
    let names = RuleNames {
        rules: c"evdev".as_ptr(),
        model: c"pc105".as_ptr(),
        layout: c"tr".as_ptr(),
        variant: c"".as_ptr(),
        options: c"".as_ptr(),
    };
    // SAFETY: a live context and NUL-terminated names that outlive the call.
    let keymap = unsafe {
        (xkbh().keymap_new_from_names)(
            context.context.as_ptr(),
            &names,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
    };
    let Some(keymap) = NonNull::new(keymap) else {
        eprintln!("SKIP: no xkeyboard-config data for evdev/tr");
        return None;
    };
    let keymap = XkbKeymap::from_raw_for_test(keymap);
    context.state = XkbState::new_for_test(&keymap);
    context.keymap = Some(keymap);
    Some(context)
}

fn press(context: &mut Context, keycode: u32, shift: bool, lock: bool) -> (String, String) {
    let indices = context.keymap.as_ref().unwrap().mods_indices();
    let mask = |on: bool, index: Option<u32>| if on { 1u32 << index.unwrap() } else { 0 };
    let state = context.state.as_mut().unwrap();
    state.update_modifiers(mask(shift, indices.shift), 0, mask(lock, indices.caps), 0, 0, 0);
    let event =
        context.key_context().unwrap().process_key_event(keycode, ElementState::Pressed, false);
    let text_ignoring_lock = event.platform_specific.text_ignoring_lock.clone();
    let text = |t: Option<SmolStr>| t.map(|t| t.to_string()).unwrap_or_default();
    (text(event.text), text(text_ignoring_lock))
}

#[test]
fn a_press_under_caps_lock_is_read_again_without_it() {
    let Some(mut tr) = turkish() else { return };
    for (keycode, shift, lock, text, ignoring_lock) in [
        (TR_DOTTED_I, true, true, "i", "\u{130}"),
        (TR_DOTTED_I, false, true, "\u{130}", "i"),
        (TR_DOTTED_I, true, false, "\u{130}", "\u{130}"),
        (TR_DOTTED_I, false, false, "i", "i"),
        (TR_DOTLESS_I, true, true, "\u{131}", "I"),
        (TR_DOTLESS_I, false, true, "I", "\u{131}"),
        (TR_DOTLESS_I, true, false, "I", "I"),
    ] {
        assert_eq!(
            press(&mut tr, keycode, shift, lock),
            (text.to_string(), ignoring_lock.to_string()),
            "key {keycode}, shift {shift}, lock {lock}"
        );
    }
}
