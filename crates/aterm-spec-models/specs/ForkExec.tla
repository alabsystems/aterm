----------------------------- MODULE ForkExec -----------------------------
\* aterm PTY-spawn SAFETY family (ATERM_DESIGN WS-G): the fork->exec child window
\* of aterm-pty's `spawn_shell` (lib.rs), fixed in commit 3d58709.
\*
\* Executable model of the child branch of `forkpty` in `spawn_shell`:
\*
\*   if pid == 0 {                       \* CHILD
\*       setrlimit(...);                 \* async-signal-safe
\*       chdir($HOME);                   \* async-signal-safe
\*       close(master);                  \* async-signal-safe, MUST precede exec
\*       execve(shell, argv, envp);      \* pre-built envp -> no alloc/env-lock
\*   }
\*
\* The frontend is MULTI-THREADED (GPU/Metal + socket threads are live), so
\* between fork() and exec() POSIX permits ONLY async-signal-safe calls: a lock a
\* now-vanished thread held (std's ENV_LOCK via var_os/setenv, or the allocator
\* via CString/format!/Vec) would deadlock or, with the macOS Obj-C runtime,
\* hard-abort the child. The pre-fix code ran `setenv`, `var_os`, `current_dir`
\* and heap allocation in the child (UB), AND leaked the forkpty MASTER fd into
\* the exec'd shell (never close()d before exec).
\*
\* The child is modeled as an ordered program counter `pc` walking a fixed step
\* list (BeforeFork -> Forked -> ... -> Execed). Two latched BOOLEAN flags record
\* what happened in the fork..exec window: `unsafeOpRan` (any non-async-signal-
\* safe op executed after Forked and before Execed) and `masterClosed` (the
\* master fd was close()d). Properties worth proving:
\*
\*   - OnlySafeBeforeExec:   if exec was reached, NO unsafe op ran in the window
\*     (no setenv/malloc/env-lock between fork and exec -- no deadlock hazard).
\*   - MasterClosedBeforeExec: if exec was reached, the master fd was closed
\*     first (it never leaks into the shell or anything the shell spawns).
\*   - SafeImpliesEnvPrebuilt: an unsafe op is the ONLY way env work reaches the
\*     window; the fix moves all env/alloc to the parent, so envPrebuilt holds
\*     whenever no unsafe op ran -- exec always has a usable pre-built envp.
\*
\* pc is a small Natural (step index); the flags are BOOLEAN, so ty's flat-state
\* engine represents the whole child trajectory directly.

EXTENDS Naturals

CONSTANTS Buggy

\* Named child program-counter positions (indices into the fixed step list).
BeforeFork == 0   \* parent side, about to forkpty
Forked     == 1   \* child branch entered; fork..exec window is now OPEN
DidRlimit  == 2   \* setrlimit applied (async-signal-safe)
DidChdir   == 3   \* chdir($HOME) (async-signal-safe)
DidClose   == 4   \* close(master) (async-signal-safe); master no longer leaks
Execed     == 5   \* execve reached; the window is CLOSED

VARIABLES
    pc,           \* child program counter (one of the named positions above)
    masterClosed, \* has the inherited forkpty master fd been close()d?
    unsafeOpRan,  \* did any NON-async-signal-safe op run in the fork..exec window?
    envPrebuilt   \* was envp built in the PARENT before forkpty (the fix)?

vars == << pc, masterClosed, unsafeOpRan, envPrebuilt >>

\* The window is open exactly while we are in the child and have not yet exec'd.
InWindow == pc >= Forked /\ pc < Execed

Init ==
    /\ pc = BeforeFork
    \* The fix builds envp in the parent before forkpty; the bug deferred env
    \* work into the child. envPrebuilt is set up-front by the parent path.
    /\ envPrebuilt = ~Buggy
    /\ masterClosed = FALSE
    /\ unsafeOpRan = FALSE

\* forkpty: parent returns to caller; child enters its branch (window opens).
Fork ==
    /\ pc = BeforeFork
    /\ pc' = Forked
    /\ UNCHANGED << masterClosed, unsafeOpRan, envPrebuilt >>

\* setrlimit via aterm_sandbox: async-signal-safe with a valid cap (no alloc).
Setrlimit ==
    /\ pc = Forked
    /\ pc' = DidRlimit
    /\ UNCHANGED << masterClosed, unsafeOpRan, envPrebuilt >>

\* chdir($HOME): async-signal-safe; the path was resolved in the parent.
Chdir ==
    /\ pc = DidRlimit
    /\ pc' = DidChdir
    /\ UNCHANGED << masterClosed, unsafeOpRan, envPrebuilt >>

\* close(master): async-signal-safe; latches masterClosed so the fd cannot leak
\* across exec. THIS is the step the pre-fix child skipped entirely.
CloseMaster ==
    /\ pc = DidChdir
    /\ IF Buggy
       THEN /\ pc' = DidClose       \* BUG: advance WITHOUT closing the master fd
            /\ masterClosed' = FALSE
       ELSE /\ pc' = DidClose       \* FIX: actually close(master) before exec
            /\ masterClosed' = TRUE
    /\ UNCHANGED << unsafeOpRan, envPrebuilt >>

\* The defect: the pre-fix child resolved env / called setenv / allocated INSIDE
\* the fork..exec window (var_os, current_dir, CString, format!, Vec, setenv) --
\* every one of those is async-signal-UNSAFE in a forked-after-threads child.
\* Modeled as an op that can fire anywhere in the open window when Buggy, marking
\* unsafeOpRan. The fix performs none of these in the child, so it is unreachable.
UnsafeEnvOp ==
    /\ Buggy
    /\ InWindow
    /\ unsafeOpRan' = TRUE
    /\ UNCHANGED << pc, masterClosed, envPrebuilt >>

\* execve with the pre-built (parent) envp: window closes. The fix reaches here
\* having done only async-signal-safe work and with the master already closed.
Exec ==
    /\ pc = DidClose
    /\ pc' = Execed
    /\ UNCHANGED << masterClosed, unsafeOpRan, envPrebuilt >>

Next == Fork \/ Setrlimit \/ Chdir \/ CloseMaster \/ UnsafeEnvOp \/ Exec

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ pc \in BeforeFork..Execed
    /\ masterClosed \in BOOLEAN
    /\ unsafeOpRan \in BOOLEAN
    /\ envPrebuilt \in BOOLEAN

\* No async-signal-UNSAFE op (setenv/malloc/env-lock) ever runs between fork and
\* exec: if exec was reached, the window stayed clean. The pre-fix child ran
\* setenv + heap alloc here -- the fork-after-threads deadlock/abort hazard.
OnlySafeBeforeExec == (pc = Execed) => ~unsafeOpRan

\* The forkpty master fd is closed BEFORE exec, so it never leaks into the
\* spawned shell or any process the shell itself spawns.
MasterClosedBeforeExec == (pc = Execed) => masterClosed

\* The fix's structural guarantee: env/argv/envp are built in the PARENT before
\* forkpty, so as long as the child runs no unsafe op, a usable pre-built envp
\* exists -- exec never needs to allocate or take the env lock in the child.
SafeImpliesEnvPrebuilt == ~unsafeOpRan => envPrebuilt
========================================================================
