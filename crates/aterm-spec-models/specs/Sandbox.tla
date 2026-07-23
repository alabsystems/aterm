----------------------------- MODULE Sandbox -----------------------------
\* aterm process-CONFINEMENT family: the platform sandbox restriction step that
\* drops privileges/capabilities before the shell child is exec'd.
\*
\* Executable model of aterm's sandbox apply path (commit 3d58709 regression):
\* on macOS the restriction step was effectively a no-op -- the set of resource
\* restrictions that were both REQUESTED (policy asked for them) and SUPPORTED
\* (the running OS can enforce them) were silently NOT applied, so the child
\* process ran UNCONFINED. The fix: the apply step must mark every restriction
\* that is requested /\ supported as actually applied to the kernel.
\*
\* We model K restrictions as fixed slots, three functions to BOOLEAN over
\* 1..K (ty's flat-state engine represents these directly, like Evict's `live`):
\*
\*   requested[n] : policy asked for restriction n
\*   supported[n] : the host OS can enforce restriction n
\*   applied[n]   : restriction n was actually pushed to the kernel
\*
\* The host capability surface (requested/supported) is chosen nondeterministi-
\* cally at Init, then a single Apply step runs. Properties worth proving:
\*
\*   - AllSupportedApplied: ONCE apply ran, every restriction that is both
\*     requested AND supported is applied -- no enforceable confinement is
\*     silently skipped (this is exactly what the macOS no-op violated).
\*   - NoPhantomApply: nothing the policy did NOT request /\ the OS does NOT
\*     support ever gets marked applied (apply is precise, not over-broad).
\*   - AppliedSubsetCapable: applied => requested /\ supported (we never claim
\*     to have enforced a restriction the OS cannot actually enforce).

EXTENDS Naturals

CONSTANTS K, Buggy

VARIABLES
    requested,  \* requested[n] = TRUE iff policy asked for restriction n
    supported,  \* supported[n] = TRUE iff the host OS can enforce restriction n
    applied,    \* applied[n]   = TRUE iff restriction n was pushed to the kernel
    done        \* latched: has the sandbox apply step executed yet?

vars == << requested, supported, applied, done >>

\* The host capability surface is fixed nondeterministically before apply runs;
\* applied starts all-FALSE (the process is born unconfined).
Init ==
    /\ requested \in [ 1..K -> BOOLEAN ]
    /\ supported \in [ 1..K -> BOOLEAN ]
    /\ applied = [ n \in 1..K |-> FALSE ]
    /\ done = FALSE

\* The sandbox apply step (runs once, before exec of the child).
\* FIXED (Buggy=FALSE): mark every requested /\ supported restriction applied.
\* BUGGY (Buggy=TRUE):   the macOS no-op -- apply nothing, leave it unconfined.
Apply ==
    /\ ~done
    /\ IF Buggy
       THEN applied' = applied
       ELSE applied' = [ n \in 1..K |-> applied[n] \/ (requested[n] /\ supported[n]) ]
    /\ done' = TRUE
    /\ UNCHANGED << requested, supported >>

Next == Apply

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ requested \in [ 1..K -> BOOLEAN ]
    /\ supported \in [ 1..K -> BOOLEAN ]
    /\ applied \in [ 1..K -> BOOLEAN ]
    /\ done \in BOOLEAN

\* THE confinement guarantee: once apply has run, every restriction the policy
\* requested that the OS can enforce is actually applied. The macOS no-op
\* (Buggy=TRUE) leaves these FALSE while done=TRUE -> violation.
AllSupportedApplied ==
    done => \A n \in 1..K : (requested[n] /\ supported[n]) => applied[n]

\* Apply is precise: it never enforces a restriction that was neither requested
\* nor supportable (no over-broad confinement that could break the shell).
NoPhantomApply ==
    \A n \in 1..K : applied[n] => (requested[n] /\ supported[n])

========================================================================
