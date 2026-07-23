--------------------------- MODULE Transact ----------------------------
\* aterm buffer-kernel CONSISTENCY family (ATERM_DESIGN §4.2 COMPOSE):
\* `transact` -- the atomic, isolated apply-group under optimistic CC.
\*
\* Executable model of aterm-buffer's `transact()` (lib.rs). What the CODE does:
\*
\*   pub fn transact(&mut self, base, body) {
\*       if self.seq() != base { return Conflict; }   \* whole-surface version eq
\*       for e in body { self.apply(e); }             \* under the &mut borrow
\*       Committed(self.seq())
\*   }
\*
\* Validation is EQUALITY of the head seq against `base` (any intervening
\* append -- even an unrelated span op -- conflicts), and the validate+apply
\* run holds the exclusive &mut borrow, so no writer can interleave inside the
\* body. The model is therefore: Commit is ONE atomic step guarded by
\* seq = base; Conflict is a step that changes NOTHING.
\*
\* Two transactions race a free writer. Properties worth proving:
\*   - NoPartialCommit: a transaction's events are ALL of its NOps ops, placed
\*     contiguously at base+1..base+NOps (no interleaved writer), or NONE.
\*   - NoLostUpdate: two transactions that captured the SAME base never both
\*     commit (first-committer-wins; the loser must observe Conflict).
\*
\* Event authorship is a function [1..MaxSeq -> 0..3] (0 = unwritten,
\* 1 = free writer, 1+t = transaction t), NOT a set, for ty's flat-state engine.

EXTENDS Naturals

CONSTANTS MaxSeq, NOps

Txns == 1..2
Author(t) == 1 + t   \* event authorship tag for transaction t (writer = 1)

\* tstate codes: 0 = idle, 1 = open (base captured), 2 = committed, 3 = aborted
IDLE == 0
OPEN == 1
COMMITTED == 2
ABORTED == 3

VARIABLES
    seq,     \* head of the spine: total events ever appended
    author,  \* author[n] = who appended event n (0 = not yet written)
    tstate,  \* tstate[t] = transaction t's lifecycle state
    base     \* base[t] = the head seq transaction t captured at Begin

vars == << seq, author, tstate, base >>

Init ==
    /\ seq = 0
    /\ author = [ n \in 1..MaxSeq |-> 0 ]
    /\ tstate = [ t \in Txns |-> IDLE ]
    /\ base = [ t \in Txns |-> 0 ]

\* The free writer appends one event (a plain `apply` racing the transactions).
Write ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ author' = [ author EXCEPT ![seq + 1] = 1 ]
    /\ UNCHANGED << tstate, base >>

\* Transaction t captures its base: the code's `snapshot().at` / current head.
Begin(t) ==
    /\ tstate[t] = IDLE
    /\ tstate' = [ tstate EXCEPT ![t] = OPEN ]
    /\ base' = [ base EXCEPT ![t] = seq ]
    /\ UNCHANGED << seq, author >>

\* The code path `self.seq() == base`: validation passes and the WHOLE body
\* applies under the exclusive borrow -- one atomic step, NOps contiguous
\* events. (seq + NOps =< MaxSeq is model finitization only.)
CommitOk(t) ==
    /\ tstate[t] = OPEN
    /\ seq = base[t]
    /\ seq + NOps =< MaxSeq
    /\ seq' = seq + NOps
    /\ author' = [ n \in 1..MaxSeq |->
                     IF n > seq /\ n =< seq + NOps THEN Author(t) ELSE author[n] ]
    /\ tstate' = [ tstate EXCEPT ![t] = COMMITTED ]
    /\ UNCHANGED base

\* The code path `self.seq() != base`: Conflict -- NOTHING is applied.
Conflict(t) ==
    /\ tstate[t] = OPEN
    /\ seq # base[t]
    /\ tstate' = [ tstate EXCEPT ![t] = ABORTED ]
    /\ UNCHANGED << seq, author, base >>

Next == Write \/ \E t \in Txns : Begin(t) \/ CommitOk(t) \/ Conflict(t)

Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------
\* INVARIANTS

TypeOK ==
    /\ seq \in 0..MaxSeq
    /\ author \in [ 1..MaxSeq -> 0..3 ]
    /\ tstate \in [ Txns -> IDLE..ABORTED ]
    /\ base \in [ Txns -> 0..MaxSeq ]

\* The spine is gap-free: exactly the events 1..seq have been written.
SpineGapFree == \A n \in 1..MaxSeq : (author[n] # 0) <=> (n =< seq)

\* No partial commit: a non-committed transaction authored NOTHING; a committed
\* transaction authored EXACTLY its NOps events, contiguously at
\* base+1..base+NOps (so no writer interleaved between validation and any op).
NoPartialCommit ==
    \A t \in Txns :
        /\ tstate[t] = COMMITTED =>
               /\ \A k \in 1..NOps : author[base[t] + k] = Author(t)
               /\ \A n \in 1..MaxSeq :
                      author[n] = Author(t) => (n > base[t] /\ n =< base[t] + NOps)
        /\ tstate[t] # COMMITTED =>
               \A n \in 1..MaxSeq : author[n] # Author(t)

\* No lost update: two transactions over the SAME base never both commit --
\* the seq = base equality guard makes the first commit invalidate the second.
NoLostUpdate ==
    ~(/\ tstate[1] = COMMITTED
      /\ tstate[2] = COMMITTED
      /\ base[1] = base[2])
========================================================================
