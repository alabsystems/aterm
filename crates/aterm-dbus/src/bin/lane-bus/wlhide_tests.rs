// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn msg(object: u32, opcode: u16, args: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + args.len()).unwrap();
    let mut m = object.to_ne_bytes().to_vec();
    m.extend_from_slice(&(size << 16 | u32::from(opcode)).to_ne_bytes());
    m.extend_from_slice(args);
    m
}

fn global(name: u32, interface: &str, version: u32) -> Vec<u8> {
    let mut a = name.to_ne_bytes().to_vec();
    let n = u32::try_from(interface.len() + 1).unwrap();
    a.extend_from_slice(&n.to_ne_bytes());
    a.extend_from_slice(interface.as_bytes());
    a.push(0);
    a.resize(8 + (interface.len() + 1).div_ceil(4) * 4, 0);
    a.extend_from_slice(&version.to_ne_bytes());
    msg(2, 0, &a)
}

fn through(f: &mut dyn Filter, mut stream: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(n) = f.len(stream).unwrap() {
        let mut m = stream[..n].to_vec();
        if f.pass(&mut m) {
            out.extend_from_slice(&m);
        }
        stream = &stream[n..];
    }
    out
}

#[test]
fn a_hidden_global_never_comes_and_a_capped_one_comes_older() {
    let rules = parse_rules(&[
        "hide:zxdg_decoration_manager_v1".to_string(),
        "cap:wl_compositor=5".to_string(),
    ])
    .unwrap();
    let known = Known::default();
    let mut req = Requests {
        known: Arc::clone(&known),
    };
    let get_registry = msg(1, 1, &2u32.to_ne_bytes());
    assert_eq!(through(&mut req, &get_registry), get_registry);
    let mut ev = Events { rules, known };
    let mut stream = global(1, "wl_compositor", 6);
    stream.extend(global(2, "zxdg_decoration_manager_v1", 1));
    stream.extend(global(3, "wl_shm", 1));
    stream.extend(msg(2, 1, &2u32.to_ne_bytes()));
    stream.extend(msg(2, 1, &3u32.to_ne_bytes()));
    stream.extend(msg(5, 0, &[1, 2, 3, 4]));
    let out = through(&mut ev, &stream);
    let mut want = global(1, "wl_compositor", 5);
    want.extend(global(3, "wl_shm", 1));
    want.extend(msg(2, 1, &3u32.to_ne_bytes()));
    want.extend(msg(5, 0, &[1, 2, 3, 4]));
    assert_eq!(out, want);
}

#[test]
fn a_rule_is_read_or_refused() {
    assert_eq!(parse_rules(&["-".into()]).unwrap(), []);
    assert!(parse_rules(&[]).is_err());
    assert!(parse_rules(&["cap:wl_compositor=0".into()]).is_err());
    assert!(parse_rules(&["cap:wl_compositor=+5".into()]).is_err());
    assert!(parse_rules(&["show:x".into()]).is_err());
    let mut four = 1u32.to_ne_bytes().to_vec();
    four.extend_from_slice(&(4u32 << 16).to_ne_bytes());
    assert!(size(&four).is_err(), "a message shorter than its header");
}
