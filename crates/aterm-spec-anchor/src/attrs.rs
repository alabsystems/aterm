// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The anchor attributes (`refines`, `spec_unmodeled`) as `macro_rules!` attributes.

/// Bind a fn to the model action it performs: `machine`, `action` and `project`, all required.
#[macro_export]
macro_rules! refines {
    attr(machine = $m:literal, action = $a:literal, project = "" $(,)?) ($($item:tt)*) => {
        ::core::compile_error! {
            "missing `project` key: name the fn that projects the real state onto the model's \
             variables"
        }
        $($item)*
    };
    attr($($args:tt)*) ($(# $at:tt)* $vis:vis fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { # ! $($b:tt)* }) => {
        $crate::__aterm_spec_anchor! { @keys refines [] [] [] [] ($($args)*)
            ($(# $at)* $vis fn $n ($($ps)*) $(-> $r)? { # ! $($b)* }) }
    };
    attr($($args:tt)*)
        ($(# $at:tt)* $vis:vis const fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { # ! $($b:tt)* }) => {
        $crate::__aterm_spec_anchor! { @keys refines [] [] [] [] ($($args)*)
            ($(# $at)* $vis const fn $n ($($ps)*) $(-> $r)? { # ! $($b)* }) }
    };
    attr(machine = $m:literal, action = $a:literal, project = $p:literal $(,)?)
        ($(# $at:tt)* $vis:vis fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { $($b:tt)* }) => {
        $(# $at)* $vis fn $n ($($ps)*) $(-> $r)? {
            $crate::__aterm_spec_anchor! { @record refines $m $a $p $n
                (::core::concat!($m, "::", $a, " @ ", ::core::stringify!($n))) }
            $crate::xref::note_entered(::core::concat!($m, "::", $a, " @ ", ::core::stringify!($n)));
            $($b)*
        }
    };
    attr(machine = $m:literal, action = $a:literal, project = $p:literal $(,)?)
        ($(# $at:tt)* $vis:vis const fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { $($b:tt)* }) => {
        $(# $at)* $vis const fn $n ($($ps)*) $(-> $r)? {
            $crate::__aterm_spec_anchor! { @record refines $m $a $p $n ("") }
            $($b)*
        }
    };
    attr($($args:tt)*) ($($item:tt)*) => {
        $crate::__aterm_spec_anchor! { @keys refines [] [] [] [] ($($args)*) ($($item)*) }
    };
}

/// Waive a fn the model does not describe.
#[macro_export]
macro_rules! spec_unmodeled {
    attr($($args:tt)*) ($(# $at:tt)* $vis:vis fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { # ! $($b:tt)* }) => {
        $crate::__aterm_spec_anchor! { @keys unmodeled [] [] [] [] ($($args)*)
            ($(# $at)* $vis fn $n ($($ps)*) $(-> $r)? { # ! $($b)* }) }
    };
    attr(machine = $m:literal, action = $a:literal, reason = $why:literal $(,)?)
        ($(# $at:tt)* $vis:vis fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { $($b:tt)* }) => {
        $(# $at)* $vis fn $n ($($ps)*) $(-> $r)? {
            $crate::__aterm_spec_anchor! { @record unmodeled $m $a $why $n }
            $($b)*
        }
    };
    attr(reason = $why:literal $(,)?)
        ($(# $at:tt)* $vis:vis fn $n:ident ($($ps:tt)*) $(-> $r:ty)? { $($b:tt)* }) => {
        $(# $at)* $vis fn $n ($($ps)*) $(-> $r)? {
            $crate::__aterm_spec_anchor! { @record unmodeled "" "" $why $n }
            $($b)*
        }
    };
    attr($($args:tt)*) ($($item:tt)*) => {
        $crate::__aterm_spec_anchor! { @keys unmodeled [] [] [] [] ($($args)*) ($($item)*) }
    };
}

/// The anchor attributes' general path: the keys in any order, any fn shape.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_spec_anchor {
    (@keys $k:ident [$($m:tt)?] [$($a:tt)?] [$($p:tt)?] [$($r:tt)?]
        (machine = $v:literal $(, $($rest:tt)*)?) $item:tt) => {
        $crate::__aterm_spec_anchor! { @keys $k [$v] [$($a)?] [$($p)?] [$($r)?]
            ($($($rest)*)?) $item }
    };
    (@keys $k:ident [$($m:tt)?] [$($a:tt)?] [$($p:tt)?] [$($r:tt)?]
        (action = $v:literal $(, $($rest:tt)*)?) $item:tt) => {
        $crate::__aterm_spec_anchor! { @keys $k [$($m)?] [$v] [$($p)?] [$($r)?]
            ($($($rest)*)?) $item }
    };
    (@keys $k:ident [$($m:tt)?] [$($a:tt)?] [$($p:tt)?] [$($r:tt)?]
        (project = $v:literal $(, $($rest:tt)*)?) $item:tt) => {
        $crate::__aterm_spec_anchor! { @keys $k [$($m)?] [$($a)?] [$v] [$($r)?]
            ($($($rest)*)?) $item }
    };
    (@keys $k:ident [$($m:tt)?] [$($a:tt)?] [$($p:tt)?] [$($r:tt)?]
        (reason = $v:literal $(, $($rest:tt)*)?) $item:tt) => {
        $crate::__aterm_spec_anchor! { @keys $k [$($m)?] [$($a)?] [$($p)?] [$v]
            ($($($rest)*)?) $item }
    };
    (@keys $k:ident $m:tt $a:tt $p:tt $r:tt () $item:tt) => {
        $crate::__aterm_spec_anchor! { @check $k $m $a $p $r $item }
    };
    (@keys $k:ident $m:tt $a:tt $p:tt $r:tt ($key:ident = $($rest:tt)*) ($($item:tt)*)) => {
        ::core::compile_error! {
            ::core::concat!(
                "`", ::core::stringify!($key),
                "`: not a key this attribute takes, or its value is not a string literal"
            )
        }
        $($item)*
    };
    (@keys $k:ident $m:tt $a:tt $p:tt $r:tt ($($bad:tt)*) ($($item:tt)*)) => {
        ::core::compile_error! { "expected `key = \"value\"` pairs separated by `,`" }
        $($item)*
    };

    (@check refines [$m:literal] [$a:literal] [""] [] ($($item:tt)*)) => {
        ::core::compile_error! {
            "missing `project` key: name the fn that projects the real state onto the model's \
             variables"
        }
        $($item)*
    };
    (@check refines [$m:literal] [$a:literal] [$p:literal] [] $item:tt) => {
        $crate::__aterm_spec_anchor! { @head [refines $m $a $p] $item $item }
    };
    (@check refines [] $a:tt $p:tt $r:tt ($($item:tt)*)) => {
        ::core::compile_error! { "missing `machine` key" }
        $($item)*
    };
    (@check refines $m:tt [] $p:tt $r:tt ($($item:tt)*)) => {
        ::core::compile_error! { "missing `action` key" }
        $($item)*
    };
    (@check refines $m:tt $a:tt [] $r:tt ($($item:tt)*)) => {
        ::core::compile_error! {
            "missing `project` key: name the fn that projects the real state onto the model's \
             variables"
        }
        $($item)*
    };
    (@check refines $m:tt $a:tt $p:tt $r:tt ($($item:tt)*)) => {
        ::core::compile_error! { "`refines` takes `machine`, `action` and `project`" }
        $($item)*
    };
    (@check unmodeled [$($m:literal)?] [$($a:literal)?] [] [$r:literal] $item:tt) => {
        $crate::__aterm_spec_anchor! { @head [unmodeled [$($m)? ""] [$($a)? ""] $r] $item $item }
    };
    (@check unmodeled $m:tt $a:tt [] [] ($($item:tt)*)) => {
        ::core::compile_error! { "missing `reason` key" }
        $($item)*
    };
    (@check unmodeled $m:tt $a:tt $p:tt $r:tt ($($item:tt)*)) => {
        ::core::compile_error! { "`spec_unmodeled` takes `reason`, `machine` and `action`" }
        $($item)*
    };

    (@head $ctx:tt $item:tt ($(# $at:tt)* $vis:vis fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n probe] [] $item }
    };
    (@head $ctx:tt $item:tt ($(# $at:tt)* $vis:vis const fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n noprobe] [] $item }
    };
    (@head $ctx:tt $item:tt ($(# $at:tt)* $vis:vis const unsafe fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n noprobe] [] $item }
    };
    (@head $ctx:tt $item:tt ($(# $at:tt)* $vis:vis unsafe fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n probe] [] $item }
    };
    (@head $ctx:tt $item:tt ($(# $at:tt)* $vis:vis async $(unsafe)? fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n probe] [] $item }
    };
    (@head $ctx:tt $item:tt
        ($(# $at:tt)* $vis:vis $(unsafe)? extern $($abi:literal)? fn $n:ident $($rest:tt)*)) => {
        $crate::__aterm_spec_anchor! { @split [$ctx $n probe] [] $item }
    };
    (@head $ctx:tt ($($item:tt)*) $analysed:tt) => {
        ::core::compile_error! {
            "an anchor goes on a fn with a body: the record is an item inside it"
        }
        $($item)*
    };

    (@split $ctx:tt [$($h:tt)*] ({ $($b:tt)* })) => {
        $crate::__aterm_spec_anchor! { @emit $ctx [$($h)*] { $($b)* } }
    };
    (@split $ctx:tt [$($h:tt)*]
        ($t1:tt $t2:tt $t3:tt $t4:tt $t5:tt $t6:tt $t7:tt $t8:tt
         $t9:tt $t10:tt $t11:tt $t12:tt $t13:tt $t14:tt $t15:tt $t16:tt $($rest:tt)+)) => {
        $crate::__aterm_spec_anchor! { @split $ctx
            [$($h)* $t1 $t2 $t3 $t4 $t5 $t6 $t7 $t8 $t9 $t10 $t11 $t12 $t13 $t14 $t15 $t16]
            ($($rest)+) }
    };
    (@split $ctx:tt [$($h:tt)*] ($t1:tt $t2:tt $t3:tt $t4:tt $($rest:tt)+)) => {
        $crate::__aterm_spec_anchor! { @split $ctx [$($h)* $t1 $t2 $t3 $t4] ($($rest)+) }
    };
    (@split $ctx:tt [$($h:tt)*] ($t:tt $($rest:tt)+)) => {
        $crate::__aterm_spec_anchor! { @split $ctx [$($h)* $t] ($($rest)+) }
    };
    (@split $ctx:tt [$($h:tt)*] ($last:tt)) => {
        ::core::compile_error! {
            "an anchor goes on a fn with a body: the record is an item inside it"
        }
        $($h)* $last
    };

    (@emit $ctx:tt $head:tt { # ! [$($ia:tt)*] $($b:tt)* }) => {
        $crate::__aterm_spec_anchor! { @inner $ctx $head [#![$($ia)*]] { $($b)* } }
    };
    (@inner $ctx:tt $head:tt [$($inner:tt)*] { # ! [$($ia:tt)*] $($b:tt)* }) => {
        $crate::__aterm_spec_anchor! { @inner $ctx $head
            [$($inner)* #![$($ia)*]] { $($b)* } }
    };
    (@inner [$ctx:tt $n:ident $probe:ident] [$($h:tt)*] [$($inner:tt)*] { $($b:tt)* }) => {
        $($h)* {
            $($inner)*
            $crate::__aterm_spec_anchor! { @body $ctx $n $probe }
            $($b)*
        }
    };
    (@emit [$ctx:tt $n:ident $probe:ident] [$($h:tt)*] { $($b:tt)* }) => {
        $($h)* {
            $crate::__aterm_spec_anchor! { @body $ctx $n $probe }
            $($b)*
        }
    };
    (@body [refines $m:literal $a:literal $p:literal] $n:ident probe) => {
        $crate::__aterm_spec_anchor! { @record refines $m $a $p $n
            (::core::concat!($m, "::", $a, " @ ", ::core::stringify!($n))) }
        $crate::xref::note_entered(::core::concat!($m, "::", $a, " @ ", ::core::stringify!($n)));
    };
    (@body [refines $m:literal $a:literal $p:literal] $n:ident noprobe) => {
        $crate::__aterm_spec_anchor! { @record refines $m $a $p $n ("") }
    };
    (@body [unmodeled [$m:literal $($dm:literal)?] [$a:literal $($da:literal)?] $r:literal]
        $n:ident $probe:ident) => {
        $crate::__aterm_spec_anchor! { @record unmodeled $m $a $r $n }
    };

    (@record refines $m:literal $a:literal $p:literal $n:ident ($($id:tt)*)) => {
        #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android", windows))]
        $crate::registry::submit! {
            $crate::xref::RefinementAnchor => $crate::xref::RefinementAnchor {
                machine: $m,
                action: $a,
                rust_method: ::core::stringify!($n),
                location: ::core::concat!(::core::file!(), ":", ::core::line!()),
                project: $p,
                entry_id: $($id)*,
            }
        }
    };
    (@record unmodeled $m:literal $a:literal $r:literal $n:ident) => {
        #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android", windows))]
        $crate::registry::submit! {
            $crate::xref::Waiver => $crate::xref::Waiver {
                machine: $m,
                action: $a,
                reason: $r,
                rust_method: ::core::stringify!($n),
                location: ::core::concat!(::core::file!(), ":", ::core::line!()),
            }
        }
    };
}
