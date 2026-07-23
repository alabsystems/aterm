--------------------------- MODULE Snapshot ----------------------------
\* aterm buffer-kernel CONSISTENCY family (ATERM_DESIGN §4.2 COMPOSE):
\* snapshot isolation over the event-log spine.
\*
\* Executable model of aterm-buffer's `snapshot()` (lib.rs): the live Surface
\* mutates lines via `apply` (each edit = exactly one log event); `snapshot()`
\* captures the lines AND the head seq N at one instant. Property worth proving
\* (NOT a tautology):
\*
\*   AFTER snapshot S at seq N, later apply()s NEVER change S's view: in every
\*   reachable state, S.view equals the replay of the history PREFIX 1..N
\*   (last-write-wins per line). A snapshot that shared mutable line storage
\*   with the live surface, copied lines before/after the wrong event, or
\*   recorded the wrong N would violate SnapIsPrefix.
\*
\* The log is a pair of functions [1..MaxSeq -> ...] (event n wrote val[n] to
\* line tgt[n]; 0 = not-yet-written sentinel), NOT a set, so ty's flat-state
\* engine represents it directly. `Write` covers both AppendLine and SetLine:
\* in the code each is one log append + one line mutation.
\*
\* SHAPE MATTERS (hard-won ty constraints, verified by seeded-bug mutation):
\* (1) invariants use bounded integer quantifiers ONLY -- a set-comprehension
\*     equality (IF W = {} ...) fails ty's native lowering and a seeded
\*     snapshot-sharing bug then went GREEN under the default engine (the
\*     interpreter, TY_TRUST_CG=0, caught it);
\* (2) actions never assign `x' = IF ... THEN ... ELSE ...` to a scalar --
\*     with function-typed state vars present that construct is miscompiled
\*     and a depth-1 violation of `snapAt =< seq` went GREEN. Formula-level
\*     IF and IF inside function literals are fine (mutation-validated in
\*     Evict.tla/Transact.tla). Keep every action/invariant in this shape.

EXTENDS Naturals

CONSTANTS MaxSeq, MaxLines, MaxVal

VARIABLES
    seq,        \* head of the spine: total events ever appended
    tgt,        \* tgt[n] = the line event n wrote (0 = no such event yet)
    val,        \* val[n] = the value event n wrote (0 = no such event yet)
    lines,      \* the LIVE surface: lines[i] = current value (0 = never set)
    snapTaken,  \* has snapshot S been taken?
    snapAt,     \* S.at  -- the head seq N at capture time
    snapView    \* S's captured lines (the code's `self.clone()`)

vars == << seq, tgt, val, lines, snapTaken, snapAt, snapView >>

Init ==
    /\ seq = 0
    /\ tgt = [ n \in 1..MaxSeq |-> 0 ]
    /\ val = [ n \in 1..MaxSeq |-> 0 ]
    /\ lines = [ i \in 1..MaxLines |-> 0 ]
    /\ snapTaken = FALSE
    /\ snapAt = 0
    /\ snapView = [ i \in 1..MaxLines |-> 0 ]

\* `apply` (lib.rs): ONE edit = ONE log event (seq+1) + the line mutation.
\* The snapshot variables are deliberately untouched -- that is the claim
\* under test, checked against the prefix replay below.
Write(i, v) ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ tgt' = [ tgt EXCEPT ![seq + 1] = i ]
    /\ val' = [ val EXCEPT ![seq + 1] = v ]
    /\ lines' = [ lines EXCEPT ![i] = v ]
    /\ UNCHANGED << snapTaken, snapAt, snapView >>

\* `snapshot()` (lib.rs): Snapshot { at: self.seq(), surface: self.clone() }.
\* Taken once, at ANY reachable point (the exhaustive search covers them all).
TakeSnapshot ==
    /\ ~snapTaken
    /\ snapTaken' = TRUE
    /\ snapAt' = seq
    /\ snapView' = lines
    /\ UNCHANGED << seq, tgt, val, lines >>

Next == TakeSnapshot \/ \E i \in 1..MaxLines, v \in 1..MaxVal : Write(i, v)

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ seq \in 0..MaxSeq
    /\ tgt \in [ 1..MaxSeq -> 0..MaxLines ]
    /\ val \in [ 1..MaxSeq -> 0..MaxVal ]
    /\ lines \in [ 1..MaxLines -> 0..MaxVal ]
    /\ snapTaken \in BOOLEAN
    /\ snapAt \in 0..MaxSeq
    /\ snapView \in [ 1..MaxLines -> 0..MaxVal ]

\* The snapshot point never lies past the head it was captured from.
SnapAtBounded == snapAt =< seq

\* THE property: S.view == replay of the history prefix 1..snapAt, in EVERY
\* reachable state -- i.e. snapView[i] is the value of the LAST event =< snapAt
\* that wrote line i (or the 0 sentinel if no prefix event wrote it). Writes
\* after snapAt must never leak into the snapshot. (Before the snapshot is
\* taken this holds of the empty prefix, so it is stated unconditionally.)
SnapIsPrefix ==
    \A i \in 1..MaxLines :
        \/ (/\ \A n \in 1..MaxSeq : n > snapAt \/ tgt[n] # i
            /\ snapView[i] = 0)
        \/ (\E n \in 1..MaxSeq :
                /\ n =< snapAt
                /\ tgt[n] = i
                /\ snapView[i] = val[n]
                /\ \A m \in 1..MaxSeq : (m =< snapAt /\ tgt[m] = i) => m =< n)
========================================================================
