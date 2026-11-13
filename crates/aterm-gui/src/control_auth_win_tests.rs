// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn ensure_private_dir_accepts_and_hardens_owned_dir() {
    let dir = std::env::temp_dir().join(format!("aterm-win-acl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).expect("current-user-owned dir must verify + harden");
    assert!(dir.is_dir());
    ensure_private_dir(&dir).expect("second pass over a hardened dir is idempotent");
    std::fs::write(dir.join("probe"), b"x").expect("owner can still write inside");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_elevated_descriptors_admit_an_elevated_token_and_refuse_the_unelevated_twin() {
    let elevated = aterm_uds::elevation::is_elevated();
    let twin = tokens::elevated_twin();
    if twin.is_none() {
        eprintln!("this account has no elevated token; the elevated side is not checked");
    }
    let me = tokens::own_user_sid();
    assert_eq!(
        tokens::granted(
            &format!("O:{me}D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
            &tokens::unelevated_twin()
        ),
        tokens::FILE_ALL_ACCESS,
        "control: the shared directory's posture admits the same user"
    );
    for sddl in [acl::ELEVATED_DIR_SDDL, acl::ELEVATED_FILE_SDDL] {
        assert_eq!(
            tokens::granted(sddl, &tokens::unelevated_twin()),
            0,
            "{sddl}: the same user, unelevated"
        );
        assert_eq!(
            tokens::granted(sddl, &tokens::own()),
            if elevated { tokens::FILE_ALL_ACCESS } else { 0 },
            "{sddl}: this process (elevated: {elevated})"
        );
        if let Some(twin) = &twin {
            assert_eq!(
                tokens::granted(sddl, twin),
                tokens::FILE_ALL_ACCESS,
                "{sddl}: the same user, elevated"
            );
        }
    }
}

#[test]
fn the_label_and_the_owner_rights_entry_each_refuse_the_unelevated_twin() {
    let twin = tokens::unelevated_twin();
    let data = tokens::FILE_READ_DATA | tokens::FILE_WRITE_DATA;
    assert_eq!(
        tokens::granted("O:BAD:(A;;FA;;;WD)", &twin),
        tokens::FILE_ALL_ACCESS,
        "control: the DACL alone admits the twin"
    );
    assert_eq!(
        tokens::granted("O:BAD:(A;;FA;;;WD)S:(ML;;NRNW;;;HI)", &twin) & data,
        0,
        "the High label refuses a Medium reader and writer"
    );
    let me = tokens::own_user_sid();
    let (sy, ba) = ("(A;;FA;;;SY)", "(A;;FA;;;BA)");
    assert_eq!(
        tokens::granted(&format!("O:{me}D:P{sy}{ba}"), &twin),
        tokens::READ_CONTROL | tokens::WRITE_DAC,
        "control: an owner's implicit rights"
    );
    assert_eq!(
        tokens::granted(&format!("O:{me}D:P{sy}{ba}(A;;RC;;;OW)"), &twin),
        tokens::READ_CONTROL,
        "OWNER RIGHTS holds the owner to READ_CONTROL"
    );
}

#[test]
fn a_socket_under_the_elevated_entries_refuses_an_unelevated_connect() {
    let dir = std::env::temp_dir().join(format!("aterm-elev-sock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let entries = acl::ELEVATED_DIR_SDDL
        .split_once("D:P")
        .and_then(|(_, rest)| rest.split_once("S:"))
        .map(|(dacl, _)| dacl.replace(";OICI;", ";OICIIO;"))
        .expect("the DACL part");
    let me = tokens::own_user_sid();
    tokens::set_dacl(&dir, &format!("D:P(A;;FA;;;{me}){entries}"));
    let sock = dir.join("t.sock");
    let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
    let dialed = aterm_uds::CtlStream::connect(&sock);
    if aterm_uds::elevation::is_elevated() {
        dialed.expect("Administrators connect");
    } else {
        let e = dialed.expect_err("an unelevated process must not connect");
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
    }
    drop(listener);
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_elevated_dir_is_never_made_or_adopted_without_elevation() {
    let elevated = aterm_uds::elevation::is_elevated();
    let base = std::env::temp_dir().join(format!("aterm-elev-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir(&base).unwrap();

    let fresh = base.join("fresh");
    let made = acl::ensure_elevated_dir(&fresh, false);
    assert_eq!(made.is_ok(), elevated, "{made:?}");
    assert_eq!(fresh.exists(), elevated, "nothing is left behind");

    let planted = base.join("planted");
    std::fs::create_dir(&planted).unwrap();
    let adopted = acl::ensure_elevated_dir(&planted, false);
    assert_eq!(adopted.is_ok(), elevated, "{adopted:?}");
    if !elevated {
        assert_eq!(
            adopted.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        std::fs::write(planted.join("probe"), b"x").expect("the refusal touched nothing");
    }

    let token = base.join("t.token");
    let created = acl::create_elevated_file(&token);
    assert_eq!(created.is_ok(), elevated, "{:?}", created.as_ref().err());
    drop(created);
    assert_eq!(token.exists(), elevated, "nothing is left behind");
    let _ = std::fs::remove_dir_all(&base);
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).expect("scratch dir");
    dir
}

#[test]
fn the_owner_check_refuses_a_folder_across_elevations() {
    let me = tokens::own_user_sid();
    let me_sid = tokens::Sid::parse(&me);
    if !aterm_uds::elevation::is_elevated() {
        assert_eq!(
            tokens::token_owner_sid(&tokens::own()),
            me,
            "an unelevated token's objects are the user's"
        );
    }
    let elevated_owner = tokens::elevated_twin()
        .map(|twin| tokens::token_owner_sid(&twin))
        .unwrap_or_else(|| {
            eprintln!("this account has no elevated token; its owner is taken as BA");
            "S-1-5-32-544".to_owned()
        });
    let elevated_sid = tokens::Sid::parse(&elevated_owner);
    assert_ne!(
        elevated_owner, me,
        "an elevated token owns what it makes as Administrators, not as the user"
    );

    let dir = scratch("aterm-owner-x");
    tokens::set_dacl(
        &dir,
        &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1200a9;;;WD)"),
    );
    let before = tokens::sddl_of(&dir);
    let owned_by = |sddl_owner: &str| {
        let sddl = format!("O:{sddl_owner}");
        move |_: &[u16]| acl::Descriptor::from_sddl(&sddl)
    };
    let refused = |token_owner: &tokens::Sid, dir_owner: &str| {
        let read = owned_by(dir_owner);
        let Err(err) = acl::verify_owner_and_harden_with(&dir, token_owner.ptr(), &read) else {
            panic!("a folder owned by {dir_owner} passed");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(tokens::sddl_of(&dir), before, "the refusal wrote nothing");
        err.to_string()
    };
    let unelevated_meets_elevated = refused(&me_sid, "BA");
    assert!(
        unelevated_meets_elevated.contains("Administrators"),
        "{unelevated_meets_elevated}"
    );
    let elevated_meets_unelevated = refused(&elevated_sid, &me);
    assert!(
        elevated_meets_unelevated.contains("not owned by the current user"),
        "{elevated_meets_unelevated}"
    );
    for stranger in ["SY", "WD", "S-1-5-21-1-2-3-1001"] {
        refused(&me_sid, stranger);
    }
    let ba = acl::Descriptor::from_sddl("O:BA").expect("O:BA");
    acl::owner_verdict(ba.owner().expect("owner"), elevated_sid.ptr())
        .expect("an elevated aterm keeps a folder it made");
    acl::verify_owner_and_harden_with(&dir, me_sid.ptr(), &acl::Descriptor::owner_of_path)
        .expect("the user's own folder");
    assert_ne!(
        tokens::sddl_of(&dir),
        before,
        "the pass hardened the folder"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_hardened_dacl_is_protected_and_shuts_everyone_else_out() {
    let me = tokens::own_user_sid();
    let base = scratch("aterm-dacl");
    tokens::set_dacl(&base, &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;WD)"));
    let child = base.join("child");
    std::fs::create_dir(&child).expect("child");
    let data = tokens::FILE_READ_DATA | tokens::FILE_WRITE_DATA;
    let inherited = tokens::sddl_of(&child);
    assert!(inherited.contains(";;;WD)"), "{inherited}");
    assert_eq!(
        tokens::granted(&inherited, &tokens::stranger()) & data,
        data,
        "control: the inherited entry admits a stranger"
    );

    ensure_private_dir(&child).expect("the user's own folder");
    let hardened = tokens::sddl_of(&child);
    let (flags, mut aces) = tokens::dacl_parts(&hardened);
    assert!(flags.contains('P'), "protected: {hardened}");
    aces.sort();
    let mut want = vec![
        format!("(A;OICI;FA;;;{me})"),
        "(A;OICI;FA;;;BA)".to_owned(),
        "(A;OICI;FA;;;SY)".to_owned(),
    ];
    want.sort();
    assert_eq!(aces, want, "{hardened}");
    let stranger = tokens::granted(&hardened, &tokens::stranger());
    assert_eq!(stranger & (data | tokens::WRITE_DAC), 0, "{stranger:#x}");
    assert_eq!(
        tokens::granted(&hardened, &tokens::unelevated_twin()),
        tokens::FILE_ALL_ACCESS
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_elevated_owner_check_takes_only_administrators_or_system() {
    let me = tokens::own_user_sid();
    let dir = std::path::Path::new(r"C:\scratch\aterm-elevated");
    for (owner, passes) in [
        ("BA", true),
        ("SY", true),
        (me.as_str(), false),
        ("WD", false),
        ("AU", false),
        ("S-1-5-21-1-2-3-1001", false),
    ] {
        let sd = acl::Descriptor::from_sddl(&format!("O:{owner}")).expect("SDDL");
        let verdict = acl::elevated_owner_verdict(sd.owner().expect("owner"), dir);
        assert_eq!(verdict.is_ok(), passes, "{owner}: {verdict:?}");
        if let Err(e) = verdict {
            assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        }
    }
}

#[test]
fn a_held_directory_cannot_be_renamed_nor_can_its_parents() {
    let base = std::env::temp_dir().join(format!("aterm-elev-pin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let held = base.join("a").join("b");
    std::fs::create_dir_all(&held).unwrap();
    let handle = acl::open_dir_nofollow(&held).expect("open the directory itself");
    assert!(std::fs::rename(&held, base.join("a").join("moved")).is_err());
    assert!(std::fs::rename(base.join("a"), base.join("moved")).is_err());
    drop(handle);
    std::fs::rename(base.join("a"), base.join("moved")).expect("free once closed");
    let _ = std::fs::remove_dir_all(&base);
}

mod tokens {
    #![allow(non_snake_case, reason = "Win32 API names are camel-case by contract")]

    use std::ffi::c_void;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    pub(super) const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    pub(super) const FILE_READ_DATA: u32 = 0x0001;
    pub(super) const FILE_WRITE_DATA: u32 = 0x0002;
    pub(super) const READ_CONTROL: u32 = 0x0002_0000;
    pub(super) const WRITE_DAC: u32 = 0x0004_0000;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
    const TOKEN_DUPLICATE: u32 = 0x0002;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
    const SECURITY_IDENTIFICATION: u32 = 1;
    const TOKEN_USER_CLASS: u32 = 1;
    const TOKEN_OWNER_CLASS: u32 = 4;
    const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    const TOKEN_LINKED_TOKEN_CLASS: u32 = 19;
    const TOKEN_ELEVATION_CLASS: u32 = 20;
    const TOKEN_INTEGRITY_LEVEL_CLASS: u32 = 25;
    const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;
    const SE_FILE_OBJECT: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

    #[repr(C)]
    struct SidAndAttributes {
        Sid: *mut c_void,
        Attributes: u32,
    }

    #[repr(C)]
    struct GenericMapping {
        GenericRead: u32,
        GenericWrite: u32,
        GenericExecute: u32,
        GenericAll: u32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            ProcessHandle: isize,
            DesiredAccess: u32,
            TokenHandle: *mut isize,
        ) -> i32;
        fn GetTokenInformation(
            TokenHandle: isize,
            TokenInformationClass: u32,
            TokenInformation: *mut c_void,
            TokenInformationLength: u32,
            ReturnLength: *mut u32,
        ) -> i32;
        fn SetTokenInformation(
            TokenHandle: isize,
            TokenInformationClass: u32,
            TokenInformation: *const c_void,
            TokenInformationLength: u32,
        ) -> i32;
        fn DuplicateToken(
            ExistingTokenHandle: isize,
            ImpersonationLevel: u32,
            DuplicateTokenHandle: *mut isize,
        ) -> i32;
        fn CreateRestrictedToken(
            ExistingTokenHandle: isize,
            Flags: u32,
            DisableSidCount: u32,
            SidsToDisable: *const SidAndAttributes,
            DeletePrivilegeCount: u32,
            PrivilegesToDelete: *const c_void,
            RestrictedSidCount: u32,
            SidsToRestrict: *const SidAndAttributes,
            NewTokenHandle: *mut isize,
        ) -> i32;
        fn AccessCheck(
            pSecurityDescriptor: *mut c_void,
            ClientToken: isize,
            DesiredAccess: u32,
            GenericMapping: *const GenericMapping,
            PrivilegeSet: *mut c_void,
            PrivilegeSetLength: *mut u32,
            GrantedAccess: *mut u32,
            AccessStatus: *mut i32,
        ) -> i32;
        fn ConvertStringSidToSidW(StringSid: *const u16, Sid: *mut *mut c_void) -> i32;
        fn ConvertSidToStringSidW(Sid: *mut c_void, StringSid: *mut *mut u16) -> i32;
        fn GetLengthSid(pSid: *mut c_void) -> u32;
        fn SetNamedSecurityInfoW(
            pObjectName: *mut u16,
            ObjectType: u32,
            SecurityInfo: u32,
            psidOwner: *mut c_void,
            psidGroup: *mut c_void,
            pDacl: *mut c_void,
            pSacl: *mut c_void,
        ) -> u32;
        fn GetSecurityDescriptorDacl(
            pSecurityDescriptor: *mut c_void,
            lpbDaclPresent: *mut i32,
            pDacl: *mut *mut c_void,
            lpbDaclDefaulted: *mut i32,
        ) -> i32;
        fn GetNamedSecurityInfoW(
            pObjectName: *const u16,
            ObjectType: u32,
            SecurityInfo: u32,
            ppsidOwner: *mut *mut c_void,
            ppsidGroup: *mut *mut c_void,
            ppDacl: *mut *mut c_void,
            ppSacl: *mut *mut c_void,
            ppSecurityDescriptor: *mut *mut c_void,
        ) -> u32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            SecurityDescriptor: *mut c_void,
            RequestedStringSDRevision: u32,
            SecurityInformation: u32,
            StringSecurityDescriptor: *mut *mut u16,
            StringSecurityDescriptorLen: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(hObject: isize) -> i32;
        fn LocalFree(hMem: *mut c_void) -> *mut c_void;
    }

    /// A token handle, closed on drop.
    pub(super) struct Token(isize);

    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: a live token handle this module opened; closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A SID parsed from its string form, freed on drop.
    pub(super) struct Sid(*mut c_void);

    impl Sid {
        pub(super) fn parse(text: &str) -> Self {
            let wide: Vec<u16> = text.encode_utf16().chain(once(0)).collect();
            let mut sid: *mut c_void = null_mut();
            // SAFETY: `wide` is NUL-terminated; we own `sid` on success.
            let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
            assert!(ok != 0 && !sid.is_null(), "SID {text}");
            Self(sid)
        }

        /// The SID itself.
        pub(super) fn ptr(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for Sid {
        fn drop(&mut self) {
            // SAFETY: ConvertStringSidToSidW LocalAlloc'd it; freed once.
            unsafe { LocalFree(self.0) };
        }
    }

    fn process_token(access: u32) -> Token {
        let mut handle: isize = 0;
        // SAFETY: the pseudo-handle is always valid; the out pointer is a local.
        let ok = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) };
        assert!(
            ok != 0,
            "OpenProcessToken: {}",
            std::io::Error::last_os_error()
        );
        Token(handle)
    }

    fn for_access_check(token: &Token) -> Token {
        let mut handle: isize = 0;
        // SAFETY: `token` is live with TOKEN_DUPLICATE; the out pointer is a local.
        let ok = unsafe { DuplicateToken(token.0, SECURITY_IDENTIFICATION, &mut handle) };
        assert!(
            ok != 0,
            "DuplicateToken: {}",
            std::io::Error::last_os_error()
        );
        Token(handle)
    }

    fn token_u32(token: &Token, class: u32) -> Option<u32> {
        let (mut value, mut len): (u32, u32) = (0, 0);
        // SAFETY: the buffer is one live `u32` and `len` a local.
        let ok =
            unsafe { GetTokenInformation(token.0, class, (&raw mut value).cast(), 4, &mut len) };
        (ok != 0).then_some(value)
    }

    /// This process's token.
    pub(super) fn own() -> Token {
        for_access_check(&process_token(TOKEN_DUPLICATE | TOKEN_QUERY))
    }

    /// This user's token as an UNELEVATED process of theirs holds it.
    pub(super) fn unelevated_twin() -> Token {
        restricted(&["S-1-5-32-544", "S-1-5-114"])
    }

    /// A token of SOMEONE ELSE on this machine.
    pub(super) fn stranger() -> Token {
        let me = own_user_sid();
        restricted(&[me.as_str(), "S-1-5-32-544", "S-1-5-114"])
    }

    fn restricted(deny_only: &[&str]) -> Token {
        let base = process_token(
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
        );
        let sids: Vec<Sid> = deny_only.iter().map(|text| Sid::parse(text)).collect();
        let disable: Vec<SidAndAttributes> = sids
            .iter()
            .map(|sid| SidAndAttributes {
                Sid: sid.0,
                Attributes: 0,
            })
            .collect();
        let mut handle: isize = 0;
        // SAFETY: `base` is live; `disable` holds live SIDs (owned by `sids`); no privileges or
        // restricting SIDs; the out pointer is a local.
        let ok = unsafe {
            CreateRestrictedToken(
                base.0,
                0,
                u32::try_from(disable.len()).expect("a few SIDs"),
                disable.as_ptr(),
                0,
                null(),
                0,
                null(),
                &mut handle,
            )
        };
        assert!(
            ok != 0,
            "CreateRestrictedToken: {}",
            std::io::Error::last_os_error()
        );
        let restricted = Token(handle);
        let medium = Sid::parse("S-1-16-8192");
        let label = SidAndAttributes {
            Sid: medium.0,
            Attributes: SE_GROUP_INTEGRITY,
        };
        // SAFETY: `label` is a live TOKEN_MANDATORY_LABEL whose SID lives in `medium`; lowering
        // a token's integrity needs TOKEN_ADJUST_DEFAULT only.
        let ok = unsafe {
            SetTokenInformation(
                restricted.0,
                TOKEN_INTEGRITY_LEVEL_CLASS,
                (&raw const label).cast(),
                std::mem::size_of::<SidAndAttributes>() as u32 + GetLengthSid(medium.0),
            )
        };
        assert!(
            ok != 0,
            "SetTokenInformation: {}",
            std::io::Error::last_os_error()
        );
        for_access_check(&restricted)
    }

    /// This user's ELEVATED token.
    pub(super) fn elevated_twin() -> Option<Token> {
        if aterm_uds::elevation::is_elevated() {
            return Some(own());
        }
        let base = process_token(TOKEN_QUERY);
        let (mut linked, mut len): (isize, u32) = (0, 0);
        // SAFETY: `base` is live with TOKEN_QUERY; TOKEN_LINKED_TOKEN is one handle, written
        // into the live local.
        let ok = unsafe {
            GetTokenInformation(
                base.0,
                TOKEN_LINKED_TOKEN_CLASS,
                (&raw mut linked).cast(),
                std::mem::size_of::<isize>() as u32,
                &mut len,
            )
        };
        if ok == 0 || linked == 0 {
            return None;
        }
        let linked = Token(linked);
        (token_u32(&linked, TOKEN_ELEVATION_CLASS)? != 0).then_some(linked)
    }

    /// What `token` is granted (`MAXIMUM_ALLOWED`; 0 when nothing) on an object with descriptor
    /// `sddl`.
    pub(super) fn granted(sddl: &str, token: &Token) -> u32 {
        let at = sddl.find("D:").expect("a DACL");
        let sddl = format!("{}G:SY{}", &sddl[..at], &sddl[at..]);
        let sd = super::super::acl::Descriptor::from_sddl(&sddl).expect("the SDDL parses");
        let mapping = GenericMapping {
            GenericRead: 0x0012_0089,
            GenericWrite: 0x0012_0116,
            GenericExecute: 0x0012_00A0,
            GenericAll: FILE_ALL_ACCESS,
        };
        let mut privileges = [0u32; 64];
        let mut privileges_len = std::mem::size_of_val(&privileges) as u32;
        let (mut granted, mut status): (u32, i32) = (0, 0);
        // SAFETY: `sd` is a live descriptor and `token` a live impersonation token; every out
        // pointer is a live local of the size given.
        let ok = unsafe {
            AccessCheck(
                sd.as_ptr(),
                token.0,
                MAXIMUM_ALLOWED,
                &mapping,
                privileges.as_mut_ptr().cast(),
                &mut privileges_len,
                &mut granted,
                &mut status,
            )
        };
        assert!(
            ok != 0,
            "AccessCheck {sddl}: {}",
            std::io::Error::last_os_error()
        );
        if status != 0 { granted } else { 0 }
    }

    /// This process's user SID, as text.
    pub(super) fn own_user_sid() -> String {
        token_sid(&process_token(TOKEN_QUERY), TOKEN_USER_CLASS)
    }

    /// The SID `token`'s objects are owned by (`TokenOwner`).
    pub(super) fn token_owner_sid(token: &Token) -> String {
        token_sid(token, TOKEN_OWNER_CLASS)
    }

    fn token_sid(token: &Token, class: u32) -> String {
        let mut len: u32 = 0;
        // SAFETY: a null buffer of length 0 asks for the length.
        unsafe { GetTokenInformation(token.0, class, null_mut(), 0, &mut len) };
        let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: `buf` holds at least `len` bytes, pointer-aligned.
        let ok =
            unsafe { GetTokenInformation(token.0, class, buf.as_mut_ptr().cast(), len, &mut len) };
        assert!(
            ok != 0,
            "token class {class}: {}",
            std::io::Error::last_os_error()
        );
        sid_text(buf[0] as *mut c_void)
    }

    fn sid_text(sid: *mut c_void) -> String {
        let mut text: *mut u16 = null_mut();
        // SAFETY: `sid` is a live SID; we own `text` on success.
        let ok = unsafe { ConvertSidToStringSidW(sid, &mut text) };
        assert!(ok != 0 && !text.is_null(), "ConvertSidToStringSidW");
        take_local_wide(text)
    }

    fn take_local_wide(text: *mut u16) -> String {
        let mut n = 0;
        // SAFETY: `text` is NUL-terminated.
        while unsafe { *text.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: `n` u16s precede the NUL in the same allocation.
        let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
        // SAFETY: the caller's API LocalAlloc'd it; freed once.
        unsafe { LocalFree(text.cast()) };
        out
    }

    /// The owner and DACL of the file or directory at `path`.
    pub(super) fn sddl_of(path: &Path) -> String {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
        let mut psd: *mut c_void = null_mut();
        let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        // SAFETY: `wide` is a NUL-terminated path; only the descriptor is asked for, and we own
        // it on success.
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                info,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut psd,
            )
        };
        assert_eq!(rc, 0, "GetNamedSecurityInfoW {}", path.display());
        let mut text: *mut u16 = null_mut();
        // SAFETY: `psd` is the live descriptor just read; we own `text` on success.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                psd,
                1,
                info,
                &mut text,
                null_mut(),
            )
        };
        // SAFETY: GetNamedSecurityInfoW LocalAlloc'd it; freed once.
        unsafe { LocalFree(psd) };
        assert!(ok != 0 && !text.is_null(), "to SDDL");
        take_local_wide(text)
    }

    /// The DACL part of `sddl`.
    pub(super) fn dacl_parts(sddl: &str) -> (String, Vec<String>) {
        let dacl = &sddl[sddl.find("D:").expect("a DACL") + 2..];
        let dacl = dacl.split("S:").next().unwrap_or(dacl);
        let open = dacl.find('(').unwrap_or(dacl.len());
        let aces = dacl[open..]
            .split_inclusive(')')
            .map(str::to_owned)
            .collect();
        (dacl[..open].to_owned(), aces)
    }

    /// Replace `dir`'s DACL with `sddl`'s, protected.
    pub(super) fn set_dacl(dir: &Path, sddl: &str) {
        let sd = super::super::acl::Descriptor::from_sddl(sddl).expect("the SDDL parses");
        let (mut present, mut defaulted): (i32, i32) = (0, 0);
        let mut dacl: *mut c_void = null_mut();
        // SAFETY: `sd` is a live descriptor; out-params are locals.
        let ok = unsafe {
            GetSecurityDescriptorDacl(sd.as_ptr(), &mut present, &mut dacl, &mut defaulted)
        };
        assert!(ok != 0, "GetSecurityDescriptorDacl");
        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().chain(once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated path; `dacl` points into `sd`.
        let rc = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        assert_eq!(rc, 0, "SetNamedSecurityInfoW {}", dir.display());
    }
}
