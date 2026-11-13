// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LOG: &str = "\
001:<: am lsb-first want 11:0 authorising with '' of length 0
001:>: Success, version is 11:0 vendor='The X.Org Foundation' roots={root=0x0000021f default-colormap=0x00000020 white-pixel=0x00ffffff}
001:<:0001: 20: Request(98): QueryExtension name='BIG-REQUESTS'
001:<:0002:  4: BIG-REQUESTS-Request(133,0): Enable
001:<:000d: 24: Request(16): InternAtom only-if-exists=false(0x00) name='_XSETTINGS_S0'
001:<:000f: 16: Request(2): ChangeWindowAttributes window=0x00000000 value-list={event-mask=PropertyChange}
001:>:000f:Error 3=Window: major=2, minor=0, bad=0x00000000, seq=000f
001:<:001e: 24: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE'
001:>:001e:32: Reply to InternAtom: atom=0xf1(\"_NET_WM_STATE\")
001:<:001f: 28: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE_ABOVE'
001:>:001f:32: Reply to InternAtom: atom=0xf2(\"_NET_WM_STATE_ABOVE\")
001:<:0020: 28: Request(16): InternAtom only-if-exists=false(0x00) name='_NET_WM_STATE_BELOW'
001:>:0020:32: Reply to InternAtom: atom=0xf3(\"_NET_WM_STATE_BELOW\")
001:<:003f: 12: RANDR-Request(140,4): SelectInput window=0x0000021f enable=ScreenChangeNotifyMask,OutputPropertyNotifyMask
001:<:00a6: 20: XInputExtension-Request(131,46): XISelectEvents win=0x0000021f masks={device=1 mask=0x0003e000;};
001:<:0098: 20: XInputExtension-Request(131,46): XISelectEvents win=0x0000021f masks={device=0 mask=0x00000800;};
001:<:0099: 16: XKEYBOARD-Request(135,1): SelectEvents opcode=0x87 opcode2=0x01 unparsed-data=0x00,0x01,0x07,0x00;
001:<:00a7: 44: Request(1): CreateWindow depth=0x00 window=0x00200002 parent=0x0000021f x=0 y=0 width=744 height=464 value-list={border-pixel=0x00000000 event-mask=KeyPress,KeyRelease,StructureNotify colormap=CopyFromParent(0x00000000)}
001:<:00b6: 20: XInputExtension-Request(131,46): XISelectEvents win=0x00200002 masks={device=1 mask=0x001c07f0;};
001:<:00b8: 44: Request(25): SendEvent propagate=false(0x00) destination=0x0000021f event-mask=SubstructureNotify,SubstructureRedirect ClientMessage(33) format=0x20 window=0x00200002 type=0xf1(\"_NET_WM_STATE\") data=0x00,0x00,0x00,0x00,0xf2,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00;
001:<:00b9: 44: Request(25): SendEvent propagate=false(0x00) destination=0x0000021f event-mask=SubstructureNotify,SubstructureRedirect ClientMessage(33) format=0x20 window=0x00200002 type=0xf1(\"_NET_WM_STATE\") data=0x00,0x00,0x00,0x00,0xf3,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00;
001:<:00c3:  8: Request(8): MapWindow window=0x00200002
001:<: Warning: buffer filled!
001:<:00f2: 16: XFIXES-Request(138,23): SetCursorName cursor=0x00200008 name='text'
001:<:0100: 12: Generic Event Extension-Request(128,0): QueryVersion major version=1 minor version=0
";

fn line(key: &str, log: &str) -> String {
    summary(log)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

#[test]
fn the_requests_are_named_once_with_their_extension() {
    assert_eq!(
        line("x11.wire.requests", LOG),
        "BIG-REQUESTS:Enable ChangeWindowAttributes CreateWindow \
         Generic Event Extension:QueryVersion InternAtom MapWindow QueryExtension \
         RANDR:SelectInput SendEvent XFIXES:SetCursorName \
         XInputExtension:XISelectEvents XKEYBOARD:SelectEvents"
    );
}

#[test]
fn the_atoms_are_named_and_counted() {
    assert_eq!(
        line("x11.wire.atoms", LOG),
        "4: _NET_WM_STATE _NET_WM_STATE_ABOVE _NET_WM_STATE_BELOW _XSETTINGS_S0"
    );
}

#[test]
fn the_selections_name_the_window_and_the_events() {
    assert_eq!(
        line("x11.wire.selects", LOG),
        "XKB 00010700; \
         none events PropertyChange; \
         root RANDR ScreenChangeNotifyMask,OutputPropertyNotifyMask; \
         root XI2 all HierarchyChanged; \
         root XI2 all-master RawKeyPress,RawKeyRelease,RawButtonPress,RawButtonRelease,RawMotion; \
         window XI2 all-master ButtonPress,ButtonRelease,Motion,Enter,Leave,FocusIn,FocusOut,TouchBegin,TouchUpdate,TouchEnd; \
         window events KeyPress,KeyRelease,StructureNotify"
    );
}

#[test]
fn the_wm_state_messages_name_their_action_and_atoms() {
    assert_eq!(
        line("x11.wire.messages", LOG),
        "window _NET_WM_STATE remove _NET_WM_STATE_ABOVE; \
         window _NET_WM_STATE remove _NET_WM_STATE_BELOW"
    );
    let dropped: String = LOG
        .lines()
        .filter(|l| !l.contains("SendEvent"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(line("x11.wire.messages", &dropped), "none");
}

#[test]
fn an_error_is_named_by_the_request_it_answered() {
    assert_eq!(
        line("x11.wire.errors", LOG),
        "Window on ChangeWindowAttributes"
    );
    let clean: String = LOG
        .lines()
        .filter(|l| !l.contains(":Error "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_eq!(line("x11.wire.errors", &clean), "none");
    assert_eq!(hex("0x0x1"), None);
}
