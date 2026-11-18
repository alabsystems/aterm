# Native final-exec policy

`FootprintLimit` prepares equal positive active/inactive fatal footprint limits
using Apple's native spawn-attribute setter. It does not launch, reserve memory,
measure RSS, install a sandbox, or manufacture a policy receipt. The launch owner
must apply it at the final payload exec and retain actual child custody.

The ABI and fatal flags follow Apple's maintained
[libsyscall setter](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/spawn/posix_spawn.c)
and [spawn attributes](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/spawn_internal.h).
Setter success alone is no evidence of a live child's installed policy or of a
hard RSS quota. Unsupported native policy must remain an explicit refusal.

On macOS, `ExecutionSeal` installs one of two fixed self-restrictions: no fork,
or no fork and no exec. There is no caller-supplied sandbox profile. Installation
is irreversible; an error is not a rollback and must not trigger an ordinary
payload fallback. `fork_denied(pid)` is only a native observation and requires
separate process-lifetime authentication. There is deliberately no global
no-exec query API: `sandbox_check(process-exec)` returned EINVAL on the qualified
Mac with both no filter and a path filter. The fixed no-exec profile is instead
qualified by an actual owned child's rejected fork and `/usr/bin/true` exec.
Neither its local installation result nor an observation grants resources.

Native qualification also found that installing a second sandbox after the
no-fork profile is refused. Managed final-payload integration must not assume
that a prelude's first sandbox can be tightened after exec.

`install_no_fork_closed_exec` supplies a distinct first-install restriction:
no fork and exec only at up to sixteen held original executable names. Its
policy source is fixed; names are native parameters. This unsafe self restriction
creates no owner or no-exec observation. The provenance `HeldExecImageSet`
retains whole original owners through checked SAME-program borrows, original
credential/deadline bookends, sticky refusal and one self-install attempt. Its
opaque `PreparedExecTransfer` binds the actual initial member and retains that
same population for a separate authenticated prelude. Each prelude still owns
its one plan and real child; name DATA cannot reconstruct the transfer. Native
capture and installation remain unavailable outside the qualified host platform.
An external lifetime
owner must still hold those SAME actual inputs across self-exec and retirement;
local Rust guards and CLOEXEC descriptors do not survive exec. Name filtering
is not descriptor execution or complete-chain/CLT selection authority. Original
runtime/search inputs, no concurrent same-UID name writers, actual child/GO/EOF
settlement and semantic validation remain separate duties. Existing NoFork and
query/object profiles are unchanged. The standalone `closed_exec_native` actors
exercise initial/later exec, unowned exec/fork denial, original-input failures,
and refusal to reuse or tighten an old seal; they are no Host or SDK validator.

`ForkingGroupRestriction` is a separate prospective constraint for a trusted
forking producer. At its original single-threaded session-leader entry it installs
one fixed Darwin syscall filter denying `setpgid` (82), `setsid` (147) and
`posix_spawn` (244). Setter denial alone is insufficient: actual spawn attributes
still escape into a new group/session. The full filter also rejects ordinary
`posix_spawn`; `configure_fork_exec` selects the standard library's fork/exec path
upfront through a fixed pre-exec group check. It never retries a failed spawn.
The native fixture requires real unrestricted and setter-only escapes, raw syscall
refusals, a successful configured fork/exec and inherited refusals after exec.

The syscall hook is Apple's
[`mpo_proc_check_syscall_unix`](https://github.com/apple-oss-distributions/xnu/blob/main/security/mac_policy.h),
not a dedicated group-change MAC hook. These exact current-kernel controls do not
attest another process's installed policy. The marker retains no child, CPU,
source, configuration, SDK, output or deadline owner and dropping it returns none.
The actual native owner must separately retain the original unreaped leader,
funded cohort, complete inputs and original endpoint through settlement. External
Mach/launchd/proxied process creation and general non-FD isolation remain outside
this constraint. NoFork roles remain separate. Bootstrap Seed admission stays
closed until its genuine producer, descendants and return route are connected.

`restrict_current_to_readonly_image` supplies a separate fixed deny-default
policy for a trusted native-worker entry. It requires an already-open regular
file, an `O_RDONLY` descriptor, and `MNT_RDONLY` from `fstatfs` on that descriptor.
It uses the descriptor's native `F_GETPATH` and its exact `/dev/fd/N` alias as
parameters to `sandbox_init_with_parameters`; pathname bytes are never inserted
into policy source. The parameter ABI is also used by
[Chromium](https://chromium.googlesource.com/chromium/src/+/master/sandbox/mac/seatbelt.cc).
The returned guard retains the descriptor for the native loader.

This is a local OS restriction, not a complete worker or immutable-artifact
receipt. The caller must authenticate bytes and the complete runtime, retain
the read-only mount and descriptor through the child lifetime, close ambient
descriptors, set resource limits, and own cancellation/kill/reap. The policy
does not make existing descriptors harmless. It must precede native
constructors; installation failure is terminal, with no ordinary fallback.
The compiler's PM-1 refusal is unchanged.

Darwin refuses the measured ad-hoc-signed DSO when its own file is unlinked
before `dlopen`. The qualified fixture instead leaves that file linked inside
a read-only mounted image and unlinks the image's backing pathname. The native
test requires actual file-read/write, network, fork and exec denials before
loading the real proc-macro DSO, then exercises all 32 bridge methods. Its
runner also puts quotes and a backslash in the mount path to exercise native
parameter handling. This establishes that bounded case, not general loader or
production macro admission.

`restrict_current_native_entry` adds the preceding descriptor-capability step
for a dedicated Darwin trusted entry. It owns exactly one connected Unix control
stream and the read-only image. It refuses a nonempty environment, anything
except the sole main thread, or a control peer whose kernel PID/euid differs from
the expected current parent. It blocks all blockable signals, clears the task's
three-slot registered-port inheritance stash and removes all current local SEND
user references to those names, including cached and duplicate lookup aliases.
Required self-task/thread aliases, dead names and surviving receive/other roles
refuse. The implementation follows the installed SDK and
[Apple's own-task registered-port wrappers](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/mach/task.c),
using get-refs/mod-refs rather than deprecated port destruction.

It closes every other descriptor (including standard I/O and aliases), requires
an exact kernel census and unchanged retained identities/flags, then installs
the same first fixed image restriction. Native setup can leave a socket in a
freed low FD slot: a final bounded close/census/identity check under the unchanged
policy removes it before returning. Control and sole-main-thread checks also
repeat after installation. A full, malformed or unavailable bounded census refuses;
no incomplete census, close retry, arbitrary descriptor allowlist or weaker
profile is accepted. Every error is terminal, including partial restriction.
The unsafe caller contract excludes other Rust FD owners and concurrent
environment/descriptor/Mach-port/signal mutation; registered names must be
disposable. This is deliberately a standalone entry, not a libtest worker thread.

`targo --unverified test --locked --manifest-path Cargo.toml --all-targets` in
this directory runs an owned standalone child and a real C DSO initializer.
An unrestricted control must actually write the inherited file/socket/control
alias and read the image alias. The restricted initializer must receive EBADF
on all four, while the exact loader image and compiler endpoint still work.
The same two controls run with input capabilities arranged in a reversed
destination-bank collision; all sources are first duplicated above that bank.
The environment, writable image, wrong control role, wrong parent, extra thread
and full kernel-census cases must refuse before the constructor, with the
specific refusal and unchanged witnesses. Own-task/thread aliases and surviving
registered receive roles refuse too.

The registered-port controls use native `posix_spawn` with no parent pre-exec
callback. The matching unrestricted real constructor must deliver its cached
send to the parent's owned sentinel port; a successful send return alone does
not pass. Three cached lookup references and duplicate entries in all three
stash slots are covered. The restricted constructor must attempt that same
remembered name, receive exact `MACH_SEND_INVALID_DEST`, and deliver no message.
The old restriction admitted actual sentinel delivery. A diagnostic fork/exec
path reported a successful send without delivery and was rejected as a control;
it cannot manufacture a green result. The suite has 17 native scenarios. The
owned read-only mount is detached after child reap; no missing-tool success
path exists.

This step creates no `Ready`, process-lifetime, native-closure or provider
authority. It does not authenticate the image bytes, read-only backing/mount,
full dynamic runtime, existing mappings, or complete Mach/non-FD capabilities.
Clearing the registered stash and current SEND names does not destroy an
underlying port reachable through special/exception/bootstrap aliases, send-once
or other retained rights; these may permit reacquisition. This step establishes
no resource envelope. Immutable closure,
outside deadline/cancellation/kill/reap ownership, persistent per-dylib state,
compiler-mediated source/environment consumption and production loader
integration remain required. PM-1 and D29 remain unchanged.
