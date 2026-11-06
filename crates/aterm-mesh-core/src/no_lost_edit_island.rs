// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P3, no lost edit: the decisions of `no_lost_edit.rs` and their laws, proved for every size by
// the kernel on every Trust build. The spec kernels are what no_lost_edit.rs's contract! kernels
// state they compute; the folds are its Gate::of, settle_reach, publish_forgets, Verdict::of,
// land and Takeover::of, one for one; the Worlds of sections 2, 8 and 13 ask the kernels as
// parameters, and `no_lost_edit_binding_island.rs` instantiates them with the compiled Rust.
// Section 12 binds the held-draft World's takesOver to server.rs takes_over. `no_lost_edit::tests`
// recomputes the answer sheets of sections 11 to 13 from the Rust and reads their words here.
clean {
    -- The decisions' scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/no_lost_edit.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body (the E6 import Trust makes of it) equal to the definition
    -- here. They sit outside the namespace so a clause can name them.

    -- spec_url_stays: a server a publish may have reached stays in the hold's reach
    -- unless its URL names one server and that server took the head.
    def spec_url_stays (oneServer tookIt : Bool) : Bool := !oneServer || !tookIt
    -- spec_hold_waits: a head proves nothing by reach of a hold of a higher
    -- sequence, nor where no server is known to have taken it.
    def spec_hold_waits (aboveHead tookAny : Bool) : Bool := aboveHead || !tookAny
    -- spec_answer_refuses: the server stored nothing and refused (NotStored or Refused).
    def spec_answer_refuses (stored refusal : Bool) : Bool := !stored && refusal
    -- spec_pending_keeps: a pending write stays while its set has not landed and no
    -- later set that lands writes its name.
    def spec_pending_keeps (lands laterLandsName : Bool) : Bool := !lands && !laterLandsName
    -- spec_pass_signs: no rotation due, the newest epoch opens, every pending set reads.
    def spec_pass_signs (rotationDue opensNewest pendingRead : Bool) : Bool :=
      !rotationDue && (opensNewest && pendingRead)
    -- spec_write_signs: not done, the log holds one of its bases, and not a take-back.
    def spec_write_signs (done standsOver takeBack : Bool) : Bool := !done && (standsOver && !takeBack)
    -- spec_write_overtaken: not done, and the log holds none of its bases, or a take-back.
    def spec_write_overtaken (done standsOver takeBack : Bool) : Bool := !done && (!standsOver || takeBack)
    -- spec_takes_head (server.rs takes_over): the pin holds, and it was replaced (no
    -- sequence and no parents check), or the envelope is newer, carries the held
    -- chain on, and names the held head among its parents wherever it or the held
    -- vault names parents.
    def spec_takes_head (pinOk replaced notNewer chainOk namesParents heldParented amongParents : Bool) : Bool :=
      pinOk && (replaced || (!notNewer && (chainOk && ((!namesParents && !heldParented) || amongParents))))
    -- spec_holds_version: the version held (its sequence and bytes), under a pin that
    -- holds and was not replaced.
    def spec_holds_version (pinOk replaced notNewer sameVersion : Bool) : Bool :=
      pinOk && (!replaced && (notNewer && sameVersion))
    -- spec_refuses_not_newer: 409, not newer than the version held and not it.
    def spec_refuses_not_newer (pinOk replaced notNewer sameVersion : Bool) : Bool :=
      pinOk && (!replaced && (notNewer && !sameVersion))
    -- spec_refuses_chain: 403, newer but it drops or changes a held link.
    def spec_refuses_chain (pinOk replaced notNewer chainOk : Bool) : Bool :=
      pinOk && (!replaced && (!notNewer && !chainOk))
    -- spec_refuses_parents: 409, newer and carries the chain on, but parents are named
    -- and leave the held head out.
    def spec_refuses_parents (pinOk replaced notNewer chainOk namesParents heldParented amongParents : Bool) : Bool :=
      pinOk && (!replaced && (!notNewer && (chainOk && ((namesParents || heldParented) && !amongParents))))
    -- spec_hold_adopts (adopt_held_draft): the store took the hold's records with no
    -- error, and the head's stamp is complete with them.
    def spec_hold_adopts (ingested completes : Bool) : Bool := ingested && completes
    -- spec_unversioned_goes: records.drafted goes only once it reads and the records
    -- store holds every record in it.
    def spec_unversioned_goes (reads allStored : Bool) : Bool := reads && allStored
    -- spec_drafted_keeps (keep_drafted_until): a version's records are kept only
    -- while its hold is there.
    def spec_drafted_keeps (held : Bool) : Bool := held
    -- spec_spine_settles (forget_landed_draft): a landed version settles the other
    -- holds by its spine only where it drafted no records or kept them.
    def spec_spine_settles (drafted kept : Bool) : Bool := !drafted || kept
    -- spec_draft_refuses (draft_document_diff_with): an edit that writes is refused
    -- where this Mac's waiting writes, or its held drafts, do not read.
    def spec_draft_refuses (writes waitingUnread heldUnread : Bool) : Bool :=
      writes && (waitingUnread || heldUnread)

    namespace AtermMeshNoLostEdit

    open AtermMeshTemporal
    open AtermMeshRevocation

    -- ===========================================================================
    -- 1. The kernels as parameters. Exact K says K computes the spec kernels;
    --    the binding island instantiates K with the compiled Rust kernels.
    -- ===========================================================================

    structure Kernels where
      stays : Bool → Bool → Bool
      waits : Bool → Bool → Bool
      refuses : Bool → Bool → Bool
      keeps : Bool → Bool → Bool
      passSigns : Bool → Bool → Bool → Bool
      signs : Bool → Bool → Bool → Bool
      overtaken : Bool → Bool → Bool → Bool
      draftedKeeps : Bool → Bool
      spineSettles : Bool → Bool → Bool
      draftRefuses : Bool → Bool → Bool → Bool

    def Exact (K : Kernels) : Prop :=
      (∀ a b, K.stays a b = spec_url_stays a b)
        ∧ (∀ a b, K.waits a b = spec_hold_waits a b)
        ∧ (∀ a b, K.refuses a b = spec_answer_refuses a b)
        ∧ (∀ a b, K.keeps a b = spec_pending_keeps a b)
        ∧ (∀ a b c, K.passSigns a b c = spec_pass_signs a b c)
        ∧ (∀ a b c, K.signs a b c = spec_write_signs a b c)
        ∧ (∀ a b c, K.overtaken a b c = spec_write_overtaken a b c)
        ∧ (∀ a, K.draftedKeeps a = spec_drafted_keeps a)
        ∧ (∀ a b, K.spineSettles a b = spec_spine_settles a b)
        ∧ (∀ a b c, K.draftRefuses a b c = spec_draft_refuses a b c)

    def spec : Kernels :=
      Kernels.mk spec_url_stays spec_hold_waits spec_answer_refuses spec_pending_keeps
        spec_pass_signs spec_write_signs spec_write_overtaken spec_drafted_keeps spec_spine_settles
        spec_draft_refuses

    theorem spec_exact : Exact spec :=
      ⟨fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl, fun _ _ => rfl,
       fun _ _ _ => rfl, fun _ _ _ => rfl, fun _ _ _ => rfl, fun _ => rfl, fun _ _ => rfl,
       fun _ _ _ => rfl⟩

    theorem ft {P : Prop} (h : false = true) : P := by cases h

    -- What each exact kernel says, for every argument.
    theorem stays_false (K : Kernels) (hK : Exact K) (o t : Bool) (h : K.stays o t = false) :
        o = true ∧ t = true := by
      rw [hK.1 o t] at h
      revert h
      cases o <;> cases t <;> decide
    theorem waits_false (K : Kernels) (hK : Exact K) (a t : Bool) (h : K.waits a t = false) :
        a = false ∧ t = true := by
      rw [hK.2.1 a t] at h
      revert h
      cases a <;> cases t <;> decide
    theorem refuses_true (K : Kernels) (hK : Exact K) (s r : Bool) (h : K.refuses s r = true) :
        s = false ∧ r = true := by
      rw [hK.2.2.1 s r] at h
      revert h
      cases s <;> cases r <;> decide
    theorem keeps_true (K : Kernels) (hK : Exact K) (l t : Bool) (h : K.keeps l t = true) :
        l = false ∧ t = false := by
      rw [hK.2.2.2.1 l t] at h
      revert h
      cases l <;> cases t <;> decide
    theorem keeps_false (K : Kernels) (hK : Exact K) (l t : Bool) (h : K.keeps l t = false) :
        l = true ∨ t = true := by
      rw [hK.2.2.2.1 l t] at h
      revert h
      cases l <;> cases t <;> decide
    theorem pass_signs_true (K : Kernels) (hK : Exact K) (d o r : Bool) (h : K.passSigns d o r = true) :
        d = false ∧ o = true ∧ r = true := by
      rw [hK.2.2.2.2.1 d o r] at h
      revert h
      cases d <;> cases o <;> cases r <;> decide
    theorem signs_true (K : Kernels) (hK : Exact K) (d s b : Bool) (h : K.signs d s b = true) :
        d = false ∧ s = true ∧ b = false := by
      rw [hK.2.2.2.2.2.1 d s b] at h
      revert h
      cases d <;> cases s <;> cases b <;> decide
    theorem overtaken_true (K : Kernels) (hK : Exact K) (d s b : Bool) (h : K.overtaken d s b = true) :
        d = false := by
      rw [hK.2.2.2.2.2.2.1 d s b] at h
      revert h
      cases d <;> cases s <;> cases b <;> decide
    theorem drafted_keeps_true (K : Kernels) (hK : Exact K) (a : Bool) (h : K.draftedKeeps a = true) :
        a = true := by
      rw [hK.2.2.2.2.2.2.2.1 a] at h
      exact h
    theorem spine_settles_true (K : Kernels) (hK : Exact K) (d k : Bool) (h : K.spineSettles d k = true) :
        d = false ∨ k = true := by
      rw [hK.2.2.2.2.2.2.2.2.1 d k] at h
      revert h
      cases d <;> cases k <;> decide

    -- ===========================================================================
    -- 2. HELD DRAFTS (one-mesh W2): the hold of each version a publish drafted
    --    records for, and what a settled head proves of it. A version is a Nat, a
    --    server is a Nat, a URL is a Nat; every map is a Nat-indexed function or a
    --    List, so nothing is bounded.
    -- ===========================================================================

    -- A hold's reach: the URLs its publish may have sent to, or unknown ("*").
    inductive Reach where
      | unknown : Reach
      | known : List Nat → Reach

    def elem (a : Nat) : List Nat → Bool
      | [] => false
      | b :: bs => if a = b then true else elem a bs

    def keepIf (p : Nat → Bool) : List Nat → List Nat
      | [] => []
      | a :: l => if p a = true then a :: keepIf p l else keepIf p l

    def upd {A : Type} (f : Nat → A) (k : Nat) (a : A) : Nat → A :=
      fun x => if x = k then a else f x

    def upd2 {A : Type} (f : Nat → Nat → A) (k j : Nat) (a : A) : Nat → Nat → A :=
      fun x y => if x = k then (if y = j then a else f x y) else f x y

    -- The step's Bool readers, each a match the proofs open by cases.
    def isTrueOpt : Option Bool → Bool
      | some true => true
      | _ => false
    def isFalseOpt : Option Bool → Bool
      | some false => true
      | _ => false
    def unsent : Option Nat → Bool
      | none => true
      | some _ => false
    def isHeld : Option Reach → Bool
      | none => false
      | some _ => true
    def allL (p : Nat → Bool) : List Nat → Bool
      | [] => true
      | a :: l => p a && allL p l
    def anyL (p : Nat → Bool) : List Nat → Bool
      | [] => false
      | a :: l => p a || anyL p l
    def nonEmpty : List Nat → Bool
      | [] => false
      | _ :: _ => true

    -- Gate.of: 0 the head descends from the hold's version, 1 it proves nothing by
    -- reach (hold_waits), 2 the hold's reach decides.
    def gateOf (K : Kernels) (named : Bool) (heldSeq headSeq : Nat) (tookAny : Bool) : Nat :=
      if named = true then 0 else if K.waits (decide (headSeq < heldSeq)) tookAny = true then 1 else 2

    -- settle_reach: none, the hold goes; unknown stays; else the servers that stay.
    def settleReach (K : Kernels) (one : Nat → Bool) (took : List Nat) : Reach → Option Reach
      | Reach.unknown => some Reach.unknown
      | Reach.known xs =>
        match keepIf (fun x => K.stays (one x) (elem x took)) xs with
        | [] => none
        | a :: l => some (Reach.known (a :: l))

    -- settle_held_drafts_locked (and land_published's settle of a pending set), one
    -- hold: the gate, then the reach. `one` is names_one_server, the code's decision.
    def settleHold (K : Kernels) (one : Nat → Bool) (headSeq : Nat) (parents took : List Nat)
        (u useq : Nat) (r : Reach) : Option Reach :=
      match gateOf K (elem u parents) useq headSeq (nonEmpty took) with
      | 0 => none
      | 1 => some r
      | _ => settleReach K one took r

    -- The publish's classification: every answer refuses (answer_refuses), folded.
    def forgets (K : Kernels) : List (Bool × Bool) → Bool
      | [] => true
      | p :: l => K.refuses p.1 p.2 && forgets K l

    -- takes_over (server.rs), a parented vault; pin checks are pinOk. The server's
    -- compare-and-set is the environment the reach rule rests on.
    def takesOver (same newer named pinOk : Bool) : Bool := same || (newer && named && pinOk)

    structure World where
      seq : Nat → Nat
      spine : Nat → List Nat
      mine : Nat → Bool
      recs : Nat → Bool
      reach : Nat → List Nat
      phase : Nat → Nat
      head : Nat → Nat
      srv : Nat → Nat
      via : Nat → Nat → Option Nat
      inflight : Nat → Nat → Bool
      res : Nat → Nat → Option Bool
      hold : Nat → Option Reach
      kept : Nat → Bool

    inductive Event where
      | draft : Nat → Nat → List Nat → Bool → Bool → List Nat → Event
      | hold : Nat → Bool → Event
      | send : Nat → Nat → Event
      | arrive : Nat → Nat → Bool → Event
      | refusedAll : Nat → Event
      | keep : Nat → Event
      | landed : Nat → List Nat → Event
      | adopt : Nat → Nat → Event
      | republish : Nat → Nat → Event
      | move : Nat → Nat → Event

    def settleOne (K : Kernels) (one : Nat → Bool) (headSeq : Nat) (parents took : List Nat) (u useq : Nat) :
        Option Reach → Option Reach
      | none => none
      | some r => settleHold K one headSeq parents took u useq r

    def settleAll (K : Kernels) (one : Nat → Bool) (w : World) (headSeq : Nat) (parents took : List Nat) :
        Nat → Option Reach :=
      fun u => settleOne K one headSeq parents took u (w.seq u) (w.hold u)

    def accept (w : World) (s v : Nat) (ok : Bool) : Bool :=
      takesOver (decide (w.head s = v)) (decide (w.seq (w.head s) < w.seq v)) (elem (w.head s) (w.spine v)) ok

    -- What a refused publish's answers read: per reach URL, stored and refused.
    def ansList (w : World) (v : Nat) : List Nat → List (Bool × Bool)
      | [] => []
      | x :: l => (isTrueOpt (w.res v x), isFalseOpt (w.res v x)) :: ansList w v l
    def answers (w : World) (v : Nat) : List (Bool × Bool) := ansList w v (w.reach v)

    -- One state-lock section of this Mac and its servers. `one` is
    -- names_one_server (the code's decision); `moves` is which URLs the world may
    -- move between servers (the truth). Events:
    --   draft v o sp mine recs reach   a version drafted from opened o, spine sp
    --   hold v unknown                 hold_drafted, before any PUT (phase 1 to 2)
    --   send v x                       the PUT to URL x leaves (phase 2 only)
    --   arrive v x pinOk               the server reads it; takes_over decides; the
    --                                  reply may be lost (no event)
    --   refusedAll v                   forget_held_draft: every answer refuses
    --   keep v                         keep_drafted_until after some server took it,
    --                                  while its hold is there (drafted_keeps)
    --   landed v took                  forget_landed_draft, its records kept, none,
    --                                  or its hold gone: its hold goes, the others
    --                                  settle with took, and with its spine where
    --                                  it kept its records or drafted none
    --   adopt v s                      adopt_held_draft: server s serves v, the hold
    --                                  completes its stamp; settle by descent only
    --   republish v s                  a mirror, put-back or another Mac's publish:
    --                                  a kept version, or one not this Mac's
    --   move x s                       the ALab name moves to another server
    def stDraft (w : World) (v o : Nat) (sp : List Nat) (m d : Bool) (r : List Nat) : World :=
      if decide (w.phase v = 0) && !(decide (v = 0)) && elem o sp
         && allL (fun p => decide (w.seq p < w.seq o + 1)) sp then
        { w with seq := upd w.seq v (w.seq o + 1), spine := upd w.spine v sp,
                 mine := upd w.mine v m, recs := upd w.recs v d, reach := upd w.reach v r,
                 phase := upd w.phase v 1 }
      else w

    def stHold (w : World) (v : Nat) (unk : Bool) : World :=
      if decide (w.phase v = 1) then
        { w with phase := upd w.phase v 2,
                 hold := if w.mine v && w.recs v
                         then upd w.hold v (some (if unk then Reach.unknown else Reach.known (w.reach v)))
                         else w.hold }
      else w

    def stSend (w : World) (v x : Nat) : World :=
      if decide (w.phase v = 2) && elem x (w.reach v) && unsent (w.via v x) then
        { w with via := upd2 w.via v x (some (w.srv x)), inflight := upd2 w.inflight v x true }
      else w

    def stArrive (w : World) (v x : Nat) (ok : Bool) : World :=
      match w.via v x with
      | none => w
      | some s =>
        if w.inflight v x then
          { w with inflight := upd2 w.inflight v x false,
                   res := upd2 w.res v x (some (accept w s v ok)),
                   head := if accept w s v ok then upd w.head s v else w.head }
        else w

    def stRefused (K : Kernels) (w : World) (v : Nat) : World :=
      if decide (w.phase v = 2) && w.mine v && forgets K (answers w v) then
        { w with hold := upd w.hold v none, phase := upd w.phase v 3 }
      else w

    def stKeep (K : Kernels) (w : World) (v : Nat) : World :=
      if decide (w.phase v = 2) && w.mine v && anyL (fun x => isTrueOpt (w.res v x)) (w.reach v)
         && K.draftedKeeps (isHeld (w.hold v)) then
        { w with kept := upd w.kept v true }
      else w

    -- The spine a landed version settles the other holds by: its own where it
    -- drafted no records or kept them (spine_settles), else none (reach alone).
    def landSpine (K : Kernels) (w : World) (v : Nat) : List Nat :=
      if K.spineSettles (w.recs v) (w.kept v) = true then w.spine v else []

    def stLanded (K : Kernels) (one : Nat → Bool) (w : World) (v : Nat) (tk : List Nat) : World :=
      if decide (w.phase v = 2) && w.mine v && nonEmpty tk
         && allL (fun x => elem x (w.reach v) && isTrueOpt (w.res v x)) tk
         && (!w.recs v || w.kept v || !isHeld (w.hold v)) then
        { w with phase := upd w.phase v 3,
                 hold := settleAll K one { w with hold := upd w.hold v none } (w.seq v) (landSpine K w v) tk }
      else w

    def stAdopt (K : Kernels) (one : Nat → Bool) (w : World) (v s : Nat) : World :=
      if w.mine v && decide (w.head s = v) && isHeld (w.hold v) && !w.kept v then
        { w with kept := upd w.kept v true,
                 hold := settleAll K one { w with hold := upd w.hold v none } (w.seq v) (w.spine v) [] }
      else w

    def stRepublish (w : World) (v s : Nat) : World :=
      if (!w.mine v || w.kept v) && decide (2 ≤ w.phase v) && accept w s v true then
        { w with head := upd w.head s v }
      else w

    def stMove (moves : Nat → Bool) (w : World) (x s : Nat) : World :=
      if moves x then { w with srv := upd w.srv x s } else w

    def step (K : Kernels) (one moves : Nat → Bool) (w : World) : Event → World
      | Event.draft v o sp m d r => stDraft w v o sp m d r
      | Event.hold v unk => stHold w v unk
      | Event.send v x => stSend w v x
      | Event.arrive v x ok => stArrive w v x ok
      | Event.refusedAll v => stRefused K w v
      | Event.keep v => stKeep K w v
      | Event.landed v tk => stLanded K one w v tk
      | Event.adopt v s => stAdopt K one w v s
      | Event.republish v s => stRepublish w v s
      | Event.move x s => stMove moves w x s

    def run (K : Kernels) (one moves : Nat → Bool) (w : World) : List Event → World
      | [] => w
      | e :: es => run K one moves (step K one moves w e) es

    def w0 : World :=
      { seq := fun _ => 0, spine := fun _ => [], mine := fun _ => false, recs := fun _ => false,
        reach := fun _ => [], phase := fun _ => 0, head := fun _ => 0, srv := fun x => x,
        via := fun _ _ => none, inflight := fun _ _ => false, res := fun _ _ => none,
        hold := fun _ => none, kept := fun _ => false }

    def HeldSpec (K : Kernels) (one moves : Nat → Bool) : StateMachine World :=
      StateMachine.mk (fun w => w = w0) (fun w w' => ∃ e, w' = step K one moves w e)

    -- A server serves my drafted version v whose records exist nowhere: no hold, not kept.
    def lostAt (w : World) (v s : Nat) : Bool :=
      w.mine v && w.recs v && w.head s == v && !w.kept v && (w.hold v).isNone

    -- ===========================================================================
    -- 3. A hold goes only on proof, for every reach and every answer.
    -- ===========================================================================

    theorem sn {A : Type} {P : Prop} {a : A} (h : some a = none) : P := by cases h
    theorem cn {P : Prop} {a : Nat} {l : List Nat} (h : a :: l = []) : P := by cases h

    theorem not_true_false {b : Bool} (h : Not (b = true)) : b = false := by
      cases b
      · rfl
      · exact absurd rfl h

    theorem keep_pos (p : Nat → Bool) (a : Nat) (l : List Nat) (hpa : p a = true) :
        keepIf p (a :: l) = a :: keepIf p l :=
      show (if p a = true then a :: keepIf p l else keepIf p l) = a :: keepIf p l from if_pos hpa

    theorem keep_neg (p : Nat → Bool) (a : Nat) (l : List Nat) (hpa : Not (p a = true)) :
        keepIf p (a :: l) = keepIf p l :=
      show (if p a = true then a :: keepIf p l else keepIf p l) = keepIf p l from if_neg hpa

    theorem elem_cons_neg (x a : Nat) (l : List Nat) (h : Not (x = a)) : elem x (a :: l) = elem x l :=
      show (if x = a then true else elem x l) = elem x l from if_neg h

    -- keepIf leaves nothing only when every element fails the test.
    theorem keepIf_nil (p : Nat → Bool) (xs : List Nat) : keepIf p xs = [] →
        (x : Nat) → elem x xs = true → p x = false := by
      induction xs with
      | nil => intro _ x hx; exact ft hx
      | cons a l ih =>
        intro h x hx
        exact (if hpa : p a = true then cn (Eq.trans (keep_pos p a l hpa).symm h)
          else if hxa : x = a then Eq.mpr (congrArg (fun y => p y = false) hxa) (not_true_false hpa)
          else ih (Eq.trans (keep_neg p a l hpa).symm h) x (Eq.trans (elem_cons_neg x a l hxa).symm hx))

    theorem reach_none (K : Kernels) (hK : Exact K) (one : Nat → Bool) (took : List Nat) (r : Reach)
        (h : settleReach K one took r = none) :
        ∃ xs, r = Reach.known xs ∧ ∀ x, elem x xs = true → one x = true ∧ elem x took = true := by
      cases r with
      | unknown => exact sn h
      | known xs =>
        refine ⟨xs, rfl, ?_⟩
        intro x hx
        cases hk : keepIf (fun x => K.stays (one x) (elem x took)) xs with
        | nil => exact stays_false K hK (one x) (elem x took) (keepIf_nil _ xs hk x hx)
        | cons a l =>
          have e : settleReach K one took (Reach.known xs) = some (Reach.known (a :: l)) := by
            show (match keepIf (fun x => K.stays (one x) (elem x took)) xs with
                  | [] => none
                  | a :: l => some (Reach.known (a :: l))) = some (Reach.known (a :: l))
            rw [hk]
          exact sn (Eq.trans e.symm h)

    -- THE SETTLE RULE. For any exact kernels, a hold goes only by descent, or when
    -- its sequence is at most the head's, some server took the head, its reach is
    -- known, and every URL it may have reached names one server that took the head.
    theorem settle_drops_only_on_proof (K : Kernels) (hK : Exact K) (one : Nat → Bool) (hs : Nat)
        (ps tk : List Nat) (u us : Nat) (r : Reach) (h : settleHold K one hs ps tk u us r = none) :
        elem u ps = true ∨ (¬ (hs < us) ∧ tk ≠ [] ∧
          ∃ xs, r = Reach.known xs ∧ ∀ x, elem x xs = true → one x = true ∧ elem x tk = true) := by
      unfold settleHold gateOf at h
      by_cases hd : elem u ps = true
      · exact Or.inl hd
      · right
        rw [if_neg hd] at h
        cases hw : K.waits (decide (hs < us)) (nonEmpty tk) with
        | true => rw [hw] at h; exact sn h
        | false =>
          rw [hw] at h
          have hwf := waits_false K hK _ _ hw
          refine ⟨?_, ?_, reach_none K hK one tk r h⟩
          · intro hlt
            have : decide (hs < us) = true := by simp only [decide_eq_true_eq]; exact hlt
            rw [this] at hwf
            exact Bool.noConfusion hwf.1
          · intro hnil
            rw [hnil] at hwf
            exact Bool.noConfusion hwf.2

    -- ===========================================================================
    -- 4. NO LOST EDIT (held drafts), for every number of versions, servers, URLs
    --    and holds. Past s v: server s will never hold v again, since it holds a
    --    version of v's sequence or later that is not v (takes_over refuses v as
    --    not newer for good).
    -- ===========================================================================

    def Past (w : World) (s v : Nat) : Prop := w.seq v ≤ w.seq (w.head s) ∧ ¬ (w.head s = v)

    -- URL x of v's publish may still bring v to a server.
    def CanBring (one : Nat → Bool) (w : World) (v x : Nat) : Prop :=
      (w.via v x = none ∧ w.phase v = 2 ∧ ¬ (one x = true ∧ Past w (w.srv x) v))
        ∨ (∃ s, w.via v x = some s ∧ w.inflight v x = true ∧ ¬ Past w s v)

    -- v is on a server, or may yet be.
    def Live (one : Nat → Bool) (w : World) (v : Nat) : Prop :=
      (∃ s, w.head s = v) ∨ (∃ x, elem x (w.reach v) = true ∧ CanBring one w v x)

    def Stored (w : World) (h : Nat) : Prop := ∃ x, w.res h x = some true

    -- v's edit rides a stored version of this Mac whose parent spine names v, and
    -- whose records are kept or none.
    def Covered (w : World) (v : Nat) : Prop :=
      ∃ h, elem v (w.spine h) = true ∧ w.mine h = true ∧ (w.kept h = true ∨ w.recs h = false) ∧ Stored w h

    -- NO LOST EDIT: every version of this Mac with records that went out has its
    -- records kept or held, or can never be on a server, or rides a descendant.
    def NoLostEdit (one : Nat → Bool) (w : World) : Prop :=
      ∀ v, 2 ≤ w.phase v → w.mine v = true → w.recs v = true → w.kept v = false →
        w.hold v = none → ¬ Live one w v ∨ Covered w v

    def ViaFixed (one : Nat → Bool) (w : World) : Prop :=
      ∀ v x s, w.via v x = some s → one x = true → s = w.srv x
    def ResWell (w : World) : Prop :=
      ∀ v x, (¬ (w.res v x = none) → ¬ (w.via v x = none) ∧ w.inflight v x = false)
        ∧ (w.inflight v x = true → ¬ (w.via v x = none) ∧ w.res v x = none)
    def Accepted (w : World) : Prop :=
      ∀ v x s, w.res v x = some true → w.via v x = some s →
        w.seq v ≤ w.seq (w.head s) ∧ (w.head s = v ∨ w.seq v < w.seq (w.head s))
    def Provenance (w : World) : Prop :=
      ∀ s v, w.head s = v → ¬ (v = 0) → w.mine v = true → w.kept v = false →
        ∃ x, elem x (w.reach v) = true ∧ w.via v x = some s ∧ w.res v x = some true
    -- A URL a hold of an unkept version lost names one server that will never hold it.
    def HoldReach (one : Nat → Bool) (w : World) : Prop :=
      ∀ v xs, w.hold v = some (Reach.known xs) → w.kept v = false → ∀ x,
        elem x (w.reach v) = true → elem x xs = false → one x = true ∧ Past w (w.srv x) v

    -- The bookkeeping the six rest on: no version 0 is drafted; a PUT, a hold and
    -- a head name a version past its hold (phase 2 or 3); a PUT goes to a URL of
    -- the version's reach.
    def PhaseZero (w : World) : Prop := w.phase 0 = 0
    def ViaPhase (w : World) : Prop :=
      ∀ v x, ¬ (w.via v x = none) → 2 ≤ w.phase v ∧ elem x (w.reach v) = true
    def HoldPhase (w : World) : Prop := ∀ v, ¬ (w.hold v = none) → 2 ≤ w.phase v
    def HeadPhase (w : World) : Prop := ∀ s v, w.head s = v → v = 0 ∨ 2 ≤ w.phase v

    def HeldInv (one : Nat → Bool) (w : World) : Prop :=
      NoLostEdit one w ∧ ViaFixed one w ∧ ResWell w ∧ Accepted w ∧ Provenance w ∧ HoldReach one w
        ∧ PhaseZero w ∧ ViaPhase w ∧ HoldPhase w ∧ HeadPhase w

    -- The environment premise the reach rule rests on: a URL the world may move
    -- between servers (the ALab name) is never one server to names_one_server.
    def OneIsFixed (one moves : Nat → Bool) : Prop := ∀ x, moves x = true → one x = false

    -- ===========================================================================
    -- 5. The descent rule's obligation. A hold goes when a head's parent spine names
    --    its version, because the head was drafted from that version's text, which
    --    carries its edit. The records of a version are not its text: a version
    --    drafted from v's text carries v's edit only if the draft keeps every change
    --    v's text made over the log, or a later save of the person replaces it.
    --    That is an obligation on the draft (draft_document_diff_with), named here
    --    as `carries` and taken as a premise of each state, never assumed: the
    --    theorem says no edit is lost in every state where the spines carry what
    --    they name.
    -- ===========================================================================

    def TextInheritance (carries : Nat → Nat → Prop) (w : World) : Prop :=
      ∀ h v, elem v (w.spine h) = true → carries h v

    -- The edit of version v is safe: its records are kept or held, it can never be
    -- on a server, or a stored version of this Mac that carries it has its records
    -- kept or drafted none.
    def EditSafe (carries : Nat → Nat → Prop) (one : Nat → Bool) (w : World) : Prop :=
      ∀ v, 2 ≤ w.phase v → w.mine v = true → w.recs v = true →
        w.kept v = true ∨ ¬ (w.hold v = none) ∨ ¬ Live one w v ∨
          ∃ h, carries h v ∧ w.mine h = true ∧ (w.kept h = true ∨ w.recs h = false) ∧ Stored w h

    theorem edit_safe_of (carries : Nat → Nat → Prop) (one : Nat → Bool) (w : World)
        (hn : NoLostEdit one w) (ht : TextInheritance carries w) : EditSafe carries one w := by
      intro v hp hm hr
      cases hk : w.kept v with
      | true => exact Or.inl rfl
      | false =>
        right
        cases hh : w.hold v with
        | some r => exact Or.inl (fun e => by cases e)
        | none =>
          right
          cases hn v hp hm hr hk hh with
          | inl hl => exact Or.inl hl
          | inr hc =>
            exact Or.inr (Exists.elim hc (fun h hh2 => ⟨h, ht h v hh2.1, hh2.2.1, hh2.2.2.1, hh2.2.2.2⟩))

    -- ===========================================================================
    -- 6. The proof: HeldInv holds in every state of every run, for any exact
    --    kernels and any names_one_server that never calls a moving URL one server.
    -- ===========================================================================

    theorem hd_andL {a b : Bool} (h : (a && b) = true) : a = true := by
      cases a
      · exact ft h
      · rfl
    theorem hd_andR {a b : Bool} (h : (a && b) = true) : b = true := by
      cases a
      · exact ft h
      · exact h
    theorem hd_or {a b : Bool} (h : (a || b) = true) : a = true ∨ b = true := by
      cases a
      · exact Or.inr h
      · exact Or.inl rfl
    theorem hd_not {a : Bool} (h : (!a) = true) : a = false := by
      cases a
      · rfl
      · exact ft h
    theorem hd_dec {p : Prop} [Decidable p] (h : decide p = true) : p := of_decide_eq_true h
    theorem hd_ndec {p : Prop} [Decidable p] (h : (!(decide p)) = true) : ¬ p := by
      intro hp
      have e : decide p = true := by simp only [decide_eq_true_eq]; exact hp
      rw [e] at h
      exact ft h
    theorem hd_dect {p : Prop} [Decidable p] (hp : p) : decide p = true := by
      simp only [decide_eq_true_eq]; exact hp
    theorem hd_some {a b : Nat} (h : some a = some b) : a = b := by cases h; rfl
    theorem hd_sn {A : Type} {P : Prop} {a : A} (h : some a = none) : P := by cases h
    theorem hd_isTrue {o : Option Bool} (h : isTrueOpt o = true) : o = some true := by
      cases o with
      | none => exact ft h
      | some b => cases b with
        | false => exact ft h
        | true => rfl
    theorem hd_isFalse {o : Option Bool} (h : isFalseOpt o = true) : o = some false := by
      cases o with
      | none => exact ft h
      | some b => cases b with
        | false => rfl
        | true => exact ft h
    theorem hd_unsent {o : Option Nat} (h : unsent o = true) : o = none := by
      cases o with
      | none => rfl
      | some b => exact ft h

    theorem hd_updS {A : Type} (f : Nat → A) (k : Nat) (a : A) : upd f k a k = a := if_pos rfl
    theorem hd_updO {A : Type} (f : Nat → A) (k x : Nat) (a : A) (h : ¬ (x = k)) : upd f k a x = f x := if_neg h
    theorem hd_upd2S {A : Type} (f : Nat → Nat → A) (k j : Nat) (a : A) : upd2 f k j a k j = a := by
      show (if k = k then (if j = j then a else f k j) else f k j) = a
      rw [if_pos rfl, if_pos rfl]
    theorem hd_upd2O {A : Type} (f : Nat → Nat → A) (k j x y : Nat) (a : A) (h : ¬ (x = k ∧ y = j)) :
        upd2 f k j a x y = f x y := by
      show (if x = k then (if y = j then a else f x y) else f x y) = f x y
      by_cases hx : x = k
      · rw [if_pos hx]
        by_cases hy : y = j
        · exact absurd ⟨hx, hy⟩ h
        · rw [if_neg hy]
      · rw [if_neg hx]

    theorem hd_elemCons (x a : Nat) (l : List Nat) (h : elem x (a :: l) = true) : x = a ∨ elem x l = true := by
      by_cases hxa : x = a
      · exact Or.inl hxa
      · exact Or.inr (Eq.trans (elem_cons_neg x a l hxa).symm h)
    theorem hd_allL (p : Nat → Bool) (l : List Nat) (h : allL p l = true) (x : Nat) (hx : elem x l = true) :
        p x = true := by
      induction l with
      | nil => exact ft hx
      | cons a l ih =>
        cases hd_elemCons x a l hx with
        | inl e => rw [e]; exact hd_andL h
        | inr e => exact ih (hd_andR h) e
    theorem hd_nonEmpty (l : List Nat) (h : nonEmpty l = true) : ∃ x, elem x l = true := by
      cases l with
      | nil => exact ft h
      | cons a l => exact ⟨a, if_pos rfl⟩

    -- Nat facts over plain variables (omega reads no applications).
    theorem hd_leLt (a b c : Nat) (h1 : a ≤ b) (h2 : b < c) : a < c := by omega
    theorem hd_leLe (a b c : Nat) (h1 : a ≤ b) (h2 : b ≤ c) : a ≤ c := by omega
    theorem hd_ltLe (a b : Nat) (h : a < b) : a ≤ b := by omega
    theorem hd_notLt (a b : Nat) (h : ¬ (a < b)) : b ≤ a := by omega
    theorem hd_ltIrr (a b : Nat) (h1 : a ≤ b) (h2 : b < a) : False := by omega
    theorem hd_two (n : Nat) (h : n = 0) : ¬ (2 ≤ n) := by omega
    theorem hd_two1 (n : Nat) (h : n = 1) : ¬ (2 ≤ n) := by omega
    theorem hd_le22 : 2 ≤ 2 := by omega
    theorem hd_le23 : 2 ≤ 3 := by omega
    theorem hd_ne02 (n : Nat) (h : 2 ≤ n) : ¬ (n = 0) := fun e => hd_two n e h
    theorem hd_leRefl (a : Nat) : a ≤ a := by omega
    theorem hd_01 : ¬ ((0 : Nat) = 1) := by decide
    theorem hd_02 : ¬ ((0 : Nat) = 2) := by decide
    theorem hd_tf {P : Prop} (h : true = false) : P := by cases h
    theorem hd_sft {P : Prop} (h : some false = some true) : P := by cases h
    theorem hd_someR {a b : Reach} (h : some a = some b) : b = a := by cases h; rfl
    theorem hd_knownInj {a b : List Nat} (h : Reach.known a = Reach.known b) : a = b := by cases h; rfl
    theorem hd_unkKnown {P : Prop} {xs : List Nat} (h : some Reach.unknown = some (Reach.known xs)) : P := by cases h
    theorem hd_heldNe {o : Option Reach} (h : isHeld o = true) : ¬ (o = none) := by
      cases o with
      | none => exact ft h
      | some r => exact fun e => hd_sn e

    theorem hd_phMono (f : Nat → Nat) (v n u : Nat) (hn : 2 ≤ n) (h : 2 ≤ f u) : 2 ≤ upd f v n u := by
      by_cases e : u = v
      · rw [e, hd_updS]; exact hn
      · rw [hd_updO _ _ _ _ e]; exact h
    theorem hd_le2upd (f : Nat → Nat) (v n : Nat) (h : 2 ≤ n) : 2 ≤ upd f v n v := by
      rw [hd_updS]; exact h
    theorem hd_keptMono (f : Nat → Bool) (v h : Nat) (a : f h = true) : upd f v true h = true := by
      by_cases e : h = v
      · rw [e, hd_updS]
      · rw [hd_updO _ _ _ _ e]; exact a

    -- What a server's compare-and-set accepting v says of the head it replaced.
    theorem hd_accept (w : World) (s v : Nat) (ok : Bool) (h : accept w s v ok = true) :
        w.head s = v ∨ w.seq (w.head s) < w.seq v := by
      unfold accept takesOver at h
      cases hd_or h with
      | inl e => exact Or.inl (hd_dec e)
      | inr e => exact Or.inr (hd_dec (hd_andL e))
    theorem hd_acceptPast (w : World) (s v : Nat) (ok : Bool) (h : accept w s v ok = true) : ¬ Past w s v := by
      intro hp
      cases hd_accept w s v ok h with
      | inl e => exact hp.2 e
      | inr e => exact hd_ltIrr _ _ hp.1 e

    -- A head that moves only forward (to v, from a lower sequence) keeps every server past what it was past.
    theorem hd_pastMono (w w' : World) (v : Nat) (es : w'.seq = w.seq)
        (hm : ∀ s, w'.head s = w.head s ∨ (w'.head s = v ∧ w.seq (w.head s) < w.seq v))
        (s u : Nat) (hp : Past w s u) : Past w' s u := by
      show w'.seq u ≤ w'.seq (w'.head s) ∧ ¬ (w'.head s = u)
      cases hm s with
      | inl e => rw [e, es]; exact hp
      | inr e =>
        rw [e.1, es]
        have hlt : w.seq u < w.seq v := hd_leLt _ _ _ hp.1 e.2
        refine ⟨hd_ltLe _ _ hlt, ?_⟩
        intro hvu
        rw [hvu] at hlt
        exact hd_ltIrr _ _ (hd_ltLe _ _ hlt) hlt
    theorem hd_pastEq (w w' : World) (e1 : w'.seq = w.seq) (e2 : w'.head = w.head) (s u : Nat)
        (h : Past w s u) : Past w' s u := by
      show w'.seq u ≤ w'.seq (w'.head s) ∧ ¬ (w'.head s = u)
      rw [e1, e2]
      exact h
    theorem hd_accMono (sq : Nat → Nat) (u h h' v : Nat) (hm : h' = h ∨ (h' = v ∧ sq h < sq v))
        (ha : sq u ≤ sq h ∧ (h = u ∨ sq u < sq h)) : sq u ≤ sq h' ∧ (h' = u ∨ sq u < sq h') := by
      cases hm with
      | inl e => rw [e]; exact ha
      | inr e =>
        rw [e.1]
        have t : sq u < sq v := hd_leLt _ _ _ ha.1 e.2
        exact ⟨hd_ltLe _ _ t, Or.inr t⟩

    theorem hd_canBack (one : Nat → Bool) (w w' : World) (u x : Nat)
        (hv : w'.via u x = w.via u x) (hi : w'.inflight u x = w.inflight u x)
        (hp : w'.phase u = 2 → w.phase u = 2) (hP : ∀ s, Past w s u → Past w' s u)
        (hs : one x = true → w'.srv x = w.srv x) (h : CanBring one w' u x) : CanBring one w u x := by
      cases h with
      | inl h =>
        refine Or.inl ⟨Eq.trans hv.symm h.1, hp h.2.1, ?_⟩
        intro hq
        apply h.2.2
        refine ⟨hq.1, ?_⟩
        rw [hs hq.1]
        exact hP _ hq.2
      | inr h =>
        exact Exists.elim h (fun s hs' => Or.inr ⟨s, Eq.trans hv.symm hs'.1, Eq.trans hi.symm hs'.2.1,
          fun hp' => hs'.2.2 (hP s hp')⟩)
    theorem hd_liveBack (one : Nat → Bool) (w w' : World) (u : Nat)
        (hh : ∀ s, w'.head s = u → w.head s = u) (hr : w'.reach u = w.reach u)
        (hc : ∀ x, CanBring one w' u x → CanBring one w u x) (h : Live one w' u) : Live one w u := by
      cases h with
      | inl h => exact Or.inl (Exists.elim h (fun s e => ⟨s, hh s e⟩))
      | inr h => exact Or.inr (Exists.elim h (fun x e => ⟨x, Eq.trans (congrArg (elem x) hr).symm e.1, hc x e.2⟩))
    theorem hd_covFwd (w w' : World) (u : Nat)
        (hst : ∀ h, Stored w h → w'.spine h = w.spine h ∧ w'.mine h = w.mine h ∧ w'.recs h = w.recs h)
        (hk : ∀ h, w.kept h = true → w'.kept h = true)
        (hres : ∀ h x, w.res h x = some true → w'.res h x = some true) (c : Covered w u) : Covered w' u := by
      exact Exists.elim c (fun h hc => by
        have hf := hst h hc.2.2.2
        refine ⟨h, ?_, ?_, ?_, ?_⟩
        · rw [hf.1]; exact hc.1
        · rw [hf.2.1]; exact hc.2.1
        · cases hc.2.2.1 with
          | inl e => exact Or.inl (hk h e)
          | inr e => exact Or.inr (by rw [hf.2.2]; exact e)
        · exact Exists.elim hc.2.2.2 (fun x e => ⟨x, hres h x e⟩))
    theorem hd_nle (one : Nat → Bool) (w w' : World) (u : Nat) (hn : NoLostEdit one w)
        (h1 : 2 ≤ w'.phase u → 2 ≤ w.phase u) (h2 : w'.mine u = true → w.mine u = true)
        (h3 : w'.recs u = true → w.recs u = true) (h4 : w'.kept u = false → w.kept u = false)
        (h5 : w'.hold u = none → w.hold u = none)
        (hl : Live one w' u → Live one w u) (hc : Covered w u → Covered w' u) :
        2 ≤ w'.phase u → w'.mine u = true → w'.recs u = true → w'.kept u = false → w'.hold u = none →
          ¬ Live one w' u ∨ Covered w' u :=
      fun a b c d e => Or.elim (hn u (h1 a) (h2 b) (h3 c) (h4 d) (h5 e))
        (fun nl => Or.inl (fun l => nl (hl l))) (fun cv => Or.inr (hc cv))

    -- Frames: an invariant whose fields a step leaves alone holds after it.
    theorem hd_fVia (one : Nat → Bool) (w w' : World) (e1 : w'.via = w.via) (e2 : w'.srv = w.srv)
        (h : ViaFixed one w) : ViaFixed one w' := by
      intro v x s a b
      rw [e1] at a
      rw [e2]
      exact h v x s a b
    theorem hd_fRes (w w' : World) (e1 : w'.res = w.res) (e2 : w'.via = w.via) (e3 : w'.inflight = w.inflight)
        (h : ResWell w) : ResWell w' := by
      intro v x
      rw [e1, e2, e3]
      exact h v x
    theorem hd_fAcc (w w' : World) (e1 : w'.res = w.res) (e2 : w'.via = w.via) (e3 : w'.seq = w.seq)
        (e4 : w'.head = w.head) (h : Accepted w) : Accepted w' := by
      intro v x s a b
      rw [e1] at a
      rw [e2] at b
      rw [e3, e4]
      exact h v x s a b
    theorem hd_fProv (w w' : World) (e1 : w'.head = w.head) (e2 : w'.mine = w.mine) (e3 : w'.kept = w.kept)
        (e4 : w'.reach = w.reach) (e5 : w'.via = w.via) (e6 : w'.res = w.res) (h : Provenance w) :
        Provenance w' := by
      intro s v a b c d
      rw [e1] at a
      rw [e2] at c
      rw [e3] at d
      rw [e4, e5, e6]
      exact h s v a b c d
    theorem hd_fHR (one : Nat → Bool) (w w' : World) (e1 : w'.hold = w.hold) (e2 : w'.kept = w.kept)
        (e3 : w'.reach = w.reach) (e4 : w'.srv = w.srv) (e5 : w'.seq = w.seq) (e6 : w'.head = w.head)
        (h : HoldReach one w) : HoldReach one w' := by
      intro v xs a b x c d
      rw [e1] at a
      rw [e2] at b
      rw [e3] at c
      have t := h v xs a b x c d
      refine ⟨t.1, ?_⟩
      rw [e4]
      exact hd_pastEq w w' e5 e6 _ _ t.2
    theorem hd_fZero (w w' : World) (e : w'.phase = w.phase) (h : PhaseZero w) : PhaseZero w' := by
      show w'.phase 0 = 0
      rw [e]; exact h
    theorem hd_fVP (w w' : World) (e1 : w'.via = w.via) (e2 : w'.phase = w.phase) (e3 : w'.reach = w.reach)
        (h : ViaPhase w) : ViaPhase w' := by
      intro v x a
      rw [e1] at a
      rw [e2, e3]
      exact h v x a
    theorem hd_fHP (w w' : World) (e1 : w'.hold = w.hold) (e2 : w'.phase = w.phase) (h : HoldPhase w) :
        HoldPhase w' := by
      intro v a
      rw [e1] at a
      rw [e2]
      exact h v a
    theorem hd_fHd (w w' : World) (e1 : w'.head = w.head) (e2 : w'.phase = w.phase) (h : HeadPhase w) :
        HeadPhase w' := by
      intro s v a
      rw [e1] at a
      rw [e2]
      exact h s v a

    -- A version below phase 2 has no PUT, no answer, no hold and no head.
    theorem hd_noVia (w : World) (hVP : ViaPhase w) (v : Nat) (h : ¬ (2 ≤ w.phase v)) (x : Nat) :
        w.via v x = none := by
      cases e : w.via v x with
      | none => rfl
      | some s => exact absurd (hVP v x (fun n => hd_sn (Eq.trans e.symm n))).1 h
    theorem hd_noRes (w : World) (hR : ResWell w) (v x : Nat) (h : w.via v x = none) : w.res v x = none := by
      cases e : w.res v x with
      | none => rfl
      | some b => exact absurd h ((hR v x).1 (fun n => hd_sn (Eq.trans e.symm n))).1
    theorem hd_noStored (w : World) (hR : ResWell w) (hVP : ViaPhase w) (v : Nat) (h : ¬ (2 ≤ w.phase v)) :
        ¬ Stored w v :=
      fun st => Exists.elim st (fun x e => hd_sn (Eq.trans e.symm (hd_noRes w hR v x (hd_noVia w hVP v h x))))
    theorem hd_noHold (w : World) (hHP : HoldPhase w) (v : Nat) (h : ¬ (2 ≤ w.phase v)) : w.hold v = none := by
      cases e : w.hold v with
      | none => rfl
      | some r => exact absurd (hHP v (fun n => hd_sn (Eq.trans e.symm n))) h
    theorem hd_noHead (w : World) (hHd : HeadPhase w) (v : Nat) (hv : ¬ (v = 0)) (h : ¬ (2 ≤ w.phase v)) (s : Nat) :
        ¬ (w.head s = v) := fun e => Or.elim (hHd s v e) hv h

    -- Every answer of a refused publish read NotStored or Refused.
    theorem hd_forgets (K : Kernels) (hK : Exact K) (w : World) (v : Nat) (xs : List Nat) :
        forgets K (ansList w v xs) = true → ∀ x, elem x xs = true → w.res v x = some false := by
      induction xs with
      | nil => intro _ x hx; exact ft hx
      | cons a l ih =>
        intro h x hx
        have hr := refuses_true K hK _ _ (hd_andL h)
        cases hd_elemCons x a l hx with
        | inl e => rw [e]; exact hd_isFalse hr.2
        | inr e => exact ih (hd_andR h) x e

    theorem hd_keepDrop (p : Nat → Bool) (x : Nat) (xs : List Nat) :
        elem x xs = true → elem x (keepIf p xs) = false → p x = false := by
      induction xs with
      | nil => intro hx _; exact ft hx
      | cons a l ih =>
        intro hx hk
        by_cases hpa : p a = true
        · rw [keep_pos p a l hpa] at hk
          by_cases hxa : x = a
          · have t : elem x (a :: keepIf p l) = true :=
              show (if x = a then true else elem x (keepIf p l)) = true from if_pos hxa
            rw [t] at hk
            exact hd_tf hk
          · rw [elem_cons_neg x a _ hxa] at hk
            exact ih (Eq.trans (elem_cons_neg x a l hxa).symm hx) hk
        · rw [keep_neg p a l hpa] at hk
          by_cases hxa : x = a
          · rw [hxa]; exact not_true_false hpa
          · exact ih (Eq.trans (elem_cons_neg x a l hxa).symm hx) hk

    -- A hold that stays settles to itself, or (gate 2) to a narrower known reach whose
    -- every lost URL names one server that took the head.
    theorem hd_settleSome (K : Kernels) (hK : Exact K) (one : Nat → Bool) (hs : Nat) (ps tk : List Nat)
        (u us : Nat) (r r' : Reach) (h : settleHold K one hs ps tk u us r = some r') :
        r' = r ∨ (¬ (hs < us) ∧ nonEmpty tk = true ∧ ∃ xs ys, r = Reach.known xs ∧ r' = Reach.known ys ∧
          ∀ x, elem x xs = true → elem x ys = false → one x = true ∧ elem x tk = true) := by
      unfold settleHold gateOf at h
      by_cases hd : elem u ps = true
      · rw [if_pos hd] at h
        exact hd_sn h.symm
      · rw [if_neg hd] at h
        cases hw : K.waits (decide (hs < us)) (nonEmpty tk) with
        | true =>
          rw [hw] at h
          exact Or.inl (hd_someR h)
        | false =>
          rw [hw] at h
          have hwf := waits_false K hK _ _ hw
          have nlt : ¬ (hs < us) := fun hlt => by rw [hd_dect hlt] at hwf; exact hd_tf hwf.1
          cases r with
          | unknown => exact Or.inl (hd_someR h)
          | known xs =>
            cases hk : keepIf (fun x => K.stays (one x) (elem x tk)) xs with
            | nil =>
              have e : settleReach K one tk (Reach.known xs) = none := by
                show (match keepIf (fun x => K.stays (one x) (elem x tk)) xs with
                      | [] => none
                      | a :: l => some (Reach.known (a :: l))) = none
                rw [hk]
              exact hd_sn (Eq.trans h.symm e)
            | cons a l =>
              have e : settleReach K one tk (Reach.known xs) = some (Reach.known (a :: l)) := by
                show (match keepIf (fun x => K.stays (one x) (elem x tk)) xs with
                      | [] => none
                      | a :: l => some (Reach.known (a :: l))) = some (Reach.known (a :: l))
                rw [hk]
              refine Or.inr ⟨nlt, hwf.2, xs, a :: l, rfl, hd_someR (Eq.trans e.symm h), ?_⟩
              intro x hx hy
              rw [← hk] at hy
              exact stays_false K hK _ _ (hd_keepDrop _ x xs hx hy)
    theorem hd_settleNil (K : Kernels) (hK : Exact K) (one : Nat → Bool) (hs : Nat) (ps : List Nat)
        (u us : Nat) (r : Reach) (h : settleHold K one hs ps [] u us r = none) : elem u ps = true :=
      Or.elim (settle_drops_only_on_proof K hK one hs ps [] u us r h) (fun d => d) (fun g => absurd rfl g.2.1)
    theorem hd_settleNilSome (K : Kernels) (hK : Exact K) (one : Nat → Bool) (hs : Nat) (ps : List Nat)
        (u us : Nat) (r r' : Reach) (h : settleHold K one hs ps [] u us r = some r') : r' = r :=
      Or.elim (hd_settleSome K hK one hs ps [] u us r r' h) (fun e => e) (fun g => ft g.2.1)

    theorem hd_ite (one : Nat → Bool) (c : Bool) (t e : World) (ht : c = true → HeldInv one t)
        (he : HeldInv one e) : HeldInv one (if c = true then t else e) := by
      cases c
      · exact he
      · exact ht rfl

    -- ---- draft: a version at phase 0 has nothing anything else rests on.
    theorem hd_draftInv (one : Nat → Bool) (w w' : World) (v n : Nat) (sp : List Nat) (m d : Bool) (r : List Nat)
        (hi : HeldInv one w) (hph : w.phase v = 0) (hv0 : ¬ (v = 0))
        (eseq : w'.seq = upd w.seq v n) (espine : w'.spine = upd w.spine v sp)
        (emine : w'.mine = upd w.mine v m) (erecs : w'.recs = upd w.recs v d) (ereach : w'.reach = upd w.reach v r)
        (ephase : w'.phase = upd w.phase v 1) (ehead : w'.head = w.head) (esrv : w'.srv = w.srv)
        (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hn2 : ¬ (2 ≤ w.phase v) := hd_two _ hph
      have nvia := hd_noVia w hVP v hn2
      have nhd := hd_noHead w hHd v hv0 hn2
      have nst := hd_noStored w hR hVP v hn2
      have nhold := hd_noHold w hHP v hn2
      have past : ∀ s u, ¬ (u = v) → Past w s u → Past w' s u := by
        intro s u huv p
        show w'.seq u ≤ w'.seq (w'.head s) ∧ ¬ (w'.head s = u)
        rw [eseq, ehead, hd_updO _ _ _ _ huv, hd_updO _ _ _ _ (nhd s)]
        exact p
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
      · intro u h1 h2 h3 h4 h5
        by_cases huv : u = v
        · rw [ephase, huv, hd_updS] at h1
          exact absurd h1 (hd_two1 1 rfl)
        · refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
          · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [emine, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [erecs, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [ekept] at a; exact a
          · intro a; rw [ehold] at a; exact a
          · refine hd_liveBack one w w' u ?_ ?_ ?_
            · intro s a; rw [ehead] at a; exact a
            · rw [ereach, hd_updO _ _ _ _ huv]
            · intro y c
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => past s u huv p) ?_ c
              · rw [evia]
              · rw [einf]
              · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
              · intro _; rw [esrv]
          · refine hd_covFwd w w' u ?_ ?_ ?_
            · intro h st
              have hh : ¬ (h = v) := fun e => nst (by rw [e] at st; exact st)
              rw [espine, emine, erecs, hd_updO _ _ _ _ hh, hd_updO _ _ _ _ hh, hd_updO _ _ _ _ hh]
              exact ⟨rfl, rfl, rfl⟩
            · intro h a; rw [ekept]; exact a
            · intro h y a; rw [eres]; exact a
      · intro u x s e1 e2
        rw [eres] at e1
        rw [evia] at e2
        have huv : ¬ (u = v) := fun e => by rw [e, nvia x] at e2; exact hd_sn e2.symm
        rw [eseq, ehead, hd_updO _ _ _ _ huv, hd_updO _ _ _ _ (nhd s)]
        exact hA u x s e1 e2
      · intro s u e1 e2 e3 e4
        rw [ehead] at e1
        have huv : ¬ (u = v) := fun e => nhd s (Eq.trans e1 e)
        rw [emine, hd_updO _ _ _ _ huv] at e3
        rw [ekept] at e4
        rw [ereach, evia, eres, hd_updO _ _ _ _ huv]
        exact hP s u e1 e2 e3 e4
      · intro u xs e1 e2 x e3 e4
        rw [ehold] at e1
        have huv : ¬ (u = v) := fun e => by rw [e, nhold] at e1; exact hd_sn e1.symm
        rw [ekept] at e2
        rw [ereach, hd_updO _ _ _ _ huv] at e3
        have t := hH u xs e1 e2 x e3 e4
        exact ⟨t.1, by rw [esrv]; exact past (w.srv x) u huv t.2⟩
      · show w'.phase 0 = 0
        rw [ephase, hd_updO _ _ _ _ (fun e => hv0 e.symm)]
        exact hZ
      · intro u x a
        rw [evia] at a
        have huv : ¬ (u = v) := fun e => a (by rw [e]; exact nvia x)
        rw [ephase, ereach, hd_updO _ _ _ _ huv, hd_updO _ _ _ _ huv]
        exact hVP u x a
      · intro u a
        rw [ehold] at a
        have huv : ¬ (u = v) := fun e => a (by rw [e]; exact nhold)
        rw [ephase, hd_updO _ _ _ _ huv]
        exact hHP u a
      · intro s u a
        rw [ehead] at a
        have huv : ¬ (u = v) := fun e => nhd s (Eq.trans a e)
        rw [ephase, hd_updO _ _ _ _ huv]
        exact hHd s u a

    theorem hd_draft (one : Nat → Bool) (w : World) (v o : Nat) (sp : List Nat) (m d : Bool) (r : List Nat)
        (hi : HeldInv one w) : HeldInv one (stDraft w v o sp m d r) := by
      unfold stDraft
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      exact hd_draftInv one w _ v (w.seq o + 1) sp m d r hi (hd_dec (hd_andL hg)) (hd_ndec (hd_andL (hd_andR hg)))
        rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ---- hold: phase 1 to 2; a version of this Mac with records gets its hold.
    theorem hd_holdInv (one : Nat → Bool) (w w' : World) (v : Nat) (H : Nat → Option Reach)
        (hi : HeldInv one w) (hph : w.phase v = 1)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = upd w.phase v 2) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : w'.hold = H) (ekept : w'.kept = w.kept)
        (hHo : ∀ u, ¬ (u = v) → H u = w.hold u)
        (hHv : w.mine v = true → w.recs v = true → ¬ (H v = none))
        (hHk : ∀ xs, H v = some (Reach.known xs) → xs = w.reach v) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hv0 : ¬ (v = 0) := fun e => by rw [e, hZ] at hph; exact hd_01 hph
      have phm : ∀ u, 2 ≤ w.phase u → 2 ≤ w'.phase u := fun u a => by
        rw [ephase]; exact hd_phMono w.phase v 2 u hd_le22 a
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        hd_fProv w w' ehead emine ekept ereach evia eres hP, ?_, ?_, ?_, ?_, ?_⟩
      · intro u h1 h2 h3 h4 h5
        by_cases huv : u = v
        · rw [huv, emine] at h2
          rw [huv, erecs] at h3
          rw [huv, ehold] at h5
          exact absurd h5 (hHv h2 h3)
        · refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
          · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [emine] at a; exact a
          · intro a; rw [erecs] at a; exact a
          · intro a; rw [ekept] at a; exact a
          · intro a; rw [ehold, hHo u huv] at a; exact a
          · refine hd_liveBack one w w' u ?_ ?_ ?_
            · intro s a; rw [ehead] at a; exact a
            · rw [ereach]
            · intro y c
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => hd_pastEq w w' eseq ehead s u p) ?_ c
              · rw [evia]
              · rw [einf]
              · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
              · intro _; rw [esrv]
          · refine hd_covFwd w w' u ?_ ?_ ?_
            · intro h _; rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩
            · intro h a; rw [ekept]; exact a
            · intro h y a; rw [eres]; exact a
      · intro u xs e1 e2 x e3 e4
        rw [ekept] at e2
        rw [ereach] at e3
        rw [ehold] at e1
        by_cases huv : u = v
        · rw [huv] at e1
          rw [huv] at e3
          rw [hHk xs e1] at e4
          rw [e4] at e3
          exact ft e3
        · rw [hHo u huv] at e1
          have t := hH u xs e1 e2 x e3 e4
          exact ⟨t.1, by rw [esrv]; exact hd_pastEq w w' eseq ehead _ _ t.2⟩
      · show w'.phase 0 = 0
        rw [ephase, hd_updO _ _ _ _ (fun e => hv0 e.symm)]
        exact hZ
      · intro u x a
        rw [evia] at a
        have t := hVP u x a
        rw [ereach]
        exact ⟨phm u t.1, t.2⟩
      · intro u a
        rw [ehold] at a
        by_cases huv : u = v
        · rw [huv, ephase]; exact hd_le2upd w.phase v 2 hd_le22
        · rw [hHo u huv] at a
          exact phm u (hHP u a)
      · intro s u a
        rw [ehead] at a
        cases hHd s u a with
        | inl z => exact Or.inl z
        | inr p => exact Or.inr (phm u p)

    theorem hd_hold (one : Nat → Bool) (w : World) (v : Nat) (unk : Bool) (hi : HeldInv one w) :
        HeldInv one (stHold w v unk) := by
      unfold stHold
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      have hph : w.phase v = 1 := hd_dec hg
      have nhold : w.hold v = none := hd_noHold w hi.2.2.2.2.2.2.2.2.1 v (hd_two1 _ hph)
      by_cases hmr : (w.mine v && w.recs v) = true
      · rw [if_pos hmr]
        exact hd_holdInv one w _ v (upd w.hold v (some (if unk = true then Reach.unknown else Reach.known (w.reach v))))
          hi hph rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl
          (fun u huv => hd_updO _ _ _ _ huv)
          (fun _ _ e => by rw [hd_updS] at e; exact hd_sn e)
          (fun xs e => by
            rw [hd_updS] at e
            cases unk with
            | true => exact hd_unkKnown e
            | false => exact hd_knownInj (hd_someR e))
      · rw [if_neg hmr]
        exact hd_holdInv one w _ v w.hold hi hph rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl
          (fun _ _ => rfl)
          (fun a b => absurd (show (w.mine v && w.recs v) = true by rw [a, b]) hmr)
          (fun xs e => by rw [nhold] at e; exact hd_sn e.symm)

    -- ---- send: the PUT to x leaves; v could bring itself there before, and still can.
    theorem hd_sendInv (one : Nat → Bool) (w w' : World) (v x : Nat) (hi : HeldInv one w)
        (hph : w.phase v = 2) (hx : elem x (w.reach v) = true) (hvn : w.via v x = none)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = upd2 w.via v x (some (w.srv x)))
        (einf : w'.inflight = upd2 w.inflight v x true) (eres : w'.res = w.res)
        (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have resn := hd_noRes w hR v x hvn
      refine ⟨?_, ?_, ?_, ?_, ?_, hd_fHR one w w' ehold ekept ereach esrv eseq ehead hH, hd_fZero w w' ephase hZ, ?_,
        hd_fHP w w' ehold ephase hHP, hd_fHd w w' ehead ephase hHd⟩
      · intro u h1 h2 h3 h4 h5
        refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
        · intro a; rw [ephase] at a; exact a
        · intro a; rw [emine] at a; exact a
        · intro a; rw [erecs] at a; exact a
        · intro a; rw [ekept] at a; exact a
        · intro a; rw [ehold] at a; exact a
        · refine hd_liveBack one w w' u ?_ ?_ ?_
          · intro s a; rw [ehead] at a; exact a
          · rw [ereach]
          · intro y c
            by_cases hp : u = v ∧ y = x
            · rw [hp.1, hp.2] at c
              rw [hp.1, hp.2]
              cases c with
              | inl h =>
                have t := h.1
                rw [evia, hd_upd2S] at t
                exact hd_sn t
              | inr h =>
                exact Exists.elim h (fun s hs => by
                  have t := hs.1
                  rw [evia, hd_upd2S] at t
                  have es := hd_some t
                  exact Or.inl ⟨hvn, hph, fun q => hs.2.2 (by rw [← es]; exact hd_pastEq w w' eseq ehead _ _ q.2)⟩)
            · refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => hd_pastEq w w' eseq ehead s u p) ?_ c
              · rw [evia, hd_upd2O _ _ _ _ _ _ hp]
              · rw [einf, hd_upd2O _ _ _ _ _ _ hp]
              · intro a; rw [ephase] at a; exact a
              · intro _; rw [esrv]
        · refine hd_covFwd w w' u ?_ ?_ ?_
          · intro h _; rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩
          · intro h a; rw [ekept]; exact a
          · intro h y a; rw [eres]; exact a
      · intro u y s e1 e2
        rw [esrv]
        rw [evia] at e1
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hd_upd2S] at e1
          rw [hp.2]
          exact (hd_some e1).symm
        · rw [hd_upd2O _ _ _ _ _ _ hp] at e1
          exact hV u y s e1 e2
      · intro u y
        rw [eres, evia, einf]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hd_upd2S, hd_upd2S, resn]
          exact ⟨fun n => absurd rfl n, fun _ => ⟨fun t => hd_sn t, rfl⟩⟩
        · rw [hd_upd2O _ _ _ _ _ _ hp, hd_upd2O _ _ _ _ _ _ hp]
          exact hR u y
      · intro u y s e1 e2
        rw [eres] at e1
        rw [evia] at e2
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, resn] at e1
          exact hd_sn e1.symm
        · rw [hd_upd2O _ _ _ _ _ _ hp] at e2
          rw [eseq, ehead]
          exact hA u y s e1 e2
      · intro s u e1 e2 e3 e4
        rw [ehead] at e1
        rw [emine] at e3
        rw [ekept] at e4
        exact Exists.elim (hP s u e1 e2 e3 e4) (fun y hy => by
          have hp : ¬ (u = v ∧ y = x) := fun hp => by
            have t := hy.2.1
            rw [hp.1, hp.2, hvn] at t
            exact hd_sn t.symm
          refine ⟨y, ?_, ?_, ?_⟩
          · rw [ereach]; exact hy.1
          · rw [evia, hd_upd2O _ _ _ _ _ _ hp]; exact hy.2.1
          · rw [eres]; exact hy.2.2)
      · intro u y a
        rw [evia] at a
        rw [ephase, ereach]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hph]
          exact ⟨hd_le22, hx⟩
        · rw [hd_upd2O _ _ _ _ _ _ hp] at a
          exact hVP u y a

    theorem hd_send (one : Nat → Bool) (w : World) (v x : Nat) (hi : HeldInv one w) :
        HeldInv one (stSend w v x) := by
      unfold stSend
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      exact hd_sendInv one w _ v x hi (hd_dec (hd_andL hg)) (hd_andL (hd_andR hg)) (hd_unsent (hd_andR (hd_andR hg)))
        rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ---- arrive, taken: the server's head moves forward to v; v was live there, so covered.
    theorem hd_arriveT (one : Nat → Bool) (w w' : World) (v x s0 : Nat) (hi : HeldInv one w)
        (hvx : w.via v x = some s0) (hinf : w.inflight v x = true)
        (hacc : w.head s0 = v ∨ w.seq (w.head s0) < w.seq v) (nP : ¬ Past w s0 v)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = upd w.head s0 v)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = upd2 w.inflight v x false)
        (eres : w'.res = upd2 w.res v x (some true)) (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) :
        HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hvx' : ¬ (w.via v x = none) := fun e => hd_sn (Eq.trans hvx.symm e)
      have resn : w.res v x = none := ((hR v x).2 hinf).2
      have vp := hVP v x hvx'
      have hm : ∀ s, w'.head s = w.head s ∨ (w'.head s = v ∧ w.seq (w.head s) < w.seq v) := by
        intro s
        rw [ehead]
        by_cases hs : s = s0
        · rw [hs, hd_updS]
          cases hacc with
          | inl e => exact Or.inl e.symm
          | inr e => exact Or.inr ⟨rfl, e⟩
        · rw [hd_updO _ _ _ _ hs]
          exact Or.inl rfl
      have pm := hd_pastMono w w' v eseq hm
      have resm : ∀ h y, w.res h y = some true → w'.res h y = some true := by
        intro h y a
        have hp : ¬ (h = v ∧ y = x) := fun hp => by rw [hp.1, hp.2, resn] at a; exact hd_sn a.symm
        rw [eres, hd_upd2O _ _ _ _ _ _ hp]; exact a
      have cov : ∀ u, Covered w u → Covered w' u := fun u c => hd_covFwd w w' u
        (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩) (fun h a => by rw [ekept]; exact a) resm c
      refine ⟨?_, hd_fVia one w w' evia esrv hV, ?_, ?_, ?_, ?_, hd_fZero w w' ephase hZ,
        hd_fVP w w' evia ephase ereach hVP, hd_fHP w w' ehold ephase hHP, ?_⟩
      · intro u h1 h2 h3 h4 h5
        rw [ephase] at h1
        rw [emine] at h2
        rw [erecs] at h3
        rw [ekept] at h4
        rw [ehold] at h5
        by_cases huv : u = v
        · rw [huv] at h1 h2 h3 h4 h5
          rw [huv]
          have live : Live one w v := Or.inr ⟨x, vp.2, Or.inr ⟨s0, hvx, hinf, nP⟩⟩
          cases hN v h1 h2 h3 h4 h5 with
          | inl nl => exact absurd live nl
          | inr c => exact Or.inr (cov v c)
        · cases hN u h1 h2 h3 h4 h5 with
          | inl nl =>
            refine Or.inl (fun l => nl (hd_liveBack one w w' u ?_ ?_ ?_ l))
            · intro s a
              rw [ehead] at a
              by_cases hs : s = s0
              · rw [hs, hd_updS] at a; exact absurd a.symm huv
              · rw [hd_updO _ _ _ _ hs] at a; exact a
            · rw [ereach]
            · intro y c
              have hp : ¬ (u = v ∧ y = x) := fun hp => huv hp.1
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => pm s u p) ?_ c
              · rw [evia]
              · rw [einf, hd_upd2O _ _ _ _ _ _ hp]
              · intro a; rw [ephase] at a; exact a
              · intro _; rw [esrv]
          | inr c => exact Or.inr (cov u c)
      · intro u y
        rw [eres, evia, einf]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hd_upd2S, hd_upd2S]
          exact ⟨fun _ => ⟨hvx', rfl⟩, fun t => ft t⟩
        · rw [hd_upd2O _ _ _ _ _ _ hp, hd_upd2O _ _ _ _ _ _ hp]
          exact hR u y
      · intro u y s e1 e2
        rw [evia] at e2
        rw [eseq]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hvx] at e2
          have es := hd_some e2
          rw [hp.1, ehead, ← es, hd_updS]
          exact ⟨hd_leRefl _, Or.inl rfl⟩
        · rw [eres, hd_upd2O _ _ _ _ _ _ hp] at e1
          exact hd_accMono w.seq u (w.head s) (w'.head s) v (hm s) (hA u y s e1 e2)
      · intro s u e1 e2 e3 e4
        rw [emine] at e3
        rw [ekept] at e4
        rw [ereach, evia]
        rw [ehead] at e1
        by_cases hs : s = s0
        · rw [hs, hd_updS] at e1
          rw [hs, ← e1]
          exact ⟨x, vp.2, hvx, by rw [eres, hd_upd2S]⟩
        · rw [hd_updO _ _ _ _ hs] at e1
          exact Exists.elim (hP s u e1 e2 e3 e4) (fun y hy => ⟨y, hy.1, hy.2.1, resm u y hy.2.2⟩)
      · intro u xs e1 e2 x' e3 e4
        rw [ehold] at e1
        rw [ekept] at e2
        rw [ereach] at e3
        have t := hH u xs e1 e2 x' e3 e4
        exact ⟨t.1, by rw [esrv]; exact pm _ _ t.2⟩
      · intro s u a
        rw [ehead] at a
        rw [ephase]
        by_cases hs : s = s0
        · rw [hs, hd_updS] at a
          rw [← a]
          exact Or.inr vp.1
        · rw [hd_updO _ _ _ _ hs] at a
          exact hHd s u a

    -- ---- arrive, refused: the answer is recorded; nothing moves.
    theorem hd_arriveF (one : Nat → Bool) (w w' : World) (v x s0 : Nat) (hi : HeldInv one w)
        (hvx : w.via v x = some s0) (hinf : w.inflight v x = true)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = upd2 w.inflight v x false)
        (eres : w'.res = upd2 w.res v x (some false)) (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) :
        HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hvx' : ¬ (w.via v x = none) := fun e => hd_sn (Eq.trans hvx.symm e)
      have resn : w.res v x = none := ((hR v x).2 hinf).2
      have resm : ∀ h y, w.res h y = some true → w'.res h y = some true := by
        intro h y a
        have hp : ¬ (h = v ∧ y = x) := fun hp => by rw [hp.1, hp.2, resn] at a; exact hd_sn a.symm
        rw [eres, hd_upd2O _ _ _ _ _ _ hp]; exact a
      refine ⟨?_, hd_fVia one w w' evia esrv hV, ?_, ?_, ?_, hd_fHR one w w' ehold ekept ereach esrv eseq ehead hH,
        hd_fZero w w' ephase hZ, hd_fVP w w' evia ephase ereach hVP, hd_fHP w w' ehold ephase hHP,
        hd_fHd w w' ehead ephase hHd⟩
      · intro u h1 h2 h3 h4 h5
        refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
        · intro a; rw [ephase] at a; exact a
        · intro a; rw [emine] at a; exact a
        · intro a; rw [erecs] at a; exact a
        · intro a; rw [ekept] at a; exact a
        · intro a; rw [ehold] at a; exact a
        · refine hd_liveBack one w w' u ?_ ?_ ?_
          · intro s a; rw [ehead] at a; exact a
          · rw [ereach]
          · intro y c
            by_cases hp : u = v ∧ y = x
            · rw [hp.1, hp.2] at c
              cases c with
              | inl h =>
                have t := h.1
                rw [evia, hvx] at t
                exact hd_sn t
              | inr h =>
                exact Exists.elim h (fun s hs => by
                  have t := hs.2.1
                  rw [einf, hd_upd2S] at t
                  exact ft t)
            · refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => hd_pastEq w w' eseq ehead s u p) ?_ c
              · rw [evia]
              · rw [einf, hd_upd2O _ _ _ _ _ _ hp]
              · intro a; rw [ephase] at a; exact a
              · intro _; rw [esrv]
        · exact hd_covFwd w w' u (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
            (fun h a => by rw [ekept]; exact a) resm
      · intro u y
        rw [eres, evia, einf]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hd_upd2S, hd_upd2S]
          exact ⟨fun _ => ⟨hvx', rfl⟩, fun t => ft t⟩
        · rw [hd_upd2O _ _ _ _ _ _ hp, hd_upd2O _ _ _ _ _ _ hp]
          exact hR u y
      · intro u y s e1 e2
        rw [eres] at e1
        rw [evia] at e2
        rw [eseq, ehead]
        by_cases hp : u = v ∧ y = x
        · rw [hp.1, hp.2, hd_upd2S] at e1
          exact hd_sft e1
        · rw [hd_upd2O _ _ _ _ _ _ hp] at e1
          exact hA u y s e1 e2
      · intro s u e1 e2 e3 e4
        rw [ehead] at e1
        rw [emine] at e3
        rw [ekept] at e4
        rw [ereach, evia]
        exact Exists.elim (hP s u e1 e2 e3 e4) (fun y hy => ⟨y, hy.1, hy.2.1, resm u y hy.2.2⟩)

    theorem hd_arrive (one : Nat → Bool) (w : World) (v x : Nat) (ok : Bool) (hi : HeldInv one w) :
        HeldInv one (stArrive w v x ok) := by
      unfold stArrive
      cases hvx : w.via v x with
      | none => exact hi
      | some s0 =>
        refine hd_ite one _ _ _ ?_ hi
        intro hinf
        by_cases ha : accept w s0 v ok = true
        · exact hd_arriveT one w _ v x s0 hi hvx hinf (hd_accept w s0 v ok ha) (hd_acceptPast w s0 v ok ha)
            rfl rfl rfl rfl rfl rfl
            (by show (if accept w s0 v ok = true then upd w.head s0 v else w.head) = upd w.head s0 v; rw [if_pos ha])
            rfl rfl rfl
            (by show upd2 w.res v x (some (accept w s0 v ok)) = upd2 w.res v x (some true); rw [ha])
            rfl rfl
        · have ha' := not_true_false ha
          exact hd_arriveF one w _ v x s0 hi hvx hinf rfl rfl rfl rfl rfl rfl
            (by show (if accept w s0 v ok = true then upd w.head s0 v else w.head) = w.head; rw [if_neg ha])
            rfl rfl rfl
            (by show upd2 w.res v x (some (accept w s0 v ok)) = upd2 w.res v x (some false); rw [ha'])
            rfl rfl

    -- ---- refusedAll: every answer refused, so v is on no server and can reach none.
    theorem hd_refusedInv (one : Nat → Bool) (w w' : World) (v : Nat) (hi : HeldInv one w)
        (hph : w.phase v = 2) (hm : w.mine v = true)
        (hall : ∀ x, elem x (w.reach v) = true → w.res v x = some false)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = upd w.phase v 3) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : w'.hold = upd w.hold v none) (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hv0 : ¬ (v = 0) := fun e => by rw [e, hZ] at hph; exact hd_02 hph
      have phm : ∀ u, 2 ≤ w.phase u → 2 ≤ w'.phase u := fun u a => by
        rw [ephase]; exact hd_phMono w.phase v 3 u hd_le23 a
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        hd_fProv w w' ehead emine ekept ereach evia eres hP, ?_, ?_, ?_, ?_, ?_⟩
      · intro u h1 h2 h3 h4 h5
        by_cases huv : u = v
        · rw [huv, emine] at h2
          rw [huv, ekept] at h4
          rw [huv]
          refine Or.inl (fun l => ?_)
          cases l with
          | inl h =>
            exact Exists.elim h (fun s e => by
              rw [ehead] at e
              exact Exists.elim (hP s v e hv0 h2 h4) (fun x hx => by
                have t := hx.2.2
                rw [hall x hx.1] at t
                exact hd_sft t))
          | inr h =>
            exact Exists.elim h (fun x hx => by
              have ex := hx.1
              rw [ereach] at ex
              have rf := hall x ex
              have rn : ¬ (w.res v x = none) := fun t => by rw [rf] at t; exact hd_sn t
              have vi := (hR v x).1 rn
              cases hx.2 with
              | inl c =>
                have t := c.1
                rw [evia] at t
                exact vi.1 t
              | inr c =>
                exact Exists.elim c (fun s cs => by
                  have t := cs.2.1
                  rw [einf, vi.2] at t
                  exact ft t))
        · refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
          · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [emine] at a; exact a
          · intro a; rw [erecs] at a; exact a
          · intro a; rw [ekept] at a; exact a
          · intro a; rw [ehold, hd_updO _ _ _ _ huv] at a; exact a
          · refine hd_liveBack one w w' u ?_ ?_ ?_
            · intro s a; rw [ehead] at a; exact a
            · rw [ereach]
            · intro y c
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => hd_pastEq w w' eseq ehead s u p) ?_ c
              · rw [evia]
              · rw [einf]
              · intro a; rw [ephase, hd_updO _ _ _ _ huv] at a; exact a
              · intro _; rw [esrv]
          · exact hd_covFwd w w' u (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
              (fun h a => by rw [ekept]; exact a) (fun h y a => by rw [eres]; exact a)
      · intro u xs e1 e2 x e3 e4
        rw [ehold] at e1
        by_cases huv : u = v
        · rw [huv, hd_updS] at e1
          exact hd_sn e1.symm
        · rw [hd_updO _ _ _ _ huv] at e1
          rw [ekept] at e2
          rw [ereach] at e3
          have t := hH u xs e1 e2 x e3 e4
          exact ⟨t.1, by rw [esrv]; exact hd_pastEq w w' eseq ehead _ _ t.2⟩
      · show w'.phase 0 = 0
        rw [ephase, hd_updO _ _ _ _ (fun e => hv0 e.symm)]
        exact hZ
      · intro u x a
        rw [evia] at a
        have t := hVP u x a
        rw [ereach]
        exact ⟨phm u t.1, t.2⟩
      · intro u a
        rw [ehold] at a
        by_cases huv : u = v
        · rw [huv, hd_updS] at a
          exact absurd rfl a
        · rw [hd_updO _ _ _ _ huv] at a
          exact phm u (hHP u a)
      · intro s u a
        rw [ehead] at a
        cases hHd s u a with
        | inl z => exact Or.inl z
        | inr p => exact Or.inr (phm u p)

    theorem hd_refused (K : Kernels) (hK : Exact K) (one : Nat → Bool) (w : World) (v : Nat) (hi : HeldInv one w) :
        HeldInv one (stRefused K w v) := by
      unfold stRefused
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      exact hd_refusedInv one w _ v hi (hd_dec (hd_andL hg)) (hd_andL (hd_andR hg))
        (hd_forgets K hK w v (w.reach v) (hd_andR (hd_andR hg))) rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ---- keep: the records are kept; kept only rises.
    theorem hd_keepInv (one : Nat → Bool) (w w' : World) (v : Nat) (hi : HeldInv one w)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : w'.hold = w.hold) (ekept : w'.kept = upd w.kept v true) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        ?_, ?_, hd_fZero w w' ephase hZ, hd_fVP w w' evia ephase ereach hVP, hd_fHP w w' ehold ephase hHP,
        hd_fHd w w' ehead ephase hHd⟩
      · intro u h1 h2 h3 h4 h5
        by_cases huv : u = v
        · rw [huv, ekept, hd_updS] at h4
          exact hd_tf h4
        · refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
          · intro a; rw [ephase] at a; exact a
          · intro a; rw [emine] at a; exact a
          · intro a; rw [erecs] at a; exact a
          · intro a; rw [ekept, hd_updO _ _ _ _ huv] at a; exact a
          · intro a; rw [ehold] at a; exact a
          · refine hd_liveBack one w w' u ?_ ?_ ?_
            · intro s a; rw [ehead] at a; exact a
            · rw [ereach]
            · intro y c
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => hd_pastEq w w' eseq ehead s u p) ?_ c
              · rw [evia]
              · rw [einf]
              · intro a; rw [ephase] at a; exact a
              · intro _; rw [esrv]
          · exact hd_covFwd w w' u (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
              (fun h a => by rw [ekept]; exact hd_keptMono w.kept v h a) (fun h y a => by rw [eres]; exact a)
      · intro s u e1 e2 e3 e4
        rw [ekept] at e4
        by_cases huv : u = v
        · rw [huv, hd_updS] at e4
          exact hd_tf e4
        · rw [hd_updO _ _ _ _ huv] at e4
          rw [ehead] at e1
          rw [emine] at e3
          rw [ereach, evia, eres]
          exact hP s u e1 e2 e3 e4
      · intro u xs e1 e2 x e3 e4
        rw [ekept] at e2
        by_cases huv : u = v
        · rw [huv, hd_updS] at e2
          exact hd_tf e2
        · rw [hd_updO _ _ _ _ huv] at e2
          rw [ehold] at e1
          rw [ereach] at e3
          have t := hH u xs e1 e2 x e3 e4
          exact ⟨t.1, by rw [esrv]; exact hd_pastEq w w' eseq ehead _ _ t.2⟩

    theorem hd_keep (K : Kernels) (one : Nat → Bool) (w : World) (v : Nat) (hi : HeldInv one w) :
        HeldInv one (stKeep K w v) := by
      unfold stKeep
      refine hd_ite one _ _ _ ?_ hi
      intro _
      exact hd_keepInv one w _ v hi rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ---- landed: v's hold goes (its records are kept or none); each other hold
    --      settles by descent (covered by v) or by reach (on no server, never again).
    theorem hd_landedInv (K : Kernels) (hK : Exact K) (one : Nat → Bool) (w w' : World) (v : Nat) (tk : List Nat)
        (hi : HeldInv one w) (hph : w.phase v = 2) (hm : w.mine v = true) (hne : nonEmpty tk = true)
        (htk : ∀ x, elem x tk = true → elem x (w.reach v) = true ∧ w.res v x = some true)
        (hrk : w.recs v = true → w.kept v = true ∨ w.hold v = none) (sp : List Nat)
        (hsp : ∀ u, elem u sp = true → elem u (w.spine v) = true ∧ (w.kept v = true ∨ w.recs v = false))
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = upd w.phase v 3) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : ∀ u, w'.hold u = settleOne K one (w.seq v) sp tk u (w.seq u) (upd w.hold v none u))
        (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hv0 : ¬ (v = 0) := fun e => by rw [e, hZ] at hph; exact hd_02 hph
      have stv : Stored w v := Exists.elim (hd_nonEmpty tk hne) (fun x hx => ⟨x, (htk x hx).2⟩)
      have phm : ∀ u, 2 ≤ w.phase u → 2 ≤ w'.phase u := fun u a => by
        rw [ephase]; exact hd_phMono w.phase v 3 u hd_le23 a
      -- A one-server URL that took v is past every other version of v's sequence or lower.
      have past : ∀ u x, ¬ (u = v) → ¬ (w.seq v < w.seq u) → one x = true → elem x tk = true →
          Past w (w.srv x) u := by
        intro u x huv hlt ho hx
        have rx := (htk x hx).2
        cases evx : w.via v x with
        | none => exact absurd evx ((hR v x).1 (fun t => by rw [rx] at t; exact hd_sn t)).1
        | some s =>
          have es := hV v x s evx ho
          rw [← es]
          have a := hA v x s rx evx
          refine ⟨hd_leLe _ _ _ (hd_notLt _ _ hlt) a.1, ?_⟩
          intro hsu
          cases a.2 with
          | inl e => exact huv (Eq.trans hsu.symm e)
          | inr e => rw [hsu] at e; exact hlt e
      -- So a hold dropped by reach leaves its version on no server, unable to reach one.
      have gone : ∀ u xs, ¬ (u = v) → w.hold u = some (Reach.known xs) → w.kept u = false →
          ¬ (w.seq v < w.seq u) → (∀ x, elem x xs = true → one x = true ∧ elem x tk = true) →
          ∀ x, elem x (w.reach u) = true → one x = true ∧ Past w (w.srv x) u := by
        intro u xs huv eh ek hlt hxs x hx
        cases ex : elem x xs with
        | false => exact hH u xs eh ek x hx ex
        | true =>
          have t := hxs x ex
          exact ⟨t.1, past u x huv hlt t.1 t.2⟩
      have notLive : ∀ u, w.mine u = true → w.kept u = false → ¬ (w.hold u = none) →
          (∀ x, elem x (w.reach u) = true → one x = true ∧ Past w (w.srv x) u) → ¬ Live one w u := by
        intro u em ek ehn all l
        have h2 := hHP u ehn
        have u0 : ¬ (u = 0) := fun e => by rw [e, hZ] at h2; exact hd_two 0 rfl h2
        cases l with
        | inl h =>
          exact Exists.elim h (fun s e => Exists.elim (hP s u e u0 em ek) (fun y hy => by
            have t := all y hy.1
            have es := hV u y s hy.2.1 t.1
            rw [← es] at t
            exact t.2.2 e))
        | inr h =>
          exact Exists.elim h (fun y hy => by
            have t := all y hy.1
            cases hy.2 with
            | inl c => exact c.2.2 t
            | inr c =>
              exact Exists.elim c (fun s cs => by
                have es := hV u y s cs.1 t.1
                rw [← es] at t
                exact cs.2.2 t.2))
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        hd_fProv w w' ehead emine ekept ereach evia eres hP, ?_, ?_, ?_, ?_, ?_⟩
      · intro u h1 h2 h3 h4 h5
        rw [emine] at h2
        rw [erecs] at h3
        rw [ekept] at h4
        rw [ehold u] at h5
        by_cases huv : u = v
        · -- v itself: its records were kept (no case here), or its hold had gone.
          rw [huv] at h2 h3 h4
          rw [huv]
          have hvn : w.hold v = none := Or.elim (hrk h3) (fun k => hd_tf (Eq.trans k.symm h4)) (fun n => n)
          have lb : Live one w' v → Live one w v := hd_liveBack one w w' v (fun s a => by rw [ehead] at a; exact a)
            (by rw [ereach])
            (fun y c => hd_canBack one w w' v y (by rw [evia]) (by rw [einf]) (fun _ => hph)
              (fun s p => hd_pastEq w w' eseq ehead s v p) (fun _ => by rw [esrv]) c)
          have cf : Covered w v → Covered w' v := hd_covFwd w w' v
            (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
            (fun h a => by rw [ekept]; exact a) (fun h y a => by rw [eres]; exact a)
          cases hN v (show 2 ≤ w.phase v by rw [hph]; exact hd_le22) h2 h3 h4 hvn with
          | inl nl => exact Or.inl (fun l => nl (lb l))
          | inr c => exact Or.inr (cf c)
        · rw [ephase, hd_updO _ _ _ _ huv] at h1
          rw [hd_updO _ _ _ _ huv] at h5
          have lb : Live one w' u → Live one w u := hd_liveBack one w w' u (fun s a => by rw [ehead] at a; exact a)
            (by rw [ereach])
            (fun y c => hd_canBack one w w' u y (by rw [evia]) (by rw [einf])
              (fun a => by rw [ephase, hd_updO _ _ _ _ huv] at a; exact a)
              (fun s p => hd_pastEq w w' eseq ehead s u p) (fun _ => by rw [esrv]) c)
          have cf : Covered w u → Covered w' u := hd_covFwd w w' u
            (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
            (fun h a => by rw [ekept]; exact a) (fun h y a => by rw [eres]; exact a)
          cases eh : w.hold u with
          | none =>
            cases hN u h1 h2 h3 h4 eh with
            | inl nl => exact Or.inl (fun l => nl (lb l))
            | inr c => exact Or.inr (cf c)
          | some r =>
            rw [eh] at h5
            have h5' : settleHold K one (w.seq v) sp tk u (w.seq u) r = none := h5
            cases settle_drops_only_on_proof K hK one (w.seq v) sp tk u (w.seq u) r h5' with
            | inl d =>
              have ds := hsp u d
              refine Or.inr ⟨v, ?_, ?_, ?_, ?_⟩
              · rw [espine]; exact ds.1
              · rw [emine]; exact hm
              · rw [ekept, erecs]; exact ds.2
              · exact Exists.elim stv (fun x e => ⟨x, by rw [eres]; exact e⟩)
            | inr g =>
              exact Exists.elim g.2.2 (fun xs hx => by
                rw [hx.1] at eh
                exact Or.inl (fun l => notLive u h2 h4 (fun t => hd_sn (Eq.trans eh.symm t))
                  (gone u xs huv eh h4 g.1 hx.2) (lb l)))
      · intro u xs e1 e2 x e3 e4
        rw [ehold u] at e1
        rw [ekept] at e2
        rw [ereach] at e3
        by_cases huv : u = v
        · rw [huv, hd_updS] at e1
          exact hd_sn e1.symm
        · rw [hd_updO _ _ _ _ huv] at e1
          have pe : Past w (w.srv x) u → Past w' (w'.srv x) u := fun p => by
            rw [esrv]; exact hd_pastEq w w' eseq ehead _ _ p
          cases eh : w.hold u with
          | none =>
            rw [eh] at e1
            exact hd_sn e1.symm
          | some r =>
            rw [eh] at e1
            have e1' : settleHold K one (w.seq v) sp tk u (w.seq u) r = some (Reach.known xs) := e1
            cases hd_settleSome K hK one _ _ _ _ _ r _ e1' with
            | inl same =>
              rw [← same] at eh
              have t := hH u xs eh e2 x e3 e4
              exact ⟨t.1, pe t.2⟩
            | inr g =>
              exact Exists.elim g.2.2 (fun xs0 h0 => Exists.elim h0 (fun ys hy => by
                have exy : xs = ys := hd_knownInj hy.2.1
                rw [hy.1] at eh
                cases ex0 : elem x xs0 with
                | false =>
                  have t := hH u xs0 eh e2 x e3 ex0
                  exact ⟨t.1, pe t.2⟩
                | true =>
                  rw [exy] at e4
                  have t := hy.2.2 x ex0 e4
                  exact ⟨t.1, pe (past u x huv g.1 t.1 t.2)⟩))
      · show w'.phase 0 = 0
        rw [ephase, hd_updO _ _ _ _ (fun e => hv0 e.symm)]
        exact hZ
      · intro u x a
        rw [evia] at a
        have t := hVP u x a
        rw [ereach]
        exact ⟨phm u t.1, t.2⟩
      · intro u a
        rw [ehold u] at a
        by_cases huv : u = v
        · rw [huv, hd_updS] at a
          exact absurd rfl a
        · rw [hd_updO _ _ _ _ huv] at a
          cases eh : w.hold u with
          | none =>
            rw [eh] at a
            exact absurd rfl a
          | some r => exact phm u (hHP u (fun t => hd_sn (Eq.trans eh.symm t)))
      · intro s u a
        rw [ehead] at a
        cases hHd s u a with
        | inl z => exact Or.inl z
        | inr p => exact Or.inr (phm u p)

    -- The spine a landing settles by names a version only where the landed one
    -- kept its records or drafted none.
    theorem hd_landSpine (K : Kernels) (hK : Exact K) (w : World) (v : Nat) :
        ∀ u, elem u (landSpine K w v) = true → elem u (w.spine v) = true ∧ (w.kept v = true ∨ w.recs v = false) := by
      intro u h
      unfold landSpine at h
      cases hs : K.spineSettles (w.recs v) (w.kept v) with
      | false => rw [hs] at h; exact ft h
      | true =>
        rw [hs] at h
        refine ⟨h, ?_⟩
        cases spine_settles_true K hK _ _ hs with
        | inl d => exact Or.inr d
        | inr k => exact Or.inl k

    theorem hd_landed (K : Kernels) (hK : Exact K) (one : Nat → Bool) (w : World) (v : Nat) (tk : List Nat)
        (hi : HeldInv one w) : HeldInv one (stLanded K one w v tk) := by
      unfold stLanded
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      have g1 := hd_andR hg
      have g2 := hd_andR g1
      have g3 := hd_andR g2
      exact hd_landedInv K hK one w _ v tk hi (hd_dec (hd_andL hg)) (hd_andL g1) (hd_andL g2)
        (fun x hx => by
          have t := hd_allL _ tk (hd_andL g3) x hx
          exact ⟨hd_andL t, hd_isTrue (hd_andR t)⟩)
        (fun e => by
          cases hd_or (hd_andR g3) with
          | inl n => rw [e] at n; exact ft n
          | inr r =>
            cases hd_or r with
            | inl k => exact Or.inl k
            | inr n =>
              cases eh : w.hold v with
              | none => exact Or.inr rfl
              | some x => rw [eh] at n; exact ft n)
        (landSpine K w v) (hd_landSpine K hK w v)
        rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl (fun u => rfl) rfl

    -- ---- adopt: v's records are kept; the others settle by descent only (no server
    --      listed as taking anything), covered by v.
    theorem hd_adoptInv (K : Kernels) (hK : Exact K) (one : Nat → Bool) (w w' : World) (v s : Nat)
        (hi : HeldInv one w) (hm : w.mine v = true) (hhs : w.head s = v) (hheld : ¬ (w.hold v = none))
        (hk : w.kept v = false)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = w.head)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : ∀ u, w'.hold u = settleOne K one (w.seq v) (w.spine v) [] u (w.seq u) (upd w.hold v none u))
        (ekept : w'.kept = upd w.kept v true) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have h2v := hHP v hheld
      have hv0 : ¬ (v = 0) := fun e => by rw [e, hZ] at h2v; exact hd_two 0 rfl h2v
      have stv : Stored w v := Exists.elim (hP s v hhs hv0 hm hk) (fun x hx => ⟨x, hx.2.2⟩)
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        ?_, ?_, hd_fZero w w' ephase hZ, hd_fVP w w' evia ephase ereach hVP, ?_, hd_fHd w w' ehead ephase hHd⟩
      · intro u h1 h2 h3 h4 h5
        rw [ephase] at h1
        rw [emine] at h2
        rw [erecs] at h3
        rw [ekept] at h4
        rw [ehold u] at h5
        by_cases huv : u = v
        · rw [huv, hd_updS] at h4
          exact hd_tf h4
        · rw [hd_updO _ _ _ _ huv] at h4
          rw [hd_updO _ _ _ _ huv] at h5
          have cf : Covered w u → Covered w' u := hd_covFwd w w' u
            (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
            (fun h a => by rw [ekept]; exact hd_keptMono w.kept v h a) (fun h y a => by rw [eres]; exact a)
          cases eh : w.hold u with
          | none =>
            cases hN u h1 h2 h3 h4 eh with
            | inl nl =>
              exact Or.inl (fun l => nl (hd_liveBack one w w' u (fun s a => by rw [ehead] at a; exact a)
                (by rw [ereach])
                (fun y c => hd_canBack one w w' u y (by rw [evia]) (by rw [einf])
                  (fun a => by rw [ephase] at a; exact a)
                  (fun s p => hd_pastEq w w' eseq ehead s u p) (fun _ => by rw [esrv]) c) l))
            | inr c => exact Or.inr (cf c)
          | some r =>
            rw [eh] at h5
            have d := hd_settleNil K hK one _ _ u _ r h5
            refine Or.inr ⟨v, ?_, ?_, ?_, ?_⟩
            · rw [espine]; exact d
            · rw [emine]; exact hm
            · exact Or.inl (by rw [ekept, hd_updS])
            · exact Exists.elim stv (fun x e => ⟨x, by rw [eres]; exact e⟩)
      · intro s' u e1 e2 e3 e4
        rw [ekept] at e4
        by_cases huv : u = v
        · rw [huv, hd_updS] at e4
          exact hd_tf e4
        · rw [hd_updO _ _ _ _ huv] at e4
          rw [ehead] at e1
          rw [emine] at e3
          rw [ereach, evia, eres]
          exact hP s' u e1 e2 e3 e4
      · intro u xs e1 e2 x e3 e4
        rw [ekept] at e2
        by_cases huv : u = v
        · rw [huv, hd_updS] at e2
          exact hd_tf e2
        · rw [hd_updO _ _ _ _ huv] at e2
          rw [ehold u] at e1
          rw [hd_updO _ _ _ _ huv] at e1
          rw [ereach] at e3
          cases eh : w.hold u with
          | none =>
            rw [eh] at e1
            exact hd_sn e1.symm
          | some r =>
            rw [eh] at e1
            have same := hd_settleNilSome K hK one _ _ u _ r _ e1
            rw [← same] at eh
            have t := hH u xs eh e2 x e3 e4
            exact ⟨t.1, by rw [esrv]; exact hd_pastEq w w' eseq ehead _ _ t.2⟩
      · intro u a
        rw [ehold u] at a
        rw [ephase]
        by_cases huv : u = v
        · rw [huv, hd_updS] at a
          exact absurd rfl a
        · rw [hd_updO _ _ _ _ huv] at a
          cases eh : w.hold u with
          | none =>
            rw [eh] at a
            exact absurd rfl a
          | some r => exact hHP u (fun t => hd_sn (Eq.trans eh.symm t))

    theorem hd_adopt (K : Kernels) (hK : Exact K) (one : Nat → Bool) (w : World) (v s : Nat) (hi : HeldInv one w) :
        HeldInv one (stAdopt K one w v s) := by
      unfold stAdopt
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      have g1 := hd_andR hg
      have g2 := hd_andR g1
      exact hd_adoptInv K hK one w _ v s hi (hd_andL hg) (hd_dec (hd_andL g1)) (hd_heldNe (hd_andL g2))
        (hd_not (hd_andR g2)) rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl (fun u => rfl) rfl

    -- ---- republish: a version not this Mac's, or kept, takes a head; heads only move forward.
    theorem hd_republishInv (one : Nat → Bool) (w w' : World) (v s0 : Nat) (hi : HeldInv one w)
        (hmk : w.mine v = true → w.kept v = true) (hph : 2 ≤ w.phase v)
        (hacc : w.head s0 = v ∨ w.seq (w.head s0) < w.seq v)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = upd w.head s0 v)
        (esrv : w'.srv = w.srv) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight) (eres : w'.res = w.res)
        (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hm : ∀ s, w'.head s = w.head s ∨ (w'.head s = v ∧ w.seq (w.head s) < w.seq v) := by
        intro s
        rw [ehead]
        by_cases hs : s = s0
        · rw [hs, hd_updS]
          cases hacc with
          | inl e => exact Or.inl e.symm
          | inr e => exact Or.inr ⟨rfl, e⟩
        · rw [hd_updO _ _ _ _ hs]
          exact Or.inl rfl
      have pm := hd_pastMono w w' v eseq hm
      refine ⟨?_, hd_fVia one w w' evia esrv hV, hd_fRes w w' eres evia einf hR, ?_, ?_, ?_, hd_fZero w w' ephase hZ,
        hd_fVP w w' evia ephase ereach hVP, hd_fHP w w' ehold ephase hHP, ?_⟩
      · intro u h1 h2 h3 h4 h5
        rw [ephase] at h1
        rw [emine] at h2
        rw [erecs] at h3
        rw [ekept] at h4
        rw [ehold] at h5
        by_cases huv : u = v
        · rw [huv] at h2
          rw [huv, hmk h2] at h4
          exact hd_tf h4
        · cases hN u h1 h2 h3 h4 h5 with
          | inl nl =>
            refine Or.inl (fun l => nl (hd_liveBack one w w' u ?_ ?_ ?_ l))
            · intro s a
              rw [ehead] at a
              by_cases hs : s = s0
              · rw [hs, hd_updS] at a; exact absurd a.symm huv
              · rw [hd_updO _ _ _ _ hs] at a; exact a
            · rw [ereach]
            · intro y c
              refine hd_canBack one w w' u y ?_ ?_ ?_ (fun s p => pm s u p) ?_ c
              · rw [evia]
              · rw [einf]
              · intro a; rw [ephase] at a; exact a
              · intro _; rw [esrv]
          | inr c =>
            exact Or.inr (hd_covFwd w w' u (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
              (fun h a => by rw [ekept]; exact a) (fun h y a => by rw [eres]; exact a) c)
      · intro u y s e1 e2
        rw [eres] at e1
        rw [evia] at e2
        rw [eseq]
        exact hd_accMono w.seq u (w.head s) (w'.head s) v (hm s) (hA u y s e1 e2)
      · intro s u e1 e2 e3 e4
        rw [emine] at e3
        rw [ekept] at e4
        rw [ereach, evia, eres]
        rw [ehead] at e1
        by_cases hs : s = s0
        · rw [hs, hd_updS] at e1
          rw [← e1] at e3
          rw [← e1, hmk e3] at e4
          exact hd_tf e4
        · rw [hd_updO _ _ _ _ hs] at e1
          exact hP s u e1 e2 e3 e4
      · intro u xs e1 e2 x e3 e4
        rw [ehold] at e1
        rw [ekept] at e2
        rw [ereach] at e3
        have t := hH u xs e1 e2 x e3 e4
        exact ⟨t.1, by rw [esrv]; exact pm _ _ t.2⟩
      · intro s u a
        rw [ehead] at a
        rw [ephase]
        by_cases hs : s = s0
        · rw [hs, hd_updS] at a
          rw [← a]
          exact Or.inr hph
        · rw [hd_updO _ _ _ _ hs] at a
          exact hHd s u a

    theorem hd_republish (one : Nat → Bool) (w : World) (v s : Nat) (hi : HeldInv one w) :
        HeldInv one (stRepublish w v s) := by
      unfold stRepublish
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      have g1 := hd_andR hg
      have hmk : w.mine v = true → w.kept v = true := fun e => by
        cases hd_or (hd_andL hg) with
        | inl n => rw [e] at n; exact ft n
        | inr k => exact k
      exact hd_republishInv one w _ v s hi hmk (hd_dec (hd_andL g1)) (hd_accept w s v true (hd_andR g1))
        rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ---- move: only a URL names_one_server calls not one server moves.
    theorem hd_moveInv (one moves : Nat → Bool) (hf : OneIsFixed one moves) (w w' : World) (x s : Nat)
        (hi : HeldInv one w) (hmv : moves x = true)
        (eseq : w'.seq = w.seq) (espine : w'.spine = w.spine) (emine : w'.mine = w.mine) (erecs : w'.recs = w.recs)
        (ereach : w'.reach = w.reach) (ephase : w'.phase = w.phase) (ehead : w'.head = w.head)
        (esrv : w'.srv = upd w.srv x s) (evia : w'.via = w.via) (einf : w'.inflight = w.inflight)
        (eres : w'.res = w.res) (ehold : w'.hold = w.hold) (ekept : w'.kept = w.kept) : HeldInv one w' := by
      have hN := hi.1
      have hV := hi.2.1
      have hR := hi.2.2.1
      have hA := hi.2.2.2.1
      have hP := hi.2.2.2.2.1
      have hH := hi.2.2.2.2.2.1
      have hZ : w.phase 0 = 0 := hi.2.2.2.2.2.2.1
      have hVP := hi.2.2.2.2.2.2.2.1
      have hHP := hi.2.2.2.2.2.2.2.2.1
      have hHd := hi.2.2.2.2.2.2.2.2.2
      have hox := hf x hmv
      have srvEq : ∀ y, one y = true → w'.srv y = w.srv y := fun y o => by
        have hyx : ¬ (y = x) := fun e => by rw [e, hox] at o; exact ft o
        rw [esrv, hd_updO _ _ _ _ hyx]
      refine ⟨?_, ?_, hd_fRes w w' eres evia einf hR, hd_fAcc w w' eres evia eseq ehead hA,
        hd_fProv w w' ehead emine ekept ereach evia eres hP, ?_, hd_fZero w w' ephase hZ,
        hd_fVP w w' evia ephase ereach hVP, hd_fHP w w' ehold ephase hHP, hd_fHd w w' ehead ephase hHd⟩
      · intro u h1 h2 h3 h4 h5
        refine hd_nle one w w' u hN ?_ ?_ ?_ ?_ ?_ ?_ ?_ h1 h2 h3 h4 h5
        · intro a; rw [ephase] at a; exact a
        · intro a; rw [emine] at a; exact a
        · intro a; rw [erecs] at a; exact a
        · intro a; rw [ekept] at a; exact a
        · intro a; rw [ehold] at a; exact a
        · refine hd_liveBack one w w' u ?_ ?_ ?_
          · intro s a; rw [ehead] at a; exact a
          · rw [ereach]
          · intro y c
            exact hd_canBack one w w' u y (by rw [evia]) (by rw [einf]) (fun a => by rw [ephase] at a; exact a)
              (fun s p => hd_pastEq w w' eseq ehead s u p) (srvEq y) c
        · exact hd_covFwd w w' u (fun h _ => by rw [espine, emine, erecs]; exact ⟨rfl, rfl, rfl⟩)
            (fun h a => by rw [ekept]; exact a) (fun h y a => by rw [eres]; exact a)
      · intro u y s' a b
        rw [evia] at a
        rw [srvEq y b]
        exact hV u y s' a b
      · intro u xs e1 e2 y e3 e4
        rw [ehold] at e1
        rw [ekept] at e2
        rw [ereach] at e3
        have t := hH u xs e1 e2 y e3 e4
        exact ⟨t.1, by rw [srvEq y t.1]; exact hd_pastEq w w' eseq ehead _ _ t.2⟩

    theorem hd_move (one moves : Nat → Bool) (hf : OneIsFixed one moves) (w : World) (x s : Nat)
        (hi : HeldInv one w) : HeldInv one (stMove moves w x s) := by
      unfold stMove
      refine hd_ite one _ _ _ ?_ hi
      intro hg
      exact hd_moveInv one moves hf w _ x s hi hg rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl rfl

    -- ===========================================================================
    -- The theorems.
    -- ===========================================================================

    theorem held_inv_init (one : Nat → Bool) : HeldInv one w0 := by
      refine ⟨?_, ?_, ?_, ?_, ?_, ?_, rfl, ?_, ?_, ?_⟩
      · intro v h1 _ _ _ _
        exact absurd h1 (hd_two (w0.phase v) rfl)
      · intro v x s a _
        exact hd_sn a.symm
      · intro v x
        exact ⟨fun a => absurd rfl a, fun a => ft a⟩
      · intro v x s a _
        exact hd_sn a.symm
      · intro s v a b _ _
        exact absurd a.symm b
      · intro v xs a
        exact hd_sn a.symm
      · intro v x a
        exact absurd rfl a
      · intro v a
        exact absurd rfl a
      · intro s v a
        exact Or.inl a.symm

    theorem held_inv_step (K : Kernels) (hK : Exact K) (one moves : Nat → Bool) (hf : OneIsFixed one moves)
        (w w' : World) (hi : HeldInv one w) (hn : ∃ e, w' = step K one moves w e) : HeldInv one w' :=
      Exists.elim hn (fun e he => by
        rw [he]
        cases e with
        | draft v o sp m d r => exact hd_draft one w v o sp m d r hi
        | hold v unk => exact hd_hold one w v unk hi
        | send v x => exact hd_send one w v x hi
        | arrive v x ok => exact hd_arrive one w v x ok hi
        | refusedAll v => exact hd_refused K hK one w v hi
        | keep v => exact hd_keep K one w v hi
        | landed v tk => exact hd_landed K hK one w v tk hi
        | adopt v s => exact hd_adopt K hK one w v s hi
        | republish v s => exact hd_republish one w v s hi
        | move x s => exact hd_move one moves hf w x s hi)

    -- HELD DRAFTS ARE SAFE: in every state of every run, for any exact kernels and
    -- any names_one_server that never calls a URL the world moves one server.
    theorem held_safe (K : Kernels) (hK : Exact K) (one moves : Nat → Bool) (hf : OneIsFixed one moves) :
        Satisfies (HeldSpec K one moves) (Always (Lift (HeldInv one))) :=
      invariantSatisfies (HeldSpec K one moves) (HeldInv one)
        (fun s hs => by
          have e : s = w0 := hs
          rw [e]
          exact held_inv_init one)
        (fun s t hi hn => held_inv_step K hK one moves hf s t hi hn)

    -- NO LOST EDIT: in every state where the spines carry what they name (the descent
    -- rule's obligation on the draft), every edit of this Mac is safe.
    theorem no_lost_edit (K : Kernels) (hK : Exact K) (one moves : Nat → Bool) (hf : OneIsFixed one moves)
        (carries : Nat → Nat → Prop) :
        Satisfies (HeldSpec K one moves) (Always (Lift (fun w => TextInheritance carries w → EditSafe carries one w))) :=
      fun b runs n ht => by
        have h : HeldInv one (drop b n 0) := held_safe K hK one moves hf b runs n
        exact edit_safe_of carries one _ h.1 ht

    -- ===========================================================================
    -- 7. The rules W2 replaced, each a run the kernel evaluates: the shipped rule
    --    keeps the edit, the replaced rule loses it (lostAt). Never a rejected
    --    declaration: each mutant is a step of its own, and its run is computed.
    -- ===========================================================================

    def oneAll : Nat → Bool := fun _ => true
    def movesNone : Nat → Bool := fun _ => false

    -- W2 (edb54cf8f0): P1 stored as v1, reply lost; P2 from the same opened text is
    -- refused as not newer. Per-version holds keep v1's records.
    def lostReply : List Event :=
      [ Event.draft 1 0 [0] true true [1], Event.hold 1 false, Event.send 1 1, Event.arrive 1 1 true,
        Event.draft 2 0 [0] true true [1], Event.hold 2 false, Event.send 2 1, Event.arrive 2 1 true,
        Event.refusedAll 2 ]

    theorem per_version_hold_keeps_v1 : lostAt (run spec oneAll movesNone w0 lostReply) 1 1 = false := rfl
    theorem v1_is_on_the_server : (run spec oneAll movesNone w0 lostReply).head 1 = 1 := rfl

    -- The single slot (SINGLE_SLOT_HOLD): a refusal forgets every hold.
    def stepSingleSlot (K : Kernels) (one moves : Nat → Bool) (w : World) : Event → World
      | Event.refusedAll v =>
          if w.phase v == 2 && w.mine v && forgets K (answers w v) then
            { w with hold := fun _ => none, phase := upd w.phase v 3 }
          else w
      | e => step K one moves w e

    def runSingleSlot (K : Kernels) (one moves : Nat → Bool) (w : World) : List Event → World
      | [] => w
      | e :: es => runSingleSlot K one moves (stepSingleSlot K one moves w e) es

    theorem single_slot_loses_v1 : lostAt (runSingleSlot spec oneAll movesNone w0 lostReply) 1 1 = true := rfl

    -- W2 review 1 (5e557e520b): v1 reaches only s2 (reply lost); the retry v2 lands on s1 alone.
    def unreached : List Event :=
      [ Event.draft 1 0 [0] true true [1, 2], Event.hold 1 false, Event.send 1 2, Event.arrive 1 2 true,
        Event.draft 2 0 [0] true true [1, 2], Event.hold 2 false, Event.send 2 1, Event.arrive 2 1 true,
        Event.send 2 2, Event.arrive 2 2 true, Event.keep 2, Event.landed 2 [1] ]

    theorem reach_rule_keeps_v1 : lostAt (run spec oneAll movesNone w0 unreached) 1 2 = false := rfl

    -- Settling by sequence alone: a head of a higher sequence that some server took drops the hold.
    def bySeq : Kernels := { spec with stays := fun _ _ => false }

    theorem settling_by_sequence_loses_v1 : lostAt (run bySeq oneAll movesNone w0 unreached) 1 2 = true := rfl

    -- W2 review 2 (2c972e55a5): URL 7 (the ALab name) moves from s1 to s2 between the publishes.
    def moving : List Event :=
      [ Event.draft 1 0 [0] true true [7], Event.hold 1 false, Event.send 1 7, Event.arrive 1 7 true,
        Event.move 7 2,
        Event.draft 2 0 [0] true true [7], Event.hold 2 false, Event.send 2 7, Event.arrive 2 7 true,
        Event.keep 2, Event.landed 2 [7] ]

    def oneButSeven : Nat → Bool := fun x => x != 7
    def movesSeven : Nat → Bool := fun x => x == 7

    theorem alab_name_settles_nothing : lostAt (run spec oneButSeven movesSeven w0 moving) 1 7 = false := rfl
    theorem alab_name_as_one_server_loses_v1 : lostAt (run spec oneAll movesSeven w0 moving) 1 7 = true := rfl

    -- The hold before the PUT: a publish may send only once its hold is written.
    def stepSendFirst (K : Kernels) (one moves : Nat → Bool) (w : World) : Event → World
      | Event.send v x =>
          if (w.phase v == 1 || w.phase v == 2) && elem x (w.reach v) && (w.via v x).isNone then
            { w with via := upd2 w.via v x (some (w.srv x)), inflight := upd2 w.inflight v x true }
          else w
      | e => step K one moves w e

    def runSendFirst (K : Kernels) (one moves : Nat → Bool) (w : World) : List Event → World
      | [] => w
      | e :: es => runSendFirst K one moves (stepSendFirst K one moves w e) es

    def crashAfterPut : List Event :=
      [ Event.draft 1 0 [0] true true [1], Event.send 1 1, Event.arrive 1 1 true ]

    theorem hold_first_sends_nothing_unheld : lostAt (run spec oneAll movesNone w0 crashAfterPut) 1 1 = false := rfl
    theorem put_before_hold_loses_v1 : lostAt (runSendFirst spec oneAll movesNone w0 crashAfterPut) 1 1 = true := rfl

    -- The landed spine's gate (spine_settles): v3, drafted from v2 with v1 left out of its spine
    -- (capped at MAX_PARENTS), lands on s2 first and settles v2's hold by descent, so v2's
    -- records are never kept; v2 then lands on s2, and settles the other holds by reach alone.
    def cappedSpine : List Event :=
      [ Event.draft 1 0 [0] true true [1], Event.hold 1 false, Event.send 1 1, Event.arrive 1 1 true,
        Event.draft 2 1 [1, 0] true true [2], Event.hold 2 false, Event.send 2 2, Event.arrive 2 2 true,
        Event.draft 3 2 [2, 0] true true [2], Event.hold 3 false, Event.send 3 2, Event.arrive 3 2 true,
        Event.keep 3, Event.landed 3 [2], Event.landed 2 [2] ]

    theorem capped_spine_keeps_nothing_of_v2 : (run spec oneAll movesNone w0 cappedSpine).kept 2 = false := rfl
    theorem spine_gate_keeps_v1 : lostAt (run spec oneAll movesNone w0 cappedSpine) 1 1 = false := rfl

    -- Settling by the spine whatever became of the records (SPINE_ALWAYS): v2's spine names v1,
    -- whose hold goes while s1 serves it.
    def spineAlways : Kernels := { spec with spineSettles := fun _ _ => true }

    theorem spine_without_kept_records_loses_v1 : lostAt (run spineAlways oneAll movesNone w0 cappedSpine) 1 1 = true := rfl


    -- ===========================================================================
    -- 8. WAITING WRITES (one-mesh M4, W3): the [everyone] writes a publish keeps
    --    pending on its own version, the ones that wait once a server took it, and
    --    the pass that signs them. Writes are ids from 1 in publish order (a later
    --    publish's set is a later set); names and values are Nat (value 0: absent,
    --    a take-back's value); a write's bases are the values it may be signed over.
    --    One event is one state-lock section. A publish whose writes wait, from
    --    before its draft reads the log until its hold and pending set are written,
    --    is one event: it holds this Mac's draft turn (draft_turn, .everyone.turn),
    --    in which no other publish of this Mac drafts, and its claim
    --    (.everyone.claim), which no pass signs through. A publish whose records
    --    write a name directly (a direct draft, ids from 1) is its steps: the draft
    --    in its turn, the hold that ends the turn (hold_drafted), and the keep of
    --    its records once a server took its version (keep_drafted_until, only while
    --    its hold is there) or the hold's going unkept (a refusal, a descent). A
    --    pass defers each name a direct draft in flight writes, as it defers each
    --    name a pending write writes: their records are not in the log it reads.
    -- ===========================================================================

    -- lastN n p: the largest i below n with p i, or 0 (no write is 0).
    def lastN : Nat → (Nat → Bool) → Nat
      | 0, _ => 0
      | Nat.succ n, p => if p n = true then n else lastN n p

    structure WWorld where
      next : Nat                  -- every write id below next is born
      nm : Nat → Nat              -- a write's name
      val : Nat → Nat             -- the value it puts (0: it takes the name back to absent)
      bs : Nat → Nat → Bool       -- its bases: what the log may hold for it to be signed
      ver : Nat → Nat             -- the version whose pending set it is in
      used : Nat → Bool           -- versions a publish kept writes for
      pend : Nat → Bool           -- it is pending, in its version's set
      waitOf : Nat → Nat          -- per name, the write that waits there (0: none)
      log : Nat → Nat             -- per name, the value the log holds (0: absent)
      holder : Nat → Nat          -- per name, the write that put it (0: another Mac)
      stored : Nat → Bool         -- a server took the version
      refused : Nat → Bool        -- the version is on no server for good
      unread : Bool               -- a pending set does not open (W3: never read as none)
      due : Bool                  -- a newest [everyone] epoch wraps a removed Mac
      passRead : Nat → Nat        -- per name, the write the last pass settled (0: none)
      fate : Nat → Nat            -- ghost: 0 none, 1 signed or done, 2 overtaken, 3 replaced, 4 dropped
      overBy : Nat → Nat          -- ghost: the write that held the name when one was overtaken
      holdUnread : Bool           -- a held draft does not read (W3: never read as none)
      dnext : Nat                 -- every direct draft id below dnext is born
      dnm : Nat → Nat             -- a direct draft's name
      dval : Nat → Nat            -- the value its records put there
      dstamp : Nat → Nat          -- next at its draft: the writes born before it are below
      dphase : Nat → Nat          -- 0 unborn, 1 drafting (its turn), 2 held, 3 kept or gone
      hdirect : Nat → Nat         -- per name, the stamp of the direct draft whose kept value the log holds (0: none)
      overD : Nat → Nat           -- ghost: that stamp at the name when a write was overtaken (0: none)

    inductive WEvent where
      -- publish v k names vals bases opened: writes next .. next+k-1 pending on v
      -- (wait_for_key), after the sets of the versions `opened` holds land
      | publish : Nat → Nat → (Nat → Nat) → (Nat → Nat) → (Nat → Nat → Bool) → (Nat → Bool) → WEvent
      | stored : Nat → WEvent
      -- forget_waiting (every server refused v) or land_published's settle (each
      -- server v's publish may have reached took a version that leaves v out)
      | dropped : Nat → WEvent
      -- land_waiting: the sets of the versions `S` says a server holds
      | land : (Nat → Bool) → WEvent
      | signPass : Bool → WEvent      -- the pass's commit (it opens the newest epoch or not)
      | settlePass : WEvent           -- the pass's settle, a lock section of its own
      | other : Nat → Nat → WEvent    -- another write of a name (another Mac, a rotation's cascade)
      | due : Bool → WEvent
      | unreadable : Bool → WEvent
      -- directDraft n x: a publish of this Mac drafts records that write name n to
      -- x directly, in its turn (no other publish of this Mac drafts meanwhile)
      | directDraft : Nat → Nat → WEvent
      | directHold : Nat → WEvent     -- hold_drafted: its hold is written, its turn ends
      | keep : Nat → WEvent           -- keep_drafted_until: its records enter the log
      | unhold : Nat → WEvent         -- its hold goes unkept, or its publish ends before it
      | holdUnreadable : Bool → WEvent

    -- Whether write i is born, a later pending write j of its name lands with S.
    def laterLands (w : WWorld) (S : Nat → Bool) (i : Nat) : Bool :=
      anyN w.next (fun j => decide (i < j) && (w.pend j && (S (w.ver j) && decide (w.nm j = w.nm i))))
    -- The write of name n that lands last with S (0: none).
    def landingOf (w : WWorld) (S : Nat → Bool) (n : Nat) : Nat :=
      lastN w.next (fun i => w.pend i && (S (w.ver i) && decide (w.nm i = n)))

    -- land_sets: each pending write whose version S holds lands, the last of its
    -- name waits (a later set's over an earlier one's and over what waited), and
    -- each name a landing set writes leaves every earlier set (pending_keeps).
    def landW (K : Kernels) (S : Nat → Bool) (w : WWorld) : WWorld :=
      { w with
        pend := fun i => w.pend i && K.keeps (S (w.ver i)) (laterLands w S i),
        waitOf := fun n => if landingOf w S n = 0 then w.waitOf n else landingOf w S n,
        fate := fun i =>
          if (w.pend i && (!(K.keeps (S (w.ver i)) (laterLands w S i)) && !(decide (landingOf w S (w.nm i) = i))))
             || (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i) && !(decide (landingOf w S (w.nm i) = 0))))
          then 3 else w.fate i }

    -- A direct draft of this Mac writes name n, drafted and not yet kept or gone:
    -- its records are this Mac's own edit in flight, which the log a pass reads
    -- does not hold yet.
    def heldW (w : WWorld) (n : Nat) : Bool :=
      anyN w.dnext (fun d => (decide (w.dphase d = 1) || decide (w.dphase d = 2)) && decide (w.dnm d = n))

    -- A pending write of this Mac writes name n, or a direct draft of this Mac in
    -- flight does (heldW), or a held draft does not read and may write any name
    -- (W3): a pass leaves n's waiting write for it. Signed under a direct draft in
    -- flight, the write would meet that draft's records as a concurrent write of
    -- the name once they are kept, and an earlier edit's keep would take the place
    -- of the later edit the pass signed (LaterKept).
    def deferredW (w : WWorld) (n : Nat) : Bool :=
      anyN w.next (fun j => w.pend j && decide (w.nm j = n)) || (w.holdUnread || heldW w n)

    -- Verdict::of: 0 leaves, 1 done, 2 overtaken, 3 signs.
    def verdictOf (K : Kernels) (deferred done standsOver takeBack : Bool) : Nat :=
      if deferred = true then 0
      else if K.signs done standsOver takeBack = true then 3
      else if K.overtaken done standsOver takeBack = true then 2
      else 1

    -- The pass's verdict on the write that waits at name n (0 where none waits).
    def passVerdict (K : Kernels) (w : WWorld) (n : Nat) : Nat :=
      if w.waitOf n = 0 then 0
      else verdictOf K (deferredW w n) (decide (w.log n = w.val (w.waitOf n)))
        (w.bs (w.waitOf n) (w.log n)) (decide (w.val (w.waitOf n) = 0))

    -- sign_waiting_writes' commit: each waiting write's verdict; a signed write
    -- puts its value, the others leave the log as it is. It runs only where
    -- pass_signs (signW).
    def signBody (K : Kernels) (w : WWorld) : WWorld :=
      { w with
        log := fun n => if passVerdict K w n = 3 then w.val (w.waitOf n) else w.log n,
        holder := fun n => if passVerdict K w n = 3 then w.waitOf n else w.holder n,
        passRead := fun n => if passVerdict K w n = 0 then 0 else w.waitOf n,
        fate := fun i =>
          if !(decide (i = 0)) && decide (w.waitOf (w.nm i) = i) then
            (if passVerdict K w (w.nm i) = 0 then w.fate i
             else if passVerdict K w (w.nm i) = 2 then 2 else 1)
          else w.fate i,
        overBy := fun i =>
          if !(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i) && decide (passVerdict K w (w.nm i) = 2))
          then w.holder (w.nm i) else w.overBy i,
        hdirect := fun n => if passVerdict K w n = 3 then 0 else w.hdirect n,
        overD := fun i =>
          if !(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i) && decide (passVerdict K w (w.nm i) = 2))
          then w.hdirect (w.nm i) else w.overD i }

    def signW (K : Kernels) (opens : Bool) (w : WWorld) : WWorld :=
      if K.passSigns w.due opens (!w.unread) = true then signBody K w else w

    -- The pass's settle: a write still waiting as the pass read it goes.
    def settleW (w : WWorld) : WWorld :=
      { w with
        waitOf := fun n =>
          if !(decide (w.passRead n = 0)) && decide (w.waitOf n = w.passRead n) then 0 else w.waitOf n,
        passRead := fun _ => 0 }

    -- wait_for_key: k writes of version v, its set after every other.
    def pushW (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool) : WWorld :=
      { w with
        next := w.next + k,
        nm := fun i => if w.next ≤ i ∧ i < w.next + k then names (i - w.next) else w.nm i,
        val := fun i => if w.next ≤ i ∧ i < w.next + k then vals (i - w.next) else w.val i,
        bs := fun i => if w.next ≤ i ∧ i < w.next + k then bases (i - w.next) else w.bs i,
        ver := fun i => if w.next ≤ i ∧ i < w.next + k then v else w.ver i,
        pend := fun i => if w.next ≤ i ∧ i < w.next + k then true else w.pend i,
        used := upd w.used v true }

    -- A publish's guard: v is a new version, its draft is not refused for writes
    -- that do not read (draft_refuses: no pending set fails to open, and no held
    -- draft, as hu says of them), every version it opened is stored, its names are
    -- distinct, each write's bases hold what the log holds at its name (the log's
    -- value the draft read is one of a write's bases), and a take-back to absent is
    -- drafted only where the log holds none (everyone_edit writes the log's value
    -- back otherwise).
    def pubBase (K : Kernels) (hu : Bool) (w : WWorld) (v k : Nat) (names vals : Nat → Nat)
        (bases : Nat → Nat → Bool) (opened : Nat → Bool) : Bool :=
      !(w.used v) && !(K.draftRefuses (decide (0 < k)) w.unread hu) && decide (0 < k)
        && allN w.next (fun i => !(w.pend i && opened (w.ver i)) || w.stored (w.ver i))
        && allN k (fun a => allN k (fun b => decide (a = b) || !(decide (names a = names b))))
        && allN k (fun a => bases a (w.log (names a)))
        && allN k (fun a => !(decide (vals a = 0)) || decide (w.log (names a) = 0))

    -- It is the publish's turn: no direct draft of this Mac is between its draft and
    -- its hold (draft_turn).
    def turnFree (w : WWorld) : Bool := allN w.dnext (fun d => !(decide (w.dphase d = 1)))

    -- Each write's bases hold what each held draft of its name leaves there
    -- (held_values): that publish's records enter the log only once it keeps them.
    def heldIn (w : WWorld) (k : Nat) (names : Nat → Nat) (bases : Nat → Nat → Bool) : Bool :=
      allN k (fun a => allN w.dnext (fun d =>
        !(decide (w.dphase d = 2) && decide (w.dnm d = names a)) || bases a (w.dval d)))

    def publishOk (K : Kernels) (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool)
        (opened : Nat → Bool) : Bool :=
      pubBase K w.holdUnread w v k names vals bases opened && (turnFree w && heldIn w k names bases)

    -- keep_drafted_until of direct draft d: its value enters the log at its name.
    def keepBody (w : WWorld) (d : Nat) : WWorld :=
      { w with log := upd w.log (w.dnm d) (w.dval d), holder := upd w.holder (w.dnm d) 0,
               hdirect := upd w.hdirect (w.dnm d) (w.dstamp d), dphase := upd w.dphase d 3 }

    def wstep (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if publishOk K w v k names vals bases opened = true then pushW (landW K opened w) v k names vals bases
          else w
      | WEvent.stored v => if w.refused v then w else { w with stored := upd w.stored v true }
      | WEvent.dropped v =>
          if w.stored v then w
          else { w with refused := upd w.refused v true,
                        pend := fun i => w.pend i && !(decide (w.ver i = v)),
                        fate := fun i => if w.pend i && decide (w.ver i = v) then 4 else w.fate i }
      | WEvent.land S =>
          if !(w.unread) && allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i)) then landW K S w
          else w
      | WEvent.signPass opens => signW K opens w
      | WEvent.settlePass => settleW w
      | WEvent.other n x => { w with log := upd w.log n x, holder := upd w.holder n 0, hdirect := upd w.hdirect n 0 }
      | WEvent.due b => { w with due := b }
      | WEvent.unreadable b => { w with unread := b }
      | WEvent.directDraft n x =>
          if turnFree w = true then
            { w with dnext := w.dnext + 1, dnm := upd w.dnm w.dnext n, dval := upd w.dval w.dnext x,
                     dstamp := upd w.dstamp w.dnext w.next, dphase := upd w.dphase w.dnext 1 }
          else w
      | WEvent.directHold d => if decide (w.dphase d = 1) = true then { w with dphase := upd w.dphase d 2 } else w
      | WEvent.keep d => if K.draftedKeeps (decide (w.dphase d = 2)) = true then keepBody w d else w
      | WEvent.unhold d =>
          if (decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true then { w with dphase := upd w.dphase d 3 }
          else w
      | WEvent.holdUnreadable b => { w with holdUnread := b }

    def wrun (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrun K (wstep K w e) es

    def ww0 : WWorld :=
      { next := 1, nm := fun _ => 0, val := fun _ => 0, bs := fun _ _ => false, ver := fun _ => 0,
        used := fun _ => false, pend := fun _ => false, waitOf := fun _ => 0, log := fun _ => 0,
        holder := fun _ => 0, stored := fun v => v == 0, refused := fun _ => false, unread := false,
        due := false, passRead := fun _ => 0, fate := fun _ => 0, overBy := fun _ => 0,
        holdUnread := false, dnext := 1, dnm := fun _ => 0, dval := fun _ => 0, dstamp := fun _ => 0,
        dphase := fun _ => 0, hdirect := fun _ => 0, overD := fun _ => 0 }

    def WaitSpec (K : Kernels) : StateMachine WWorld :=
      StateMachine.mk (fun w => w = ww0) (fun w w' => ∃ e, w' = wstep K w e)

    -- Write i is accounted for: still pending, waiting, signed or done, overtaken
    -- (dropped and recorded), or replaced by a later write of this Mac of its name.
    def Accounted (w : WWorld) (i : Nat) : Prop :=
      w.pend i = true ∨ w.waitOf (w.nm i) = i ∨ w.fate i = 1 ∨ w.fate i = 2
        ∨ (w.fate i = 3 ∧ ∃ j, i < j ∧ j < w.next ∧ w.nm j = w.nm i)

    -- W-a: a write waits, or is signed, only once a server took its version.
    def SignedOnlyStored (w : WWorld) : Prop :=
      (∀ n, ¬ (w.waitOf n = 0) → w.stored (w.ver (w.waitOf n)) = true)
        ∧ (∀ i, w.fate i = 1 → w.stored (w.ver i) = true)

    -- W-c: a write of a version a server took is never lost, with no bound on the
    -- pending sets and with sets that do not open never read as none.
    def NoLostWrite (w : WWorld) : Prop :=
      ∀ i, 1 ≤ i → i < w.next → w.stored (w.ver i) = true → Accounted w i

    -- W-b: a pass changes a name's value only by signing the write that waits
    -- there, over one of its bases, never as a take-back; any other change is
    -- another write's (holder 0: another Mac's, or this Mac's direct draft's keep).
    def CasStep (w w' : WWorld) : Prop :=
      ∀ n, w'.log n = w.log n ∨ w'.holder n = 0
        ∨ (¬ (w.waitOf n = 0) ∧ w.bs (w.waitOf n) (w.log n) = true ∧ ¬ (w.val (w.waitOf n) = 0)
            ∧ w'.log n = w.val (w.waitOf n))

    -- W-d: a write is overtaken only by a value it never saw from this Mac: no
    -- waiting write of this Mac held its name, and a direct draft of this Mac whose
    -- kept value held it was drafted after the write (the person's later edit),
    -- never one of this Mac's earlier publishes, whose values its bases hold, unless
    -- the write is a take-back (F15: a pass signs no Delete).
    def OvertakenByAnother (w : WWorld) : Prop :=
      ∀ i, w.fate i = 2 → w.overBy i = 0 ∧ (w.val i = 0 ∨ w.overD i = 0 ∨ i < w.overD i)

    -- W-e: a write a pass signed, or found done, keeps its value against every
    -- direct draft of this Mac from before it: where the log's value of its name
    -- is a direct draft's kept value, and that draft was drafted before the write,
    -- the log holds the write's value. An earlier edit's keep never takes the
    -- place of the person's later edit.
    def LaterKept (w : WWorld) : Prop :=
      ∀ i, w.fate i = 1 → w.hdirect (w.nm i) ≠ 0 → w.hdirect (w.nm i) ≤ i → w.log (w.nm i) = w.val i

    -- ===========================================================================
    -- 9. THE WAITING-WRITE LAWS, for every number of writes, names, versions and
    --    passes, and for any kernels that compute the spec: a write waits or is
    --    signed only once a server took its version (W-a), a pass changes a value
    --    only by signing the waiting write over one of its bases (W-b), no write
    --    of a stored version is lost (W-c), a write is overtaken only by
    --    another Mac's value (W-d), and an earlier direct draft's keep never takes
    --    the place of a later write a pass signed (W-e). W-a, W-c and W-d follow
    --    from one inductive invariant, WaitInv (W-d with DirectInv too), W-e from
    --    LaterInv with both; W-b is a property of every step.
    -- ===========================================================================

    theorem ww_and {a b : Bool} (h : (a && b) = true) : a = true ∧ b = true := by
      cases a <;> cases b <;> first | exact ⟨rfl, rfl⟩ | exact ft h
    theorem ww_or {a b : Bool} (h : (a || b) = true) : a = true ∨ b = true := by
      cases a <;> cases b <;> first | exact Or.inl rfl | exact Or.inr rfl | exact ft h
    theorem ww_not {a : Bool} (h : (!a) = true) : a = false := by
      cases a <;> first | rfl | exact ft h
    theorem ww_nn {a b : Bool} (h : (!a && !b) = false) : a = true ∨ b = true := by
      cases a <;> cases b <;> first | exact Or.inl rfl | exact Or.inr rfl | exact ft h.symm
    theorem ww_nn_true {a b : Bool} (h : (!a && !b) = true) : a = false ∧ b = false := by
      cases a <;> cases b <;> first | exact ⟨rfl, rfl⟩ | exact ft h
    theorem ww_df {p : Prop} [Decidable p] (h : decide p = false) : ¬ p :=
      fun hp => ft (Eq.trans h.symm (dtrue hp))
    theorem ww_ne_true {a : Bool} (h : ¬ (a = true)) : a = false := by
      cases a <;> first | rfl | exact absurd rfl h
    -- Or elimination by lambdas. (Clean's `cases`/`rcases` infer a projection of a
    -- later alternative's binder from the first alternative's type; a lambda's
    -- binder takes its type from the disjunction.)
    theorem ww_or_cases {a b c : Prop} (h : a ∨ b) (f : a → c) (g : b → c) : c := by
      cases h with
      | inl x => exact f x
      | inr y => exact g y
    theorem ww_by_cases (p : Prop) [Decidable p] {q : Prop} (f : p → q) (g : ¬ p → q) : q := by
      by_cases h : p
      · exact f h
      · exact g h

    -- lastN: the largest index below n that p holds of, never one past it.
    theorem ww_lastN_ge (p : Nat → Bool) : ∀ (n i : Nat), i < n → p i = true → i ≤ lastN n p := by
      intro n
      induction n with
      | zero => intro i hi _; exact absurd hi (by omega)
      | succ n ih =>
        intro i hi hp
        by_cases hpn : p n = true
        · have e : lastN (Nat.succ n) p = n := show (if p n = true then n else lastN n p) = n from if_pos hpn
          rw [e]; omega
        · have e : lastN (Nat.succ n) p = lastN n p :=
            show (if p n = true then n else lastN n p) = lastN n p from if_neg hpn
          have hin : ¬ (i = n) := fun h => hpn (Eq.mpr (congrArg (fun x => p x = true) h.symm) hp)
          rw [e]; exact ih i (by omega) hp

    theorem ww_lastN_spec (p : Nat → Bool) : ∀ (n : Nat), lastN n p ≠ 0 → p (lastN n p) = true ∧ lastN n p < n := by
      intro n
      induction n with
      | zero => intro h; exact absurd rfl h
      | succ n ih =>
        intro h
        by_cases hpn : p n = true
        · have e : lastN (Nat.succ n) p = n := show (if p n = true then n else lastN n p) = n from if_pos hpn
          rw [e]; exact ⟨hpn, by omega⟩
        · have e : lastN (Nat.succ n) p = lastN n p :=
            show (if p n = true then n else lastN n p) = lastN n p from if_neg hpn
          have r := ih (fun z => h (Eq.trans e z))
          rw [e]
          exact ⟨r.1, by omega⟩

    -- The write of name n that lands with S is pending, of a version S holds, of name n, born.
    theorem ww_landing (w : WWorld) (S : Nat → Bool) (n : Nat) (h : landingOf w S n ≠ 0) :
        w.pend (landingOf w S n) = true ∧ S (w.ver (landingOf w S n)) = true
          ∧ w.nm (landingOf w S n) = n ∧ landingOf w S n < w.next := by
      have r := ww_lastN_spec (fun i => w.pend i && (S (w.ver i) && decide (w.nm i = n))) w.next h
      have a := ww_and r.1
      have b := ww_and a.2
      exact ⟨a.1, b.1, of_decide_eq_true b.2, r.2⟩

    theorem ww_landing_ge (w : WWorld) (S : Nat → Bool) (i : Nat) (hp : w.pend i = true)
        (hs : S (w.ver i) = true) (hi : i < w.next) : i ≤ landingOf w S (w.nm i) := by
      apply ww_lastN_ge (fun j => w.pend j && (S (w.ver j) && decide (w.nm j = w.nm i))) w.next i hi
      show (w.pend i && (S (w.ver i) && decide (w.nm i = w.nm i))) = true
      rw [hp, hs, dtrue (rfl : w.nm i = w.nm i)]

    theorem ww_later_elim (w : WWorld) (S : Nat → Bool) (i : Nat) (h : laterLands w S i = true) :
        ∃ j, j < w.next ∧ i < j ∧ w.pend j = true ∧ S (w.ver j) = true ∧ w.nm j = w.nm i := by
      cases anyN_elim _ w.next h with
      | intro j hj =>
        have a := ww_and hj.2
        have b := ww_and a.2
        have c := ww_and b.2
        exact ⟨j, hj.1, of_decide_eq_true a.1, b.1, c.1, of_decide_eq_true c.2⟩

    theorem ww_later_intro (w : WWorld) (S : Nat → Bool) (i j : Nat) (hj : j < w.next) (hij : i < j)
        (hp : w.pend j = true) (hs : S (w.ver j) = true) (hn : w.nm j = w.nm i) : laterLands w S i = true := by
      apply anyN_intro _ w.next j hj
      show (decide (i < j) && (w.pend j && (S (w.ver j) && decide (w.nm j = w.nm i)))) = true
      rw [dtrue hij, hp, hs, dtrue hn]

    theorem wl_or_l {a b : Bool} (h : a = true) : (a || b) = true := by
      cases a <;> first | rfl | exact ft h
    theorem wl_or_r {a b : Bool} (h : b = true) : (a || b) = true := by
      cases a <;> first | rfl | exact h

    theorem ww_deferred (w : WWorld) (n j : Nat) (h : deferredW w n = false) (hj : j < w.next)
        (hp : w.pend j = true) : w.nm j ≠ n := by
      intro e
      have a : anyN w.next (fun j => w.pend j && decide (w.nm j = n)) = true := by
        apply anyN_intro _ w.next j hj
        show (w.pend j && decide (w.nm j = n)) = true
        rw [hp, dtrue e]
      have t : deferredW w n = true := wl_or_l a
      exact ft (Eq.trans h.symm t)

    -- A pass that reads a name, no direct draft in flight writes it.
    theorem wl_deferred_held (w : WWorld) (n d : Nat) (h : deferredW w n = false) (hd : d < w.dnext)
        (hp : w.dphase d = 1 ∨ w.dphase d = 2) : w.dnm d ≠ n := by
      intro e
      have p : (decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true :=
        ww_or_cases hp (fun a => wl_or_l (dtrue a)) (fun b => wl_or_r (dtrue b))
      have a : heldW w n = true := by
        apply anyN_intro _ w.dnext d hd
        show ((decide (w.dphase d = 1) || decide (w.dphase d = 2)) && decide (w.dnm d = n)) = true
        rw [p, dtrue e]
      have t : deferredW w n = true := wl_or_r (wl_or_r a)
      exact ft (Eq.trans h.symm t)

    -- What the verdicts say, for exact kernels.
    theorem ww_vo3 (K : Kernels) (hK : Exact K) (d dn so tb : Bool) (h : verdictOf K d dn so tb = 3) :
        d = false ∧ dn = false ∧ so = true ∧ tb = false := by
      unfold verdictOf at h
      rw [hK.2.2.2.2.2.1 dn so tb, hK.2.2.2.2.2.2.1 dn so tb] at h
      revert h
      cases d <;> cases dn <;> cases so <;> cases tb <;> decide
    theorem ww_vo2 (K : Kernels) (hK : Exact K) (d dn so tb : Bool) (h : verdictOf K d dn so tb = 2) :
        d = false ∧ dn = false ∧ (so = false ∨ tb = true) := by
      unfold verdictOf at h
      rw [hK.2.2.2.2.2.1 dn so tb, hK.2.2.2.2.2.2.1 dn so tb] at h
      revert h
      cases d <;> cases dn <;> cases so <;> cases tb <;> decide

    theorem ww_pv_def (K : Kernels) (w : WWorld) (n : Nat) : passVerdict K w n =
        if w.waitOf n = 0 then 0
        else verdictOf K (deferredW w n) (decide (w.log n = w.val (w.waitOf n)))
          (w.bs (w.waitOf n) (w.log n)) (decide (w.val (w.waitOf n) = 0)) := rfl
    theorem ww_pv_wait (K : Kernels) (w : WWorld) (n : Nat) (h : passVerdict K w n ≠ 0) : w.waitOf n ≠ 0 :=
      fun hz => h (by rw [ww_pv_def, if_pos hz])
    theorem ww_pv3 (K : Kernels) (hK : Exact K) (w : WWorld) (n : Nat) (h : passVerdict K w n = 3) :
        w.waitOf n ≠ 0 ∧ deferredW w n = false ∧ w.log n ≠ w.val (w.waitOf n)
          ∧ w.bs (w.waitOf n) (w.log n) = true ∧ w.val (w.waitOf n) ≠ 0 := by
      have hw := ww_pv_wait K w n (by rw [h]; decide)
      rw [ww_pv_def, if_neg hw] at h
      have v := ww_vo3 K hK _ _ _ _ h
      exact ⟨hw, v.1, ww_df v.2.1, v.2.2.1, ww_df v.2.2.2⟩
    theorem ww_pv2 (K : Kernels) (hK : Exact K) (w : WWorld) (n : Nat) (h : passVerdict K w n = 2) :
        w.waitOf n ≠ 0 ∧ w.log n ≠ w.val (w.waitOf n)
          ∧ (w.bs (w.waitOf n) (w.log n) = false ∨ w.val (w.waitOf n) = 0) := by
      have hw := ww_pv_wait K w n (by rw [h]; decide)
      rw [ww_pv_def, if_neg hw] at h
      have v := ww_vo2 K hK _ _ _ _ h
      exact ⟨hw, ww_df v.2.1, Or.elim v.2.2 Or.inl (fun t => Or.inr (of_decide_eq_true t))⟩

    -- The invariant. Born ids are 1 .. next-1; a waiting write is born, of its
    -- name, not pending, earlier than every pending write of its name, and of a
    -- stored version; a refused version is never stored; the write a pass read
    -- is born and not pending, and while it still waits it is signed or
    -- overtaken; every born write is accounted for or dropped with its version
    -- refused for good; a write of this Mac that may still be signed stands over
    -- whatever value this Mac's own writes put at its name; an overtaken write
    -- was overtaken by another Mac.
    structure WaitInv (w : WWorld) : Prop where
      nextPos : 1 ≤ w.next
      pendBorn : ∀ i, w.pend i = true → 1 ≤ i ∧ i < w.next
      waitBorn : ∀ n, w.waitOf n ≠ 0 → w.waitOf n < w.next
      waitName : ∀ n, w.waitOf n ≠ 0 → w.nm (w.waitOf n) = n
      waitIdle : ∀ n, w.waitOf n ≠ 0 → w.pend (w.waitOf n) = false
      waitLatest : ∀ n j, w.waitOf n ≠ 0 → w.pend j = true → w.nm j = n → w.waitOf n < j
      waitStored : ∀ n, w.waitOf n ≠ 0 → w.stored (w.ver (w.waitOf n)) = true
      fateStored : ∀ i, w.fate i = 1 → i < w.next ∧ w.stored (w.ver i) = true
      refusedNot : ∀ v, w.refused v = true → w.stored v = false
      readBorn : ∀ n, w.passRead n ≠ 0 →
        w.passRead n < w.next ∧ w.pend (w.passRead n) = false ∧ w.nm (w.passRead n) = n
      readFate : ∀ n, w.passRead n ≠ 0 → w.waitOf n = w.passRead n →
        w.fate (w.passRead n) = 1 ∨ w.fate (w.passRead n) = 2
      acc : ∀ i, 1 ≤ i → i < w.next → Accounted w i ∨ (w.fate i = 4 ∧ w.refused (w.ver i) = true)
      ownBase : ∀ i, (w.pend i = true ∨ (i ≠ 0 ∧ w.waitOf (w.nm i) = i)) → w.holder (w.nm i) ≠ 0 →
        w.log (w.nm i) = w.val i ∨ (w.bs i (w.log (w.nm i)) = true ∧ w.val i ≠ 0)
      overtaken : ∀ i, w.fate i = 2 → w.overBy i = 0

    theorem ww_init (w : WWorld) (h : w = ww0) : WaitInv w := by
      subst h
      exact
        { nextPos := by show 1 ≤ 1; omega
          pendBorn := fun i h => ft h
          waitBorn := fun n h => absurd rfl h
          waitName := fun n h => absurd rfl h
          waitIdle := fun n h => absurd rfl h
          waitLatest := fun n j h => absurd rfl h
          waitStored := fun n h => absurd rfl h
          fateStored := fun i h => by have h' : (0 : Nat) = 1 := h; exact absurd h' (by decide)
          refusedNot := fun v h => ft h
          readBorn := fun n h => absurd rfl h
          readFate := fun n h => absurd rfl h
          acc := fun i h1 h2 => by have h2' : i < 1 := h2; exact absurd h1 (by omega)
          ownBase := fun i _ h => absurd rfl h
          overtaken := fun i h => rfl }

    theorem ww_pos_ne (a b : Nat) (h1 : 1 ≤ a) (h2 : a ≤ b) : b ≠ 0 := fun e => by rw [e] at h2; omega
    theorem ww_lt_ne (a b : Nat) (h1 : a ≤ b) (h2 : ¬ (b = a)) : a < b := by omega
    theorem ww_lt_of (a b : Nat) (h1 : ¬ (b < a)) (h2 : ¬ (b = a)) : a < b := by omega
    theorem ww_ne0 (i : Nat) (h : 1 ≤ i) : i ≠ 0 := by omega

    theorem ww_and_false (a : Bool) : (a && false) = false := by cases a <;> rfl
    theorem ww_and_and_false (a b : Bool) : (a && (b && false)) = false := by cases a <;> cases b <;> rfl
    theorem ww_upd_true (f : Nat → Bool) (k x : Nat) (h : f x = true) : upd f k true x = true := by
      show (if x = k then true else f x) = true
      by_cases e : x = k
      · rw [if_pos e]
      · rw [if_neg e]; exact h

    -- A guard that every pending write of a version S holds is stored.
    theorem ww_guard (w : WWorld) (S : Nat → Bool)
        (h : allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i)) = true) :
        ∀ i, i < w.next → w.pend i = true → S (w.ver i) = true → w.stored (w.ver i) = true := by
      intro i hi hp hs
      have t : (!(w.pend i && S (w.ver i)) || w.stored (w.ver i)) = true := allN_elim _ w.next h i hi
      rw [hp, hs] at t
      exact t

    -- landW, field by field.
    theorem ww_lw_pend (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (i : Nat)
        (h : (landW K S w).pend i = true) :
        w.pend i = true ∧ S (w.ver i) = false ∧ laterLands w S i = false := by
      have a : (w.pend i && K.keeps (S (w.ver i)) (laterLands w S i)) = true := h
      have b := ww_and a
      have c : (!(S (w.ver i)) && !(laterLands w S i)) = true := Eq.trans (hK.2.2.2.1 _ _).symm b.2
      have d := ww_nn_true c
      exact ⟨b.1, d.1, d.2⟩
    theorem ww_lw_pend_keep (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) (h : w.pend i = false) :
        (landW K S w).pend i = false := by
      show (w.pend i && K.keeps (S (w.ver i)) (laterLands w S i)) = false
      rw [h]
    theorem ww_lw_pend_land (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (i : Nat)
        (h : S (w.ver i) = true) : (landW K S w).pend i = false := by
      have k : K.keeps (S (w.ver i)) (laterLands w S i) = false := by
        rw [hK.2.2.2.1, h] <;> rfl
      show (w.pend i && K.keeps (S (w.ver i)) (laterLands w S i)) = false
      rw [k]
      exact ww_and_false _

    theorem ww_lw_wait (K : Kernels) (S : Nat → Bool) (w : WWorld) (n : Nat) :
        (landingOf w S n = 0 ∧ (landW K S w).waitOf n = w.waitOf n)
          ∨ (landingOf w S n ≠ 0 ∧ (landW K S w).waitOf n = landingOf w S n) := by
      by_cases hl : landingOf w S n = 0
      · exact Or.inl ⟨hl, show (if landingOf w S n = 0 then w.waitOf n else landingOf w S n) = w.waitOf n from if_pos hl⟩
      · exact Or.inr ⟨hl, show (if landingOf w S n = 0 then w.waitOf n else landingOf w S n) = landingOf w S n from if_neg hl⟩

    def ww_c1 (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) : Bool :=
      w.pend i && (!(K.keeps (S (w.ver i)) (laterLands w S i)) && !(decide (landingOf w S (w.nm i) = i)))
    def ww_c2 (S : Nat → Bool) (w : WWorld) (i : Nat) : Bool :=
      !(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i) && !(decide (landingOf w S (w.nm i) = 0)))
    theorem ww_lw_fate_def (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) :
        (landW K S w).fate i = if (ww_c1 K S w i || ww_c2 S w i) = true then 3 else w.fate i := rfl

    -- landW gives a write fate 3 only with a later born write of its name; else
    -- its fate stays, a pending write that leaves lands as the waiter, and a
    -- waiting write stays where no write of its name lands.
    theorem ww_lw_fate (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w) (i : Nat) :
        ((landW K S w).fate i = 3 ∧ ∃ j, i < j ∧ j < w.next ∧ w.nm j = w.nm i)
        ∨ ((landW K S w).fate i = w.fate i
            ∧ (w.pend i = true → K.keeps (S (w.ver i)) (laterLands w S i) = false → landingOf w S (w.nm i) = i)
            ∧ (i ≠ 0 → w.waitOf (w.nm i) = i → landingOf w S (w.nm i) = 0)) := by
      cases h1 : ww_c1 K S w i with
      | true =>
        left
        refine ⟨by rw [ww_lw_fate_def, h1] <;> rfl, ?_⟩
        have a : (w.pend i && (!(K.keeps (S (w.ver i)) (laterLands w S i))
            && !(decide (landingOf w S (w.nm i) = i)))) = true := h1
        have b := ww_and a
        have c := ww_and b.2
        have hk := ww_not c.1
        have hne := ww_df (ww_not c.2)
        have hor := ww_nn (Eq.trans (hK.2.2.2.1 _ _).symm hk)
        have hib := hi.pendBorn i b.1
        have hib1 := hib.1
        cases hor with
        | inl hs =>
          have hge := ww_landing_ge w S i b.1 hs hib.2
          have hl0 : landingOf w S (w.nm i) ≠ 0 := ww_pos_ne i _ hib1 hge
          have r := ww_landing w S (w.nm i) hl0
          exact ⟨landingOf w S (w.nm i), ww_lt_ne i _ hge hne, r.2.2.2, r.2.2.1⟩
        | inr hl =>
          cases ww_later_elim w S i hl with
          | intro j hj => exact ⟨j, hj.2.1, hj.1, hj.2.2.2.2⟩
      | false =>
        cases h2 : ww_c2 S w i with
        | true =>
          left
          refine ⟨by rw [ww_lw_fate_def, h1, h2] <;> rfl, ?_⟩
          have a : (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
              && !(decide (landingOf w S (w.nm i) = 0)))) = true := h2
          have b := ww_and a
          have c := ww_and b.2
          have hi0 := ww_df (ww_not b.1)
          have hw := of_decide_eq_true c.1
          have hl0 := ww_df (ww_not c.2)
          have r := ww_landing w S (w.nm i) hl0
          have hz : w.waitOf (w.nm i) ≠ 0 := by rw [hw]; exact hi0
          have lt := hi.waitLatest (w.nm i) (landingOf w S (w.nm i)) hz r.1 r.2.2.1
          rw [hw] at lt
          exact ⟨landingOf w S (w.nm i), lt, r.2.2.2, r.2.2.1⟩
        | false =>
          right
          refine ⟨by rw [ww_lw_fate_def, h1, h2] <;> rfl, ?_, ?_⟩
          · intro hp hk
            by_cases e : landingOf w S (w.nm i) = i
            · exact e
            · have t : ww_c1 K S w i = true := by
                show (w.pend i && (!(K.keeps (S (w.ver i)) (laterLands w S i))
                  && !(decide (landingOf w S (w.nm i) = i)))) = true
                rw [hp, hk, dfalse e]
              exact ft (Eq.trans h1.symm t)
          · intro hi0 hw
            by_cases e : landingOf w S (w.nm i) = 0
            · exact e
            · have t : ww_c2 S w i = true := by
                show (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
                  && !(decide (landingOf w S (w.nm i) = 0)))) = true
                rw [dfalse hi0, dtrue hw, dfalse e]
              exact ft (Eq.trans h2.symm t)

    theorem ww_lw_fate_same (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) (hp : w.pend i = false)
        (hl : landingOf w S (w.nm i) = 0) : (landW K S w).fate i = w.fate i := by
      have e1 : ww_c1 K S w i = false := by
        show (w.pend i && (!(K.keeps (S (w.ver i)) (laterLands w S i))
          && !(decide (landingOf w S (w.nm i) = i)))) = false
        rw [hp]
      have e2 : ww_c2 S w i = false := by
        show (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
          && !(decide (landingOf w S (w.nm i) = 0)))) = false
        rw [hl]
        exact ww_and_and_false _ _
      rw [ww_lw_fate_def, e1, e2] <;> rfl

    theorem ww_lw_read_stay (K : Kernels) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w) (n : Nat)
        (h : w.passRead n ≠ 0) (hw : (landW K S w).waitOf n = w.passRead n)
        (c : landingOf w S n = 0 ∧ (landW K S w).waitOf n = w.waitOf n) :
        (landW K S w).fate (w.passRead n) = 1 ∨ (landW K S w).fate (w.passRead n) = 2 := by
      have r := hi.readBorn n h
      have hw2 : w.waitOf n = w.passRead n := Eq.trans c.2.symm hw
      have hl : landingOf w S (w.nm (w.passRead n)) = 0 := by rw [r.2.2]; exact c.1
      rw [ww_lw_fate_same K S w (w.passRead n) r.2.1 hl]
      exact hi.readFate n h hw2
    theorem ww_lw_read_lands (K : Kernels) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w) (n : Nat)
        (h : w.passRead n ≠ 0) (hw : (landW K S w).waitOf n = w.passRead n)
        (d : landingOf w S n ≠ 0 ∧ (landW K S w).waitOf n = landingOf w S n) : False := by
      have r := hi.readBorn n h
      have hw3 : landingOf w S n = w.passRead n := Eq.trans d.2.symm hw
      have p := (ww_landing w S n d.1).1
      rw [hw3] at p
      exact ft (Eq.trans r.2.1.symm p)
    theorem ww_lw_read (K : Kernels) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w) (n : Nat)
        (h : w.passRead n ≠ 0) (hw : (landW K S w).waitOf n = w.passRead n) :
        (landW K S w).fate (w.passRead n) = 1 ∨ (landW K S w).fate (w.passRead n) = 2 :=
      ww_or_cases (ww_lw_wait K S w n) (fun c => ww_lw_read_stay K S w hi n h hw c)
        (fun d => absurd (ww_lw_read_lands K S w hi n h hw d) (fun x => x))

    -- Landing the sets S holds keeps the invariant, where each pending write of a
    -- version S holds is stored (the land guard; a publish's opened check).
    theorem ww_landW (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w)
        (hst : ∀ i, i < w.next → w.pend i = true → S (w.ver i) = true → w.stored (w.ver i) = true) :
        WaitInv (landW K S w) :=
      { nextPos := hi.nextPos
        pendBorn := fun i h => hi.pendBorn i (ww_lw_pend K hK S w i h).1
        waitBorn := fun n h => ww_or_cases (ww_lw_wait K S w n)
          (fun c => by rw [c.2] at h; rw [c.2]; exact hi.waitBorn n h)
          (fun c => by rw [c.2]; exact (ww_landing w S n c.1).2.2.2)
        waitName := fun n h => ww_or_cases (ww_lw_wait K S w n)
          (fun c => by rw [c.2] at h; rw [c.2]; exact hi.waitName n h)
          (fun c => by rw [c.2]; exact (ww_landing w S n c.1).2.2.1)
        waitIdle := fun n h => ww_or_cases (ww_lw_wait K S w n)
          (fun c => by rw [c.2] at h; rw [c.2]; exact ww_lw_pend_keep K S w _ (hi.waitIdle n h))
          (fun c => by rw [c.2]; exact ww_lw_pend_land K hK S w _ (ww_landing w S n c.1).2.1)
        waitLatest := fun n j h hpj hnj => by
          have pj := ww_lw_pend K hK S w j hpj
          exact ww_or_cases (ww_lw_wait K S w n)
            (fun c => by rw [c.2] at h; rw [c.2]; exact hi.waitLatest n j h pj.1 hnj)
            (fun c => by
              rw [c.2]
              have r := ww_landing w S n c.1
              by_cases hjl : j < landingOf w S n
              · have t := ww_later_intro w S j (landingOf w S n) r.2.2.2 hjl r.1 r.2.1 (Eq.trans r.2.2.1 hnj.symm)
                exact ft (Eq.trans pj.2.2.symm t)
              · by_cases hje : j = landingOf w S n
                · have s := pj.2.1
                  rw [hje] at s
                  exact ft (Eq.trans s.symm r.2.1)
                · exact ww_lt_of _ _ hjl hje)
        waitStored := fun n h => ww_or_cases (ww_lw_wait K S w n)
          (fun c => by rw [c.2] at h; rw [c.2]; exact hi.waitStored n h)
          (fun c => by
            rw [c.2]
            have r := ww_landing w S n c.1
            exact hst _ r.2.2.2 r.1 r.2.1)
        fateStored := fun i h => ww_or_cases (ww_lw_fate K hK S w hi i)
          (fun c => by rw [c.1] at h; exact absurd h (by decide))
          (fun c => by rw [c.1] at h; exact hi.fateStored i h)
        refusedNot := hi.refusedNot
        readBorn := fun n h => by
          have r := hi.readBorn n h
          exact ⟨r.1, ww_lw_pend_keep K S w _ r.2.1, r.2.2⟩
        readFate := fun n h hw => ww_lw_read K S w hi n h hw
        acc := fun i h1 h2 => ww_or_cases (ww_lw_fate K hK S w hi i)
          (fun c => Or.inl (Or.inr (Or.inr (Or.inr (Or.inr ⟨c.1, c.2⟩)))))
          (fun c => by
            have hf := c.1
            exact ww_or_cases (hi.acc i h1 h2) (fun a => by
              left
              rcases a with hp | hw | f1 | f2 | f3
              · cases hq : (landW K S w).pend i with
                | true => exact Or.inl hq
                | false =>
                  have hk : K.keeps (S (w.ver i)) (laterLands w S i) = false := by
                    have t : (w.pend i && K.keeps (S (w.ver i)) (laterLands w S i)) = false := hq
                    rw [hp] at t
                    exact t
                  have hl := c.2.1 hp hk
                  right; left
                  exact ww_or_cases (ww_lw_wait K S w (w.nm i))
                    (fun d => absurd (Eq.trans hl.symm d.1) (ww_ne0 i h1))
                    (fun d => Eq.trans d.2 hl)
              · right; left
                exact ww_or_cases (ww_lw_wait K S w (w.nm i))
                  (fun d => Eq.trans d.2 hw)
                  (fun d => absurd (c.2.2 (ww_ne0 i h1) hw) d.1)
              · right; right; left; rw [hf]; exact f1
              · right; right; right; left; rw [hf]; exact f2
              · right; right; right; right; rw [hf]; exact f3)
              (fun d => by right; rw [hf]; exact d))
        ownBase := fun i hh hne => by
          have old : w.pend i = true ∨ (i ≠ 0 ∧ w.waitOf (w.nm i) = i) :=
            ww_or_cases hh (fun hp => Or.inl (ww_lw_pend K hK S w i hp).1)
              (fun hw => ww_or_cases (ww_lw_wait K S w (w.nm i))
                (fun d => Or.inr ⟨hw.1, Eq.trans d.2.symm hw.2⟩)
                (fun d => by
                  have r := ww_landing w S (w.nm i) d.1
                  have e : landingOf w S (w.nm i) = i := Eq.trans d.2.symm hw.2
                  have p := r.1
                  rw [e] at p
                  exact Or.inl p))
          exact hi.ownBase i old hne
        overtaken := fun i h => ww_or_cases (ww_lw_fate K hK S w hi i)
          (fun c => by rw [c.1] at h; exact absurd h (by decide))
          (fun c => by rw [c.1] at h; exact hi.overtaken i h) }

    -- pushW: born writes keep every field; the k new ones are pending.
    theorem ww_push_old (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (i : Nat)
        (h : ¬ (w.next ≤ i ∧ i < w.next + k)) :
        (pushW w v k names vals bases).nm i = w.nm i ∧ (pushW w v k names vals bases).val i = w.val i
          ∧ (pushW w v k names vals bases).bs i = w.bs i ∧ (pushW w v k names vals bases).ver i = w.ver i
          ∧ (pushW w v k names vals bases).pend i = w.pend i :=
      ⟨if_neg h, if_neg h, if_neg h, if_neg h, if_neg h⟩
    theorem ww_push_new (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (i : Nat)
        (h : w.next ≤ i ∧ i < w.next + k) :
        (pushW w v k names vals bases).nm i = names (i - w.next) ∧ (pushW w v k names vals bases).val i = vals (i - w.next)
          ∧ (pushW w v k names vals bases).bs i = bases (i - w.next) ∧ (pushW w v k names vals bases).pend i = true :=
      ⟨if_pos h, if_pos h, if_pos h, if_pos h⟩
    theorem ww_not_in (w : WWorld) (k i : Nat) (h : i < w.next) : ¬ (w.next ≤ i ∧ i < w.next + k) :=
      fun hr => absurd hr.1 (by omega)
    theorem ww_le_add (a b c : Nat) (h : a < b) : a < b + c := by omega
    theorem ww_one_le (a b : Nat) (h : 1 ≤ a) : 1 ≤ a + b := by omega
    theorem ww_lt_le (a b c : Nat) (h1 : a < b) (h2 : b ≤ c) : a < c := by omega
    theorem ww_le_tr (a b c : Nat) (h1 : a ≤ b) (h2 : b ≤ c) : a ≤ c := by omega
    theorem ww_zero_add : ∀ (a : Nat), 0 + a = a := by
      intro a
      induction a with
      | zero => rfl
      | succ a ih => show Nat.succ (0 + a) = Nat.succ a; rw [ih]
    theorem ww_succ_add (s : Nat) : ∀ (a : Nat), Nat.succ s + a = s + Nat.succ a := by
      intro a
      induction a with
      | zero => rfl
      | succ a ih => show Nat.succ (Nat.succ s + a) = Nat.succ (s + Nat.succ a); rw [ih]
    theorem ww_not_succ_le (a : Nat) : ¬ (Nat.succ a ≤ a) := by omega
    theorem ww_pred_add (t a i : Nat) (e : t + a = i) (h : Nat.succ a ≤ i) : Nat.pred t + Nat.succ a = i := by
      cases t with
      | zero =>
        have e2 : a = i := Eq.trans (ww_zero_add a).symm e
        rw [← e2] at h
        exact absurd h (ww_not_succ_le a)
      | succ s => exact Eq.trans (ww_succ_add s a).symm e
    theorem ww_sub_add : ∀ (a i : Nat), a ≤ i → i - a + a = i := by
      intro a
      induction a with
      | zero => intro i _; rfl
      | succ a ih =>
        intro i h
        have e := ih i (by omega)
        have hp : i - Nat.succ a = Nat.pred (i - a) := rfl
        rw [hp]
        exact ww_pred_add (i - a) a i e h
    theorem ww_lt_of_add (t a i k : Nat) (e : t + a = i) (h : i < a + k) : t < k := by omega
    theorem ww_sub_lt (a i k : Nat) (h1 : a ≤ i) (h2 : i < a + k) : i - a < k :=
      ww_lt_of_add (i - a) a i k (ww_sub_add a i h1) h2
    theorem ww_below (w : WWorld) (k i : Nat) (h2 : i < w.next + k) (hr : ¬ (w.next ≤ i ∧ i < w.next + k)) :
        i < w.next := by
      by_cases hx : i < w.next
      · exact hx
      · exact absurd ⟨by omega, h2⟩ hr

    theorem ww_push_own_old (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool)
        (hi : WaitInv w) (i : Nat) (hr : ¬ (w.next ≤ i ∧ i < w.next + k))
        (hh : (pushW w v k names vals bases).pend i = true
          ∨ (i ≠ 0 ∧ (pushW w v k names vals bases).waitOf ((pushW w v k names vals bases).nm i) = i))
        (hne : (pushW w v k names vals bases).holder ((pushW w v k names vals bases).nm i) ≠ 0) :
        (pushW w v k names vals bases).log ((pushW w v k names vals bases).nm i) = (pushW w v k names vals bases).val i
          ∨ ((pushW w v k names vals bases).bs i ((pushW w v k names vals bases).log ((pushW w v k names vals bases).nm i)) = true
              ∧ (pushW w v k names vals bases).val i ≠ 0) := by
      have q := ww_push_old w v k names vals bases i hr
      have hh' : w.pend i = true ∨ (i ≠ 0 ∧ w.waitOf (w.nm i) = i) :=
        ww_or_cases hh (fun hp => Or.inl (Eq.trans q.2.2.2.2.symm hp))
          (fun hw => Or.inr ⟨hw.1, by rw [← q.1]; exact hw.2⟩)
      have hne' : w.holder (w.nm i) ≠ 0 := by rw [← q.1]; exact hne
      show w.log ((pushW w v k names vals bases).nm i) = (pushW w v k names vals bases).val i
        ∨ ((pushW w v k names vals bases).bs i (w.log ((pushW w v k names vals bases).nm i)) = true
            ∧ (pushW w v k names vals bases).val i ≠ 0)
      rw [q.1, q.2.1, q.2.2.1]
      exact hi.ownBase i hh' hne'

    -- A publish's push keeps the invariant: each new write's bases hold the log's
    -- value at its name, and a new take-back is drafted only where the log holds none.
    theorem ww_pushW (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (hi : WaitInv w)
        (hb : ∀ a, a < k → bases a (w.log (names a)) = true)
        (ht : ∀ a, a < k → vals a = 0 → w.log (names a) = 0) :
        WaitInv (pushW w v k names vals bases) :=
      { nextPos := ww_one_le _ k hi.nextPos
        pendBorn := fun i h => by
          by_cases hr : w.next ≤ i ∧ i < w.next + k
          · exact ⟨ww_le_tr _ _ _ hi.nextPos hr.1, hr.2⟩
          · have e := (ww_push_old w v k names vals bases i hr).2.2.2.2
            rw [e] at h
            have r := hi.pendBorn i h
            exact ⟨r.1, ww_le_add _ _ k r.2⟩
        waitBorn := fun n h => ww_le_add _ _ k (hi.waitBorn n h)
        waitName := fun n h =>
          Eq.trans (ww_push_old w v k names vals bases _ (ww_not_in w k _ (hi.waitBorn n h))).1 (hi.waitName n h)
        waitIdle := fun n h =>
          Eq.trans (ww_push_old w v k names vals bases _ (ww_not_in w k _ (hi.waitBorn n h))).2.2.2.2 (hi.waitIdle n h)
        waitLatest := fun n j h hpj hnj => by
          by_cases hr : w.next ≤ j ∧ j < w.next + k
          · exact ww_lt_le _ _ _ (hi.waitBorn n h) hr.1
          · have o := ww_push_old w v k names vals bases j hr
            exact hi.waitLatest n j h (Eq.trans o.2.2.2.2.symm hpj) (Eq.trans o.1.symm hnj)
        waitStored := fun n h => by
          have o := ww_push_old w v k names vals bases _ (ww_not_in w k _ (hi.waitBorn n h))
          show w.stored ((pushW w v k names vals bases).ver (w.waitOf n)) = true
          rw [o.2.2.2.1]
          exact hi.waitStored n h
        fateStored := fun i h => by
          have r := hi.fateStored i h
          have o := ww_push_old w v k names vals bases i (ww_not_in w k i r.1)
          refine ⟨ww_le_add _ _ k r.1, ?_⟩
          show w.stored ((pushW w v k names vals bases).ver i) = true
          rw [o.2.2.2.1]
          exact r.2
        refusedNot := hi.refusedNot
        readBorn := fun n h => by
          have r := hi.readBorn n h
          have o := ww_push_old w v k names vals bases _ (ww_not_in w k _ r.1)
          exact ⟨ww_le_add _ _ k r.1, Eq.trans o.2.2.2.2 r.2.1, Eq.trans o.1 r.2.2⟩
        readFate := hi.readFate
        acc := fun i h1 h2 => by
          by_cases hr : w.next ≤ i ∧ i < w.next + k
          · exact Or.inl (Or.inl (ww_push_new w v k names vals bases i hr).2.2.2)
          · have hlt := ww_below w k i h2 hr
            have o := ww_push_old w v k names vals bases i hr
            exact ww_or_cases (hi.acc i h1 hlt)
              (fun a => by
                left
                rcases a with hp | hw | f1 | f2 | f3
                · exact Or.inl (Eq.trans o.2.2.2.2 hp)
                · right; left
                  show w.waitOf ((pushW w v k names vals bases).nm i) = i
                  rw [o.1]
                  exact hw
                · exact Or.inr (Or.inr (Or.inl f1))
                · exact Or.inr (Or.inr (Or.inr (Or.inl f2)))
                · have f : w.fate i = 3 ∧ (∃ j, i < j ∧ j < w.next ∧ w.nm j = w.nm i) := f3
                  exact Exists.elim f.2 (fun j hj =>
                    Or.inr (Or.inr (Or.inr (Or.inr ⟨f.1, j, hj.1, ww_le_add _ _ k hj.2.1,
                      Eq.trans (ww_push_old w v k names vals bases j (ww_not_in w k j hj.2.1)).1
                        (Eq.trans hj.2.2 o.1.symm)⟩)))))
              (fun d => by
                right
                refine ⟨d.1, ?_⟩
                show w.refused ((pushW w v k names vals bases).ver i) = true
                rw [o.2.2.2.1]
                exact d.2)
        ownBase := fun i hh hne => by
          by_cases hr : w.next ≤ i ∧ i < w.next + k
          · have o := ww_push_new w v k names vals bases i hr
            have ha : i - w.next < k := ww_sub_lt _ _ _ hr.1 hr.2
            show w.log ((pushW w v k names vals bases).nm i) = (pushW w v k names vals bases).val i
              ∨ ((pushW w v k names vals bases).bs i (w.log ((pushW w v k names vals bases).nm i)) = true
                  ∧ (pushW w v k names vals bases).val i ≠ 0)
            rw [o.1, o.2.1, o.2.2.1]
            by_cases hv : vals (i - w.next) = 0
            · left
              rw [hv]
              exact ht _ ha hv
            · exact Or.inr ⟨hb _ ha, hv⟩
          · exact ww_push_own_old w v k names vals bases hi i hr hh hne
        overtaken := hi.overtaken }

    -- settleW: a waiting write goes only as the write the pass read.
    theorem ww_settle_wait (w : WWorld) (n : Nat) :
        (settleW w).waitOf n = w.waitOf n
          ∨ ((settleW w).waitOf n = 0 ∧ w.passRead n ≠ 0 ∧ w.waitOf n = w.passRead n) :=
      ww_by_cases (w.passRead n = 0)
        (fun hc => Or.inl (by
          show (if (!(decide (w.passRead n = 0)) && decide (w.waitOf n = w.passRead n)) = true
            then 0 else w.waitOf n) = w.waitOf n
          rw [dtrue hc] <;> rfl))
        (fun hc => ww_by_cases (w.waitOf n = w.passRead n)
          (fun hw => Or.inr ⟨by
            show (if (!(decide (w.passRead n = 0)) && decide (w.waitOf n = w.passRead n)) = true
              then 0 else w.waitOf n) = 0
            rw [dfalse hc, dtrue hw] <;> rfl, hc, hw⟩)
          (fun hw => Or.inl (by
            show (if (!(decide (w.passRead n = 0)) && decide (w.waitOf n = w.passRead n)) = true
              then 0 else w.waitOf n) = w.waitOf n
            rw [dfalse hc, dfalse hw] <;> rfl)))

    theorem ww_settle_gone (w : WWorld) (hi : WaitInv w) (i : Nat) (hw : w.waitOf (w.nm i) = i)
        (c : (settleW w).waitOf (w.nm i) = 0 ∧ w.passRead (w.nm i) ≠ 0 ∧ w.waitOf (w.nm i) = w.passRead (w.nm i)) :
        Accounted (settleW w) i := by
      have e : w.passRead (w.nm i) = i := Eq.trans c.2.2.symm hw
      have f := hi.readFate (w.nm i) c.2.1 c.2.2
      rw [e] at f
      exact ww_or_cases f (fun f1 => Or.inr (Or.inr (Or.inl f1))) (fun f2 => Or.inr (Or.inr (Or.inr (Or.inl f2))))

    theorem ww_settle_acc (w : WWorld) (hi : WaitInv w) (i : Nat) (a : Accounted w i) : Accounted (settleW w) i :=
      ww_or_cases a (fun hp => Or.inl hp) (fun r1 =>
        ww_or_cases r1
          (fun hw => ww_or_cases (ww_settle_wait w (w.nm i))
            (fun c => Or.inr (Or.inl (Eq.trans c hw)))
            (fun c => ww_settle_gone w hi i hw c))
          (fun r2 => Or.inr (Or.inr r2)))

    theorem ww_settleW (w : WWorld) (hi : WaitInv w) : WaitInv (settleW w) :=
      { nextPos := hi.nextPos
        pendBorn := hi.pendBorn
        waitBorn := fun n h => ww_or_cases (ww_settle_wait w n)
          (fun c => by rw [c] at h; rw [c]; exact hi.waitBorn n h)
          (fun c => absurd c.1 h)
        waitName := fun n h => ww_or_cases (ww_settle_wait w n)
          (fun c => by rw [c] at h; rw [c]; exact hi.waitName n h)
          (fun c => absurd c.1 h)
        waitIdle := fun n h => ww_or_cases (ww_settle_wait w n)
          (fun c => by rw [c] at h; rw [c]; exact hi.waitIdle n h)
          (fun c => absurd c.1 h)
        waitLatest := fun n j h hp hn => ww_or_cases (ww_settle_wait w n)
          (fun c => by rw [c] at h; rw [c]; exact hi.waitLatest n j h hp hn)
          (fun c => absurd c.1 h)
        waitStored := fun n h => ww_or_cases (ww_settle_wait w n)
          (fun c => by rw [c] at h; rw [c]; exact hi.waitStored n h)
          (fun c => absurd c.1 h)
        fateStored := hi.fateStored
        refusedNot := hi.refusedNot
        readBorn := fun n h => absurd rfl h
        readFate := fun n h => absurd rfl h
        acc := fun i h1 h2 => ww_or_cases (hi.acc i h1 h2)
          (fun a => Or.inl (ww_settle_acc w hi i a))
          (fun d => Or.inr d)
        ownBase := fun i hh hne => hi.ownBase i
          (ww_or_cases hh Or.inl (fun hw => Or.inr ⟨hw.1, ww_or_cases (ww_settle_wait w (w.nm i))
            (fun c => Eq.trans c.symm hw.2)
            (fun c => absurd (Eq.trans c.1.symm hw.2) (fun e => hw.1 e.symm))⟩)) hne
        overtaken := hi.overtaken }

    -- signBody, field by field.
    theorem ww_sb_fate_def (K : Kernels) (w : WWorld) (i : Nat) : (signBody K w).fate i =
        if (!(decide (i = 0)) && decide (w.waitOf (w.nm i) = i)) = true then
          (if passVerdict K w (w.nm i) = 0 then w.fate i else if passVerdict K w (w.nm i) = 2 then 2 else 1)
        else w.fate i := rfl
    theorem ww_sb_over_def (K : Kernels) (w : WWorld) (i : Nat) : (signBody K w).overBy i =
        if (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i) && decide (passVerdict K w (w.nm i) = 2))) = true
        then w.holder (w.nm i) else w.overBy i := rfl
    theorem ww_sb_log_def (K : Kernels) (w : WWorld) (n : Nat) :
        (signBody K w).log n = if passVerdict K w n = 3 then w.val (w.waitOf n) else w.log n := rfl
    theorem ww_sb_holder_def (K : Kernels) (w : WWorld) (n : Nat) :
        (signBody K w).holder n = if passVerdict K w n = 3 then w.waitOf n else w.holder n := rfl
    theorem ww_sb_read_def (K : Kernels) (w : WWorld) (n : Nat) :
        (signBody K w).passRead n = if passVerdict K w n = 0 then 0 else w.waitOf n := rfl

    theorem ww_sb_fate_other (K : Kernels) (w : WWorld) (i : Nat) (h : ¬ (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)) :
        (signBody K w).fate i = w.fate i := by
      rw [ww_sb_fate_def]
      apply if_neg
      intro hc
      have a := ww_and hc
      exact h ⟨ww_df (ww_not a.1), of_decide_eq_true a.2⟩
    theorem ww_sb_fate_wait (K : Kernels) (w : WWorld) (i : Nat) (hz : ¬ (i = 0)) (hw : w.waitOf (w.nm i) = i) :
        (signBody K w).fate i =
          (if passVerdict K w (w.nm i) = 0 then w.fate i else if passVerdict K w (w.nm i) = 2 then 2 else 1) := by
      rw [ww_sb_fate_def, dfalse hz, dtrue hw] <;> rfl

    theorem ww_sb_fs_wait (w : WWorld) (hi : WaitInv w) (i : Nat) (hc : ¬ (i = 0) ∧ w.waitOf (w.nm i) = i) :
        i < w.next ∧ w.stored (w.ver i) = true := by
      have hz : w.waitOf (w.nm i) ≠ 0 := by rw [hc.2]; exact hc.1
      have s := hi.waitStored (w.nm i) hz
      have b := hi.waitBorn (w.nm i) hz
      refine ⟨?_, ?_⟩
      · rw [← hc.2]; exact b
      · rw [← hc.2]; exact s

    theorem ww_sb_fateStored (K : Kernels) (w : WWorld) (hi : WaitInv w) (i : Nat) (h : (signBody K w).fate i = 1) :
        i < w.next ∧ w.stored (w.ver i) = true :=
      ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
        (fun hc => ww_sb_fs_wait w hi i hc)
        (fun hc => hi.fateStored i (Eq.trans (ww_sb_fate_other K w i hc).symm h))

    theorem ww_sb_readBorn (K : Kernels) (w : WWorld) (hi : WaitInv w) (n : Nat) (h : (signBody K w).passRead n ≠ 0) :
        (signBody K w).passRead n < w.next ∧ w.pend ((signBody K w).passRead n) = false
          ∧ w.nm ((signBody K w).passRead n) = n :=
      ww_by_cases (passVerdict K w n = 0)
        (fun h0 => absurd (Eq.trans (ww_sb_read_def K w n) (if_pos h0)) h)
        (fun h0 => by
          have e : (signBody K w).passRead n = w.waitOf n := Eq.trans (ww_sb_read_def K w n) (if_neg h0)
          have hw : w.waitOf n ≠ 0 := ww_pv_wait K w n h0
          rw [e]
          exact ⟨hi.waitBorn n hw, hi.waitIdle n hw, hi.waitName n hw⟩)

    theorem ww_sb_readFate (K : Kernels) (w : WWorld) (hi : WaitInv w) (n : Nat) (h : (signBody K w).passRead n ≠ 0) :
        (signBody K w).fate ((signBody K w).passRead n) = 1 ∨ (signBody K w).fate ((signBody K w).passRead n) = 2 :=
      ww_by_cases (passVerdict K w n = 0)
        (fun h0 => absurd (Eq.trans (ww_sb_read_def K w n) (if_pos h0)) h)
        (fun h0 => by
          have e : (signBody K w).passRead n = w.waitOf n := Eq.trans (ww_sb_read_def K w n) (if_neg h0)
          have hw : w.waitOf n ≠ 0 := ww_pv_wait K w n h0
          have hn : w.nm (w.waitOf n) = n := hi.waitName n hw
          have hf := ww_sb_fate_wait K w (w.waitOf n) hw (by rw [hn])
          rw [e, hf, hn, if_neg h0]
          exact ww_by_cases (passVerdict K w n = 2)
            (fun h2 => by rw [if_pos h2]; exact Or.inr rfl)
            (fun h2 => by rw [if_neg h2]; exact Or.inl rfl))

    theorem ww_sb_acc (K : Kernels) (w : WWorld) (hi : WaitInv w) (i : Nat) (h1 : 1 ≤ i) (h2 : i < w.next) :
        Accounted (signBody K w) i ∨ ((signBody K w).fate i = 4 ∧ w.refused (w.ver i) = true) :=
      ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
        (fun hc => Or.inl (Or.inr (Or.inl hc.2)))
        (fun hc => by
          have hf : (signBody K w).fate i = w.fate i := ww_sb_fate_other K w i hc
          exact ww_or_cases (hi.acc i h1 h2)
            (fun a => Or.inl (by unfold Accounted; rw [hf]; exact a))
            (fun d => Or.inr (by rw [hf]; exact d)))

    theorem ww_sb_own (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (i : Nat)
        (hh : w.pend i = true ∨ (i ≠ 0 ∧ w.waitOf (w.nm i) = i)) (hne : (signBody K w).holder (w.nm i) ≠ 0) :
        (signBody K w).log (w.nm i) = w.val i ∨ (w.bs i ((signBody K w).log (w.nm i)) = true ∧ w.val i ≠ 0) :=
      ww_by_cases (passVerdict K w (w.nm i) = 3)
        (fun h3 => by
          have p := ww_pv3 K hK w (w.nm i) h3
          have hw : i ≠ 0 ∧ w.waitOf (w.nm i) = i := ww_or_cases hh
            (fun hp => absurd rfl (ww_deferred w (w.nm i) i p.2.1 (hi.pendBorn i hp).2 hp))
            (fun hw => hw)
          left
          show (if passVerdict K w (w.nm i) = 3 then w.val (w.waitOf (w.nm i)) else w.log (w.nm i)) = w.val i
          rw [if_pos h3, hw.2])
        (fun h3 => by
          have el : (signBody K w).log (w.nm i) = w.log (w.nm i) := Eq.trans (ww_sb_log_def K w (w.nm i)) (if_neg h3)
          have eh : (signBody K w).holder (w.nm i) = w.holder (w.nm i) :=
            Eq.trans (ww_sb_holder_def K w (w.nm i)) (if_neg h3)
          have hne' : w.holder (w.nm i) ≠ 0 := fun z => hne (Eq.trans eh z)
          rw [el]
          exact hi.ownBase i hh hne')

    -- A verdict of overtaken on this Mac's waiting write finds another Mac's value.
    theorem ww_holder_zero (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (i : Nat)
        (hc : ¬ (i = 0) ∧ w.waitOf (w.nm i) = i) (h2 : passVerdict K w (w.nm i) = 2) : w.holder (w.nm i) = 0 := by
      have p := ww_pv2 K hK w (w.nm i) h2
      rw [hc.2] at p
      exact ww_by_cases (w.holder (w.nm i) = 0) (fun hz => hz) (fun hz =>
        absurd (hi.ownBase i (Or.inr hc) hz) (fun ob => ww_or_cases ob
          (fun hd => p.2.1 hd)
          (fun hb => ww_or_cases p.2.2 (fun hf => ft (Eq.trans hf.symm hb.1)) (fun hv => hb.2 hv))))

    theorem ww_sb_fate_w0 (K : Kernels) (w : WWorld) (i : Nat) (hz : ¬ (i = 0)) (hw : w.waitOf (w.nm i) = i)
        (h0 : passVerdict K w (w.nm i) = 0) : (signBody K w).fate i = w.fate i := by
      rw [ww_sb_fate_def, dfalse hz, dtrue hw, if_pos h0] <;> rfl
    theorem ww_sb_fate_w2 (K : Kernels) (w : WWorld) (i : Nat) (hz : ¬ (i = 0)) (hw : w.waitOf (w.nm i) = i)
        (h0 : ¬ (passVerdict K w (w.nm i) = 0)) (h2 : passVerdict K w (w.nm i) = 2) : (signBody K w).fate i = 2 := by
      rw [ww_sb_fate_def, dfalse hz, dtrue hw, if_neg h0, if_pos h2] <;> rfl
    theorem ww_sb_fate_w1 (K : Kernels) (w : WWorld) (i : Nat) (hz : ¬ (i = 0)) (hw : w.waitOf (w.nm i) = i)
        (h0 : ¬ (passVerdict K w (w.nm i) = 0)) (h2 : ¬ (passVerdict K w (w.nm i) = 2)) : (signBody K w).fate i = 1 := by
      rw [ww_sb_fate_def, dfalse hz, dtrue hw, if_neg h0, if_neg h2] <;> rfl

    theorem ww_sb_over_other (K : Kernels) (w : WWorld) (i : Nat) (h : ¬ (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)) :
        (signBody K w).overBy i = w.overBy i := by
      rw [ww_sb_over_def]
      apply if_neg
      intro hb
      have a := ww_and hb
      have b := ww_and a.2
      exact h ⟨ww_df (ww_not a.1), of_decide_eq_true b.1⟩

    theorem ww_sb_over (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (i : Nat)
        (h : (signBody K w).fate i = 2) : (signBody K w).overBy i = 0 :=
      ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
        (fun hc => ww_by_cases (passVerdict K w (w.nm i) = 0)
          (fun h0 => by
            have hf : w.fate i = 2 := Eq.trans (ww_sb_fate_w0 K w i hc.1 hc.2 h0).symm h
            have n2 : ¬ (passVerdict K w (w.nm i) = 2) := fun e2 => absurd (Eq.trans h0.symm e2) (by decide)
            have e : (signBody K w).overBy i = w.overBy i := by
              rw [ww_sb_over_def, dfalse hc.1, dtrue hc.2, dfalse n2] <;> rfl
            rw [e]
            exact hi.overtaken i hf)
          (fun h0 => ww_by_cases (passVerdict K w (w.nm i) = 2)
            (fun h2 => by
              have e : (signBody K w).overBy i = w.holder (w.nm i) := by
                rw [ww_sb_over_def, dfalse hc.1, dtrue hc.2, dtrue h2] <;> rfl
              rw [e]
              exact ww_holder_zero K hK w hi i hc h2)
            (fun h2 => absurd (Eq.trans (ww_sb_fate_w1 K w i hc.1 hc.2 h0 h2).symm h) (by decide))))
        (fun hc => by
          have hf : w.fate i = 2 := Eq.trans (ww_sb_fate_other K w i hc).symm h
          rw [ww_sb_over_other K w i hc]
          exact hi.overtaken i hf)

    theorem ww_signBody (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) : WaitInv (signBody K w) :=
      { nextPos := hi.nextPos
        pendBorn := hi.pendBorn
        waitBorn := hi.waitBorn
        waitName := hi.waitName
        waitIdle := hi.waitIdle
        waitLatest := hi.waitLatest
        waitStored := hi.waitStored
        fateStored := fun i h => ww_sb_fateStored K w hi i h
        refusedNot := hi.refusedNot
        readBorn := fun n h => ww_sb_readBorn K w hi n h
        readFate := fun n h _ => ww_sb_readFate K w hi n h
        acc := fun i h1 h2 => ww_sb_acc K w hi i h1 h2
        ownBase := fun i hh hne => ww_sb_own K hK w hi i hh hne
        overtaken := fun i h => ww_sb_over K hK w hi i h }

    -- dropped v (forget_waiting, or a settle that shows v reached no server): the
    -- writes pending on v leave, dropped with v refused for good.
    def ww_dropW (w : WWorld) (v : Nat) : WWorld :=
      { w with refused := upd w.refused v true,
               pend := fun i => w.pend i && !(decide (w.ver i = v)),
               fate := fun i => if w.pend i && decide (w.ver i = v) then 4 else w.fate i }
    theorem ww_drop_pend_def (w : WWorld) (v i : Nat) :
        (ww_dropW w v).pend i = (w.pend i && !(decide (w.ver i = v))) := rfl
    theorem ww_drop_fate_def (w : WWorld) (v i : Nat) :
        (ww_dropW w v).fate i = if (w.pend i && decide (w.ver i = v)) = true then 4 else w.fate i := rfl

    theorem ww_drop_same (w : WWorld) (v i : Nat) (h : ¬ (w.pend i = true ∧ w.ver i = v)) :
        (ww_dropW w v).pend i = w.pend i ∧ (ww_dropW w v).fate i = w.fate i :=
      ww_by_cases (w.pend i = true)
        (fun hp => by
          have e : ¬ (w.ver i = v) := fun e => h ⟨hp, e⟩
          exact ⟨by rw [ww_drop_pend_def, hp, dfalse e] <;> rfl, by rw [ww_drop_fate_def, hp, dfalse e] <;> rfl⟩)
        (fun hp => by
          have hf : w.pend i = false := ww_ne_true hp
          exact ⟨by rw [ww_drop_pend_def, hf] <;> rfl, by rw [ww_drop_fate_def, hf] <;> rfl⟩)

    theorem ww_drop_cases (w : WWorld) (v i : Nat) :
        ((ww_dropW w v).pend i = false ∧ (ww_dropW w v).fate i = 4 ∧ w.ver i = v ∧ w.pend i = true)
          ∨ ((ww_dropW w v).pend i = w.pend i ∧ (ww_dropW w v).fate i = w.fate i) :=
      ww_by_cases (w.pend i = true ∧ w.ver i = v)
        (fun h => Or.inl ⟨by rw [ww_drop_pend_def, h.1, dtrue h.2] <;> rfl,
          by rw [ww_drop_fate_def, h.1, dtrue h.2] <;> rfl, h.2, h.1⟩)
        (fun h => Or.inr (ww_drop_same w v i h))

    theorem ww_drop_pend (w : WWorld) (v i : Nat) (h : (ww_dropW w v).pend i = true) : w.pend i = true :=
      (ww_and (Eq.trans (ww_drop_pend_def w v i).symm h)).1

    theorem ww_dropped (w : WWorld) (v : Nat) (hi : WaitInv w) (hs : ¬ (w.stored v = true)) :
        WaitInv (ww_dropW w v) :=
      { nextPos := hi.nextPos
        pendBorn := fun i h => hi.pendBorn i (ww_drop_pend w v i h)
        waitBorn := hi.waitBorn
        waitName := hi.waitName
        waitIdle := fun n h => by
          show (w.pend (w.waitOf n) && !(decide (w.ver (w.waitOf n) = v))) = false
          rw [hi.waitIdle n h]
        waitLatest := fun n j h hp hn => hi.waitLatest n j h (ww_drop_pend w v j hp) hn
        waitStored := hi.waitStored
        fateStored := fun i h => ww_or_cases (ww_drop_cases w v i)
          (fun c => absurd (Eq.trans c.2.1.symm h) (by decide))
          (fun c => hi.fateStored i (Eq.trans c.2.symm h))
        refusedNot := fun u h => ww_by_cases (u = v)
          (fun e => by rw [e]; exact ww_ne_true hs)
          (fun e => hi.refusedNot u (Eq.trans (if_neg e).symm h))
        readBorn := fun n h => by
          have r := hi.readBorn n h
          refine ⟨r.1, ?_, r.2.2⟩
          show (w.pend (w.passRead n) && !(decide (w.ver (w.passRead n) = v))) = false
          rw [r.2.1]
        readFate := fun n h hw => by
          have r := hi.readBorn n h
          have c := ww_drop_same w v (w.passRead n) (fun x => ft (Eq.trans r.2.1.symm x.1))
          show (ww_dropW w v).fate (w.passRead n) = 1 ∨ (ww_dropW w v).fate (w.passRead n) = 2
          rw [c.2]
          exact hi.readFate n h hw
        acc := fun i h1 h2 => ww_or_cases (ww_drop_cases w v i)
          (fun c => Or.inr ⟨c.2.1, by
            show (if w.ver i = v then true else w.refused (w.ver i)) = true
            rw [if_pos c.2.2.1]⟩)
          (fun c => ww_or_cases (hi.acc i h1 h2)
            (fun a => Or.inl (by unfold Accounted; rw [c.1, c.2]; exact a))
            (fun d => Or.inr ⟨Eq.trans c.2 d.1, ww_upd_true _ _ _ d.2⟩))
        ownBase := fun i hh hne => hi.ownBase i
          (ww_or_cases hh (fun hp => Or.inl (ww_drop_pend w v i hp)) (fun hw => Or.inr hw)) hne
        overtaken := fun i h => ww_or_cases (ww_drop_cases w v i)
          (fun c => absurd (Eq.trans c.2.1.symm h) (by decide))
          (fun c => hi.overtaken i (Eq.trans c.2.symm h)) }

    theorem ww_stored (w : WWorld) (v : Nat) (hi : WaitInv w) (hr : ¬ (w.refused v = true)) :
        WaitInv { w with stored := upd w.stored v true } :=
      { nextPos := hi.nextPos
        pendBorn := hi.pendBorn
        waitBorn := hi.waitBorn
        waitName := hi.waitName
        waitIdle := hi.waitIdle
        waitLatest := hi.waitLatest
        waitStored := fun n h => ww_upd_true _ _ _ (hi.waitStored n h)
        fateStored := fun i h => ⟨(hi.fateStored i h).1, ww_upd_true _ _ _ (hi.fateStored i h).2⟩
        refusedNot := fun u h => ww_by_cases (u = v)
          (fun e => by rw [e] at h; exact absurd h hr)
          (fun e => Eq.trans (if_neg e) (hi.refusedNot u h))
        readBorn := hi.readBorn
        readFate := hi.readFate
        acc := hi.acc
        ownBase := hi.ownBase
        overtaken := hi.overtaken }

    theorem ww_other (w : WWorld) (n x : Nat) (hi : WaitInv w) :
        WaitInv { w with log := upd w.log n x, holder := upd w.holder n 0, hdirect := upd w.hdirect n 0 } :=
      { nextPos := hi.nextPos
        pendBorn := hi.pendBorn
        waitBorn := hi.waitBorn
        waitName := hi.waitName
        waitIdle := hi.waitIdle
        waitLatest := hi.waitLatest
        waitStored := hi.waitStored
        fateStored := hi.fateStored
        refusedNot := hi.refusedNot
        readBorn := hi.readBorn
        readFate := hi.readFate
        acc := hi.acc
        ownBase := fun i hh hne => ww_by_cases (w.nm i = n)
          (fun e => absurd (show upd w.holder n 0 (w.nm i) = 0 from if_pos e) hne)
          (fun e => by
            have el : upd w.log n x (w.nm i) = w.log (w.nm i) := if_neg e
            have eh : upd w.holder n 0 (w.nm i) = w.holder (w.nm i) := if_neg e
            show upd w.log n x (w.nm i) = w.val i ∨ (w.bs i (upd w.log n x (w.nm i)) = true ∧ w.val i ≠ 0)
            rw [el]
            exact hi.ownBase i hh (fun z => hne (Eq.trans eh z)))
        overtaken := hi.overtaken }

    theorem ww_due (w : WWorld) (b : Bool) (hi : WaitInv w) : WaitInv { w with due := b } :=
      ⟨hi.nextPos, hi.pendBorn, hi.waitBorn, hi.waitName, hi.waitIdle, hi.waitLatest, hi.waitStored,
       hi.fateStored, hi.refusedNot, hi.readBorn, hi.readFate, hi.acc, hi.ownBase, hi.overtaken⟩
    theorem ww_unread (w : WWorld) (b : Bool) (hi : WaitInv w) : WaitInv { w with unread := b } :=
      ⟨hi.nextPos, hi.pendBorn, hi.waitBorn, hi.waitName, hi.waitIdle, hi.waitLatest, hi.waitStored,
       hi.fateStored, hi.refusedNot, hi.readBorn, hi.readFate, hi.acc, hi.ownBase, hi.overtaken⟩

    -- A direct draft's steps touch no field WaitInv reads but the keep's, which
    -- writes a name as another write does.
    theorem ww_holdUnread (w : WWorld) (b : Bool) (hi : WaitInv w) : WaitInv { w with holdUnread := b } :=
      ⟨hi.nextPos, hi.pendBorn, hi.waitBorn, hi.waitName, hi.waitIdle, hi.waitLatest, hi.waitStored,
       hi.fateStored, hi.refusedNot, hi.readBorn, hi.readFate, hi.acc, hi.ownBase, hi.overtaken⟩
    theorem ww_dphase (w : WWorld) (f : Nat → Nat) (hi : WaitInv w) : WaitInv { w with dphase := f } :=
      ⟨hi.nextPos, hi.pendBorn, hi.waitBorn, hi.waitName, hi.waitIdle, hi.waitLatest, hi.waitStored,
       hi.fateStored, hi.refusedNot, hi.readBorn, hi.readFate, hi.acc, hi.ownBase, hi.overtaken⟩
    theorem ww_draftD (w : WWorld) (n x : Nat) (hi : WaitInv w) :
        WaitInv { w with dnext := w.dnext + 1, dnm := upd w.dnm w.dnext n, dval := upd w.dval w.dnext x,
                         dstamp := upd w.dstamp w.dnext w.next, dphase := upd w.dphase w.dnext 1 } :=
      ⟨hi.nextPos, hi.pendBorn, hi.waitBorn, hi.waitName, hi.waitIdle, hi.waitLatest, hi.waitStored,
       hi.fateStored, hi.refusedNot, hi.readBorn, hi.readFate, hi.acc, hi.ownBase, hi.overtaken⟩
    theorem ww_keep (w : WWorld) (d : Nat) (hi : WaitInv w) : WaitInv (keepBody w d) :=
      have h := ww_other w (w.dnm d) (w.dval d) hi
      ⟨h.nextPos, h.pendBorn, h.waitBorn, h.waitName, h.waitIdle, h.waitLatest, h.waitStored,
       h.fateStored, h.refusedNot, h.readBorn, h.readFate, h.acc, h.ownBase, h.overtaken⟩

    -- The publish guard's parts the step needs (publishOk, its conjuncts in order).
    theorem ww_pub_parts (K : Kernels) (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool)
        (opened : Nat → Bool) (hg : publishOk K w v k names vals bases opened = true) :
        allN w.next (fun i => !(w.pend i && opened (w.ver i)) || w.stored (w.ver i)) = true
          ∧ allN k (fun a => bases a (w.log (names a))) = true
          ∧ allN k (fun a => !(decide (vals a = 0)) || decide (w.log (names a) = 0)) = true := by
      unfold publishOk at hg
      have z := ww_and hg
      have hb : pubBase K w.holdUnread w v k names vals bases opened = true := z.1
      unfold pubBase at hb
      -- Clean's && groups to the right: a && (b && c).
      have a := ww_and hb
      have b := ww_and a.2
      have c := ww_and b.2
      have d := ww_and c.2
      have e := ww_and d.2
      have f := ww_and e.2
      exact ⟨d.1, f.1, f.2⟩
    theorem ww_takeback (w : WWorld) (k : Nat) (names vals : Nat → Nat)
        (h : allN k (fun a => !(decide (vals a = 0)) || decide (w.log (names a) = 0)) = true)
        (x : Nat) (hx : x < k) (hv : vals x = 0) : w.log (names x) = 0 := by
      have t : (!(decide (vals x = 0)) || decide (w.log (names x) = 0)) = true := allN_elim _ k h x hx
      rw [dtrue hv] at t
      have t2 : decide (w.log (names x) = 0) = true := t
      exact of_decide_eq_true t2

    -- Each event, one state-lock section, keeps WaitInv.
    theorem ww_step_publish (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (v k : Nat)
        (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (opened : Nat → Bool) :
        WaitInv (wstep K w (WEvent.publish v k names vals bases opened)) :=
      ww_by_cases (publishOk K w v k names vals bases opened = true)
        (fun hg => by
          rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_pos hg]
          have p := ww_pub_parts K w v k names vals bases opened hg
          exact ww_pushW (landW K opened w) v k names vals bases (ww_landW K hK opened w hi (ww_guard w opened p.1))
            (fun x hx => allN_elim _ k p.2.1 x hx)
            (fun x hx hv => ww_takeback w k names vals p.2.2 x hx hv))
        (fun hg => by
          rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_neg hg]
          exact hi)

    theorem ww_step_stored (K : Kernels) (w : WWorld) (hi : WaitInv w) (v : Nat) :
        WaitInv (wstep K w (WEvent.stored v)) :=
      ww_by_cases (w.refused v = true)
        (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_pos hr]; exact hi)
        (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_neg hr]; exact ww_stored w v hi hr)

    theorem ww_step_dropped (K : Kernels) (w : WWorld) (hi : WaitInv w) (v : Nat) :
        WaitInv (wstep K w (WEvent.dropped v)) :=
      ww_by_cases (w.stored v = true)
        (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_pos hs]; exact hi)
        (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_neg hs]; exact ww_dropped w v hi hs)

    theorem ww_step_land (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (S : Nat → Bool) :
        WaitInv (wstep K w (WEvent.land S)) :=
      ww_by_cases ((!(w.unread) && allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i))) = true)
        (fun hg => by
          rw [show wstep K w (WEvent.land S) = _ from if_pos hg]
          exact ww_landW K hK S w hi (ww_guard w S (ww_and hg).2))
        (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_neg hg]; exact hi)

    theorem ww_step_sign (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (opens : Bool) :
        WaitInv (wstep K w (WEvent.signPass opens)) :=
      ww_by_cases (K.passSigns w.due opens (!w.unread) = true)
        (fun hps => by
          show WaitInv (signW K opens w)
          rw [show signW K opens w = signBody K w from if_pos hps]
          exact ww_signBody K hK w hi)
        (fun hps => by
          show WaitInv (signW K opens w)
          rw [show signW K opens w = w from if_neg hps]
          exact hi)

    theorem ww_step (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (e : WEvent) :
        WaitInv (wstep K w e) := by
      cases e with
      | publish v k names vals bases opened => exact ww_step_publish K hK w hi v k names vals bases opened
      | stored v => exact ww_step_stored K w hi v
      | dropped v => exact ww_step_dropped K w hi v
      | land S => exact ww_step_land K hK w hi S
      | signPass opens => exact ww_step_sign K hK w hi opens
      | settlePass => exact ww_settleW w hi
      | other n x => exact ww_other w n x hi
      | due b => exact ww_due w b hi
      | unreadable b => exact ww_unread w b hi
      | directDraft n x =>
        exact ww_by_cases (turnFree w = true)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_pos g]; exact ww_draftD w n x hi)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_neg g]; exact hi)
      | directHold d =>
        exact ww_by_cases (decide (w.dphase d = 1) = true)
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_pos g]; exact ww_dphase w _ hi)
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_neg g]; exact hi)
      | keep d =>
        exact ww_by_cases (K.draftedKeeps (decide (w.dphase d = 2)) = true)
          (fun g => by rw [show wstep K w (WEvent.keep d) = _ from if_pos g]; exact ww_keep w d hi)
          (fun g => by rw [show wstep K w (WEvent.keep d) = _ from if_neg g]; exact hi)
      | unhold d =>
        exact ww_by_cases ((decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true)
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_pos g]; exact ww_dphase w _ hi)
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_neg g]; exact hi)
      | holdUnreadable b => exact ww_holdUnread w b hi

    -- WaitInv holds in every state of every run, for any kernels that compute the spec.
    theorem waits_hold (K : Kernels) (hK : Exact K) : Satisfies (WaitSpec K) (Always (Lift WaitInv)) :=
      invariantSatisfies (WaitSpec K) WaitInv (fun s h => ww_init s h)
        (fun s t hs ht => Exists.elim ht (fun e he => Eq.mpr (congrArg WaitInv he) (ww_step K hK s hs e)))

    -- W-a: a write waits, or is signed, only once a server took its version.
    theorem signed_only_stored (K : Kernels) (hK : Exact K) : Satisfies (WaitSpec K) (Always (Lift SignedOnlyStored)) := by
      intro b runs n
      have hi : WaitInv (drop b n 0) := waits_hold K hK b runs n
      exact And.intro (fun m hm => hi.waitStored m hm) (fun i hf => (hi.fateStored i hf).2)

    -- W-c: no write of a version a server took is lost: no bound on the pending
    -- sets, and a set that does not open is never read as none.
    theorem no_lost_write (K : Kernels) (hK : Exact K) : Satisfies (WaitSpec K) (Always (Lift NoLostWrite)) := by
      intro b runs n
      have hi : WaitInv (drop b n 0) := waits_hold K hK b runs n
      intro i h1 h2 hs
      exact ww_or_cases (hi.acc i h1 h2) (fun a => a)
        (fun d => ft (Eq.trans (hi.refusedNot _ d.2).symm hs))

    -- ---- This Mac's direct drafts. A write is live while it is pending or waits at
    --      its name. DirectInv: a born direct draft has an id below dnext; one in its
    --      turn was drafted after every born write; a live write of a name a held
    --      draft from before it writes has that draft's value among its bases
    --      (heldIn); a live write of a name whose log value a direct draft from
    --      before it kept stands over that value, or is done; and an overtaken write
    --      is born, and is a take-back, or no direct draft's kept value overtook it,
    --      or one drafted after it.
    def LiveW (w : WWorld) (i : Nat) : Prop := w.pend i = true ∨ (i ≠ 0 ∧ w.waitOf (w.nm i) = i)

    structure DirectInv (w : WWorld) : Prop where
      dBorn : ∀ d, w.dphase d ≠ 0 → d < w.dnext
      drafting : ∀ d, w.dphase d = 1 → w.next ≤ w.dstamp d
      heldBase : ∀ i d, LiveW w i → w.dphase d = 2 → w.dnm d = w.nm i → w.dstamp d ≤ i →
        w.bs i (w.dval d) = true
      directBase : ∀ i, LiveW w i → w.hdirect (w.nm i) ≠ 0 → w.hdirect (w.nm i) ≤ i →
        w.log (w.nm i) = w.val i ∨ w.bs i (w.log (w.nm i)) = true
      overDirect : ∀ i, w.fate i = 2 → i < w.next ∧ (w.val i = 0 ∨ w.overD i = 0 ∨ i < w.overD i)

    theorem wd_lt_succ (n : Nat) : n < n + 1 := by omega
    theorem wd_lt_succ_of (a b : Nat) (h : a < b) : a < b + 1 := by omega
    theorem wd_lt (a b : Nat) (h : ¬ (a ≤ b)) : b < a := by omega
    theorem wd_le_eq (a b : Nat) (h : a = b) : a ≤ b := by omega

    theorem wd_init (w : WWorld) (h : w = ww0) : DirectInv w := by
      subst h
      exact
        { dBorn := fun d h => absurd rfl h
          drafting := fun d h => by have h' : (0 : Nat) = 1 := h; exact absurd h' (by decide)
          heldBase := fun i d _ h => by have h' : (0 : Nat) = 2 := h; exact absurd h' (by decide)
          directBase := fun i _ h => absurd rfl h
          overDirect := fun i h => by have h' : (0 : Nat) = 2 := h; exact absurd h' (by decide) }

    theorem wd_live_born (w : WWorld) (hi : WaitInv w) (i : Nat) (h : LiveW w i) : i < w.next :=
      ww_or_cases h (fun p => (hi.pendBorn i p).2)
        (fun q => by
          have hz : (w.waitOf (w.nm i) ≠ 0) := by rw [q.2]; exact q.1
          have t := hi.waitBorn (w.nm i) hz
          rw [q.2] at t
          exact t)

    -- landW, settleW and a drop make no write live; pushW makes the new ones live.
    theorem wd_lw_live (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (i : Nat)
        (h : LiveW (landW K S w) i) : LiveW w i :=
      ww_or_cases h (fun p => Or.inl (ww_lw_pend K hK S w i p).1)
        (fun q => ww_or_cases (ww_lw_wait K S w (w.nm i))
          (fun a => Or.inr ⟨q.1, Eq.trans a.2.symm q.2⟩)
          (fun b => Or.inl (by
            have e : (landingOf w S (w.nm i) = i) := Eq.trans b.2.symm q.2
            have t := (ww_landing w S (w.nm i) b.1).1
            rw [e] at t
            exact t)))
    theorem wd_lw_fate2 (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) (h : (landW K S w).fate i = 2) :
        w.fate i = 2 := by
      rw [ww_lw_fate_def] at h
      exact ww_by_cases ((ww_c1 K S w i || ww_c2 S w i) = true)
        (fun c => by rw [if_pos c] at h; exact absurd h (by decide))
        (fun c => by rw [if_neg c] at h; exact h)
    theorem wd_push_live (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (i : Nat)
        (hr : ¬ (w.next ≤ i ∧ i < w.next + k)) (h : LiveW (pushW w v k names vals bases) i) : LiveW w i :=
      have o := ww_push_old w v k names vals bases i hr
      ww_or_cases h (fun p => Or.inl (Eq.trans o.2.2.2.2.symm p))
        (fun q => Or.inr ⟨q.1, by rw [← o.1]; exact q.2⟩)
    theorem wd_settle_live (w : WWorld) (i : Nat) (h : LiveW (settleW w) i) : LiveW w i :=
      ww_or_cases h (fun p => Or.inl p)
        (fun q => Or.inr ⟨q.1, ww_or_cases (ww_settle_wait w (w.nm i))
          (fun e => Eq.trans e.symm q.2)
          (fun z => absurd (Eq.trans z.1.symm q.2).symm q.1)⟩)
    theorem wd_drop_live (w : WWorld) (v i : Nat) (h : LiveW (ww_dropW w v) i) : LiveW w i :=
      ww_or_cases h (fun p => Or.inl (ww_drop_pend w v i p)) (fun q => Or.inr q)

    theorem wd_landW (K : Kernels) (hK : Exact K) (S : Nat → Bool) (w : WWorld) (hD : DirectInv w) :
        DirectInv (landW K S w) :=
      { dBorn := hD.dBorn
        drafting := hD.drafting
        heldBase := fun i d hl => hD.heldBase i d (wd_lw_live K hK S w i hl)
        directBase := fun i hl => hD.directBase i (wd_lw_live K hK S w i hl)
        overDirect := fun i h => hD.overDirect i (wd_lw_fate2 K S w i h) }

    theorem ww_pub_direct (K : Kernels) (w : WWorld) (v k : Nat) (names vals : Nat → Nat) (bases : Nat → Nat → Bool)
        (opened : Nat → Bool) (hg : publishOk K w v k names vals bases opened = true) :
        allN w.dnext (fun d => !(decide (w.dphase d = 1))) = true
          ∧ allN k (fun a => allN w.dnext (fun d =>
              !(decide (w.dphase d = 2) && decide (w.dnm d = names a)) || bases a (w.dval d))) = true := by
      unfold publishOk turnFree heldIn at hg
      exact ww_and (ww_and hg).2

    -- publish: no direct draft is in its turn (turnFree); each new write's bases
    -- hold every held draft's value of its name (heldIn) and the log's (pubBase).
    theorem wd_publish (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (hD : DirectInv w) (v k : Nat)
        (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (opened : Nat → Bool)
        (hg : publishOk K w v k names vals bases opened = true) :
        DirectInv (pushW (landW K opened w) v k names vals bases) :=
      have p := ww_pub_parts K w v k names vals bases opened hg
      have z := ww_pub_direct K w v k names vals bases opened hg
      have hL := wd_landW K hK opened w hD
      { dBorn := hD.dBorn
        drafting := fun d h => by
          have h' : (w.dphase d = 1) := h
          have hb : (d < w.dnext) := hD.dBorn d (fun e => hd_01 (Eq.trans e.symm h'))
          have t : ((!(decide (w.dphase d = 1))) = true) := allN_elim _ w.dnext z.1 d hb
          exact absurd h' (hd_ndec t)
        heldBase := fun i d hl hp hn hs => ww_by_cases (w.next ≤ i ∧ i < w.next + k)
          (fun hr => by
            have nw := ww_push_new (landW K opened w) v k names vals bases i hr
            have ha : (i - w.next < k) := ww_sub_lt w.next i k hr.1 hr.2
            have hp' : (w.dphase d = 2) := hp
            have hdn : (d < w.dnext) := hD.dBorn d (fun e => hd_02 (Eq.trans e.symm hp'))
            have hn' : w.dnm d = names (i - w.next) := Eq.trans hn nw.1
            have t : ((!(decide (w.dphase d = 2) && decide (w.dnm d = names (i - w.next)))
                || bases (i - w.next) (w.dval d)) = true) :=
              allN_elim _ w.dnext (allN_elim _ k z.2 (i - w.next) ha) d hdn
            rw [dtrue hp', dtrue hn'] at t
            rw [nw.2.2.1]
            exact t)
          (fun hr => by
            have o := ww_push_old (landW K opened w) v k names vals bases i hr
            rw [o.2.2.1]
            exact hL.heldBase i d (wd_push_live (landW K opened w) v k names vals bases i hr hl) hp
              (Eq.trans hn o.1) hs)
        directBase := fun i hl h1 h2 => ww_by_cases (w.next ≤ i ∧ i < w.next + k)
          (fun hr => by
            have nw := ww_push_new (landW K opened w) v k names vals bases i hr
            have ha : (i - w.next < k) := ww_sub_lt w.next i k hr.1 hr.2
            have t : (bases (i - w.next) (w.log (names (i - w.next))) = true) := allN_elim _ k p.2.1 (i - w.next) ha
            rw [nw.1, nw.2.2.1]
            exact Or.inr t)
          (fun hr => by
            have o := ww_push_old (landW K opened w) v k names vals bases i hr
            rw [o.1] at h1 h2
            rw [o.1, o.2.1, o.2.2.1]
            exact hL.directBase i (wd_push_live (landW K opened w) v k names vals bases i hr hl) h1 h2)
        overDirect := fun i h => by
          have f : (w.fate i = 2) := wd_lw_fate2 K opened w i h
          have t := hD.overDirect i f
          have hr : ¬ (w.next ≤ i ∧ i < w.next + k) := ww_not_in w k i t.1
          have o := ww_push_old (landW K opened w) v k names vals bases i hr
          refine ⟨ww_le_add i w.next k t.1, ?_⟩
          rw [o.2.1]
          exact t.2 }

    -- signBody's overD, by case: the stamp at a name whose waiting write is
    -- overtaken, else as it was.
    theorem wd_sb_overD_two (K : Kernels) (w : WWorld) (i : Nat) (hz : ¬ (i = 0)) (hw : w.waitOf (w.nm i) = i)
        (h2 : passVerdict K w (w.nm i) = 2) : (signBody K w).overD i = w.hdirect (w.nm i) := by
      show (if (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
          && decide (passVerdict K w (w.nm i) = 2))) = true
        then w.hdirect (w.nm i) else w.overD i) = w.hdirect (w.nm i)
      rw [dfalse hz, dtrue hw, dtrue h2] <;> rfl
    theorem wd_sb_overD_not_two (K : Kernels) (w : WWorld) (i : Nat) (h2 : ¬ (passVerdict K w (w.nm i) = 2)) :
        (signBody K w).overD i = w.overD i := by
      show (if (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
          && decide (passVerdict K w (w.nm i) = 2))) = true
        then w.hdirect (w.nm i) else w.overD i) = w.overD i
      rw [dfalse h2, ww_and_and_false] <;> rfl
    theorem wd_sb_overD_other (K : Kernels) (w : WWorld) (i : Nat) (h : ¬ (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)) :
        (signBody K w).overD i = w.overD i := by
      show (if (!(decide (i = 0)) && (decide (w.waitOf (w.nm i) = i)
          && decide (passVerdict K w (w.nm i) = 2))) = true
        then w.hdirect (w.nm i) else w.overD i) = w.overD i
      apply if_neg
      intro hc
      have a := ww_and hc
      have b := ww_and a.2
      exact h ⟨ww_df (ww_not a.1), of_decide_eq_true b.1⟩

    -- signBody: a signed name's log value is this Mac's write's now (hdirect 0);
    -- an overtaken write was a take-back, or stood over no direct draft's value
    -- from before it (directBase).
    theorem wd_signBody (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (hD : DirectInv w) :
        DirectInv (signBody K w) :=
      { dBorn := hD.dBorn
        drafting := hD.drafting
        heldBase := hD.heldBase
        directBase := fun i hl h1 h2 => ww_by_cases (passVerdict K w (w.nm i) = 3)
          (fun h3 => absurd (show (signBody K w).hdirect (w.nm i) = 0 from if_pos h3) h1)
          (fun h3 => by
            have eh : (signBody K w).hdirect (w.nm i) = w.hdirect (w.nm i) := if_neg h3
            have el : (signBody K w).log (w.nm i) = w.log (w.nm i) := if_neg h3
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : ((signBody K w).hdirect (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show (signBody K w).log (w.nm i) = w.val i ∨ w.bs i ((signBody K w).log (w.nm i)) = true
            rw [el]
            exact hD.directBase i hl h1' h2')
        overDirect := fun i h => ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
          (fun c => by
            have hz : (w.waitOf (w.nm i) ≠ 0) := by rw [c.2]; exact c.1
            have hb : (i < w.next) := by
              have t := hi.waitBorn (w.nm i) hz
              rw [c.2] at t
              exact t
            exact ww_by_cases (passVerdict K w (w.nm i) = 2)
              (fun h2 => by
                have eo := wd_sb_overD_two K w i c.1 c.2 h2
                refine ⟨hb, ?_⟩
                rw [eo]
                have pv := ww_pv2 K hK w (w.nm i) h2
                rw [c.2] at pv
                exact ww_or_cases pv.2.2
                  (fun nb => Or.inr (ww_by_cases (w.hdirect (w.nm i) = 0) (fun e => Or.inl e)
                    (fun e => Or.inr (ww_by_cases (w.hdirect (w.nm i) ≤ i)
                      (fun le => absurd (hD.directBase i (Or.inr c) e le)
                        (fun r => ww_or_cases r (fun a => pv.2.1 a) (fun b => hd_tf (Eq.trans b.symm nb))))
                      (fun le => wd_lt _ _ le)))))
                  (fun tb => Or.inl tb))
              (fun h2 => by
                have eo := wd_sb_overD_not_two K w i h2
                have ef := ww_sb_fate_wait K w i c.1 c.2
                exact ww_by_cases (passVerdict K w (w.nm i) = 0)
                  (fun h0 => by
                    rw [ef, if_pos h0] at h
                    have t := hD.overDirect i h
                    refine ⟨t.1, ?_⟩
                    rw [eo]
                    exact t.2)
                  (fun h0 => by
                    rw [ef, if_neg h0, if_neg h2] at h
                    exact absurd h (by decide))))
          (fun c => by
            have ef := ww_sb_fate_other K w i c
            have eo := wd_sb_overD_other K w i c
            have t := hD.overDirect i (Eq.trans ef.symm h)
            refine ⟨t.1, ?_⟩
            rw [eo]
            exact t.2) }

    theorem wd_settleW (w : WWorld) (hD : DirectInv w) : DirectInv (settleW w) :=
      { dBorn := hD.dBorn
        drafting := hD.drafting
        heldBase := fun i d hl => hD.heldBase i d (wd_settle_live w i hl)
        directBase := fun i hl => hD.directBase i (wd_settle_live w i hl)
        overDirect := hD.overDirect }

    theorem wd_dropped (w : WWorld) (v : Nat) (hD : DirectInv w) : DirectInv (ww_dropW w v) :=
      { dBorn := hD.dBorn
        drafting := hD.drafting
        heldBase := fun i d hl => hD.heldBase i d (wd_drop_live w v i hl)
        directBase := fun i hl => hD.directBase i (wd_drop_live w v i hl)
        overDirect := fun i h => hD.overDirect i (ww_or_cases (ww_drop_cases w v i)
          (fun a => absurd (Eq.trans a.2.1.symm h) (by decide)) (fun b => Eq.trans b.2.symm h)) }

    -- another write of name n: the log's value there is no direct draft's now.
    theorem wd_other (w : WWorld) (n x : Nat) (hD : DirectInv w) :
        DirectInv { w with log := upd w.log n x, holder := upd w.holder n 0, hdirect := upd w.hdirect n 0 } :=
      { dBorn := hD.dBorn
        drafting := hD.drafting
        heldBase := hD.heldBase
        directBase := fun i hl h1 h2 => ww_by_cases (w.nm i = n)
          (fun e => absurd (show upd w.hdirect n 0 (w.nm i) = 0 from if_pos e) h1)
          (fun e => by
            have eh : upd w.hdirect n 0 (w.nm i) = w.hdirect (w.nm i) := if_neg e
            have el : upd w.log n x (w.nm i) = w.log (w.nm i) := if_neg e
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : (upd w.hdirect n 0 (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show upd w.log n x (w.nm i) = w.val i ∨ w.bs i (upd w.log n x (w.nm i)) = true
            rw [el]
            exact hD.directBase i hl h1' h2')
        overDirect := hD.overDirect }

    -- directDraft in its turn: the new draft is stamped after every born write.
    theorem wd_draftD (w : WWorld) (hD : DirectInv w) (n x : Nat) :
        DirectInv { w with dnext := w.dnext + 1, dnm := upd w.dnm w.dnext n, dval := upd w.dval w.dnext x,
                           dstamp := upd w.dstamp w.dnext w.next, dphase := upd w.dphase w.dnext 1 } :=
      { dBorn := fun d h => ww_by_cases (d = w.dnext)
          (fun e => by rw [e]; exact wd_lt_succ w.dnext)
          (fun e => wd_lt_succ_of _ _ (hD.dBorn d (fun z => h (Eq.trans (hd_updO w.dphase w.dnext d 1 e) z))))
        drafting := fun d h => ww_by_cases (d = w.dnext)
          (fun e => by
            have t : (upd w.dstamp w.dnext w.next d = w.next) := by rw [e, hd_updS]
            show w.next ≤ upd w.dstamp w.dnext w.next d
            exact wd_le_eq _ _ t.symm)
          (fun e => by
            have t : (w.dphase d = 1) := Eq.trans (hd_updO w.dphase w.dnext d 1 e).symm h
            have u : (upd w.dstamp w.dnext w.next d = w.dstamp d) := hd_updO w.dstamp w.dnext d w.next e
            show w.next ≤ upd w.dstamp w.dnext w.next d
            rw [u]
            exact hD.drafting d t)
        heldBase := fun i d hl hp hn hs => ww_by_cases (d = w.dnext)
          (fun e => by
            have t : (upd w.dphase w.dnext 1 d = 2) := hp
            rw [e, hd_updS] at t
            exact absurd t (by decide))
          (fun e => by
            have t : (w.dphase d = 2) := Eq.trans (hd_updO w.dphase w.dnext d 1 e).symm hp
            have tn : (w.dnm d = w.nm i) := Eq.trans (hd_updO w.dnm w.dnext d n e).symm hn
            have ts : (w.dstamp d ≤ i) := by
              have u : (upd w.dstamp w.dnext w.next d ≤ i) := hs
              rw [hd_updO w.dstamp w.dnext d w.next e] at u
              exact u
            show w.bs i (upd w.dval w.dnext x d) = true
            rw [hd_updO w.dval w.dnext d x e]
            exact hD.heldBase i d hl t tn ts)
        directBase := hD.directBase
        overDirect := hD.overDirect }

    -- directHold: a draft held now was stamped after every live write.
    theorem wd_holdD (w : WWorld) (hi : WaitInv w) (hD : DirectInv w) (d : Nat) (g : w.dphase d = 1) :
        DirectInv { w with dphase := upd w.dphase d 2 } :=
      { dBorn := fun e h => ww_by_cases (e = d)
          (fun ed => by rw [ed]; exact hD.dBorn d (fun z => hd_01 (Eq.trans z.symm g)))
          (fun ed => hD.dBorn e (fun z => h (Eq.trans (hd_updO w.dphase d e 2 ed) z)))
        drafting := fun e h => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 2 e = 1) := h
            rw [ed, hd_updS] at t
            exact absurd t (by decide))
          (fun ed => hD.drafting e (Eq.trans (hd_updO w.dphase d e 2 ed).symm h))
        heldBase := fun i e hl hp hn hs => ww_by_cases (e = d)
          (fun ed => by
            have hb := wd_live_born w hi i hl
            have hd := hD.drafting d g
            have hs' : (w.dstamp d ≤ i) := by rw [← ed]; exact hs
            exact False.elim (hd_ltIrr _ _ hs' (ww_lt_le _ _ _ hb hd)))
          (fun ed => hD.heldBase i e hl (Eq.trans (hd_updO w.dphase d e 2 ed).symm hp) hn hs)
        directBase := hD.directBase
        overDirect := hD.overDirect }

    -- keep: a held draft's value enters the log; every live write of its name from
    -- after its draft has that value among its bases (heldBase).
    theorem wd_keep (w : WWorld) (hD : DirectInv w) (d : Nat) (g : w.dphase d = 2) : DirectInv (keepBody w d) :=
      { dBorn := fun e h => ww_by_cases (e = d)
          (fun ed => by rw [ed]; exact hD.dBorn d (fun z => hd_02 (Eq.trans z.symm g)))
          (fun ed => hD.dBorn e (fun z => h (Eq.trans (hd_updO w.dphase d e 3 ed) z)))
        drafting := fun e h => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 1) := h
            rw [ed, hd_updS] at t
            exact absurd t (by decide))
          (fun ed => hD.drafting e (Eq.trans (hd_updO w.dphase d e 3 ed).symm h))
        heldBase := fun i e hl hp hn hs => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 2) := hp
            rw [ed, hd_updS] at t
            exact absurd t (by decide))
          (fun ed => hD.heldBase i e hl (Eq.trans (hd_updO w.dphase d e 3 ed).symm hp) hn hs)
        directBase := fun i hl h1 h2 => ww_by_cases (w.nm i = w.dnm d)
          (fun en => by
            have eh : (upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) = w.dstamp d) := by rw [en, hd_updS]
            have el : (upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.dval d) := by rw [en, hd_updS]
            have hs : (w.dstamp d ≤ i) := by
              have t : (upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.val i
              ∨ w.bs i (upd w.log (w.dnm d) (w.dval d) (w.nm i)) = true
            rw [el]
            exact Or.inr (hD.heldBase i d hl g en.symm hs))
          (fun en => by
            have eh : upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) = w.hdirect (w.nm i) :=
              hd_updO w.hdirect (w.dnm d) (w.nm i) (w.dstamp d) en
            have el : upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.log (w.nm i) :=
              hd_updO w.log (w.dnm d) (w.nm i) (w.dval d) en
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : (upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.val i
              ∨ w.bs i (upd w.log (w.dnm d) (w.dval d) (w.nm i)) = true
            rw [el]
            exact hD.directBase i hl h1' h2')
        overDirect := hD.overDirect }

    -- unhold: a draft leaves its turn or its hold unkept.
    theorem wd_unhold (w : WWorld) (hD : DirectInv w) (d : Nat) (g : w.dphase d ≠ 0) :
        DirectInv { w with dphase := upd w.dphase d 3 } :=
      { dBorn := fun e h => ww_by_cases (e = d)
          (fun ed => by rw [ed]; exact hD.dBorn d g)
          (fun ed => hD.dBorn e (fun z => h (Eq.trans (hd_updO w.dphase d e 3 ed) z)))
        drafting := fun e h => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 1) := h
            rw [ed, hd_updS] at t
            exact absurd t (by decide))
          (fun ed => hD.drafting e (Eq.trans (hd_updO w.dphase d e 3 ed).symm h))
        heldBase := fun i e hl hp hn hs => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 2) := hp
            rw [ed, hd_updS] at t
            exact absurd t (by decide))
          (fun ed => hD.heldBase i e hl (Eq.trans (hd_updO w.dphase d e 3 ed).symm hp) hn hs)
        directBase := hD.directBase
        overDirect := hD.overDirect }

    theorem wd_step (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (hD : DirectInv w) (e : WEvent) :
        DirectInv (wstep K w e) := by
      cases e with
      | publish v k names vals bases opened =>
        exact ww_by_cases (publishOk K w v k names vals bases opened = true)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_pos hg]
            exact wd_publish K hK w hi hD v k names vals bases opened hg)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_neg hg]
            exact hD)
      | stored v =>
        exact ww_by_cases (w.refused v = true)
          (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_pos hr]; exact hD)
          (fun hr => by
            rw [show wstep K w (WEvent.stored v) = _ from if_neg hr]
            exact ⟨hD.dBorn, hD.drafting, hD.heldBase, hD.directBase, hD.overDirect⟩)
      | dropped v =>
        exact ww_by_cases (w.stored v = true)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_pos hs]; exact hD)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_neg hs]; exact wd_dropped w v hD)
      | land S =>
        exact ww_by_cases ((!(w.unread) && allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i))) = true)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_pos hg]; exact wd_landW K hK S w hD)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_neg hg]; exact hD)
      | signPass opens =>
        exact ww_by_cases (K.passSigns w.due opens (!w.unread) = true)
          (fun hps => by
            show DirectInv (signW K opens w)
            rw [show signW K opens w = signBody K w from if_pos hps]
            exact wd_signBody K hK w hi hD)
          (fun hps => by
            show DirectInv (signW K opens w)
            rw [show signW K opens w = w from if_neg hps]
            exact hD)
      | settlePass => exact wd_settleW w hD
      | other n x => exact wd_other w n x hD
      | due b => exact ⟨hD.dBorn, hD.drafting, hD.heldBase, hD.directBase, hD.overDirect⟩
      | unreadable b => exact ⟨hD.dBorn, hD.drafting, hD.heldBase, hD.directBase, hD.overDirect⟩
      | directDraft n x =>
        exact ww_by_cases (turnFree w = true)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_pos g]; exact wd_draftD w hD n x)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_neg g]; exact hD)
      | directHold d =>
        exact ww_by_cases (decide (w.dphase d = 1) = true)
          (fun g => by
            rw [show wstep K w (WEvent.directHold d) = _ from if_pos g]
            exact wd_holdD w hi hD d (hd_dec g))
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_neg g]; exact hD)
      | keep d =>
        exact ww_by_cases (K.draftedKeeps (decide (w.dphase d = 2)) = true)
          (fun g => by
            rw [show wstep K w (WEvent.keep d) = _ from if_pos g]
            exact wd_keep w hD d (hd_dec (drafted_keeps_true K hK _ g)))
          (fun g => by rw [show wstep K w (WEvent.keep d) = _ from if_neg g]; exact hD)
      | unhold d =>
        exact ww_by_cases ((decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true)
          (fun g => by
            rw [show wstep K w (WEvent.unhold d) = _ from if_pos g]
            exact wd_unhold w hD d (ww_or_cases (ww_or g)
              (fun a z => hd_01 (Eq.trans z.symm (hd_dec a)))
              (fun b z => hd_02 (Eq.trans z.symm (hd_dec b)))))
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_neg g]; exact hD)
      | holdUnreadable b => exact ⟨hD.dBorn, hD.drafting, hD.heldBase, hD.directBase, hD.overDirect⟩

    -- WaitInv and DirectInv hold together in every state of every run.
    theorem direct_hold (K : Kernels) (hK : Exact K) :
        Satisfies (WaitSpec K) (Always (Lift (fun w => WaitInv w ∧ DirectInv w))) :=
      invariantSatisfies (WaitSpec K) (fun w => WaitInv w ∧ DirectInv w)
        (fun s h => ⟨ww_init s h, wd_init s h⟩)
        (fun s t hs ht => Exists.elim ht (fun e he =>
          Eq.mpr (congrArg (fun x => WaitInv x ∧ DirectInv x) he) ⟨ww_step K hK s hs.1 e, wd_step K hK s hs.1 hs.2 e⟩))

    -- W-d: a write is overtaken only by another Mac's value, or by this Mac's own
    -- later edit; never by one of this Mac's earlier publishes, unless a take-back.
    theorem overtaken_by_another (K : Kernels) (hK : Exact K) :
        Satisfies (WaitSpec K) (Always (Lift OvertakenByAnother)) := by
      intro b runs n
      have hi : WaitInv (drop b n 0) ∧ DirectInv (drop b n 0) := direct_hold K hK b runs n
      exact fun i h => ⟨hi.1.overtaken i h, (hi.2.overDirect i h).2⟩

    -- W-e, LaterKept, from one more inductive invariant: no write born after a
    -- direct draft in flight drafted is signed or done while that draft is in
    -- flight (a pass defers its name: deferredW), so its keep finds none, and a
    -- write signed or done keeps its value until another write of its name: a
    -- pass's signing (which ends the kept value's hold) or another Mac's write.
    structure LaterInv (w : WWorld) : Prop where
      unsigned : ∀ i d, (w.dphase d = 1 ∨ w.dphase d = 2) → w.nm i = w.dnm d → w.dstamp d ≤ i → w.fate i ≠ 1
      kept : ∀ i, w.fate i = 1 → w.hdirect (w.nm i) ≠ 0 → w.hdirect (w.nm i) ≤ i → w.log (w.nm i) = w.val i

    theorem wl_init (w : WWorld) (h : w = ww0) : LaterInv w := by
      subst h
      exact
        { unsigned := fun i d hp _ _ _ => ww_or_cases hp (fun a => hd_01 a) (fun b => hd_02 b)
          kept := fun i h => by have h' : (0 : Nat) = 1 := h; exact absurd h' (by decide) }

    theorem wl_phase_ne0 (w : WWorld) (d : Nat) (hp : w.dphase d = 1 ∨ w.dphase d = 2) : w.dphase d ≠ 0 :=
      fun z => ww_or_cases hp (fun a => hd_01 (Eq.trans z.symm a)) (fun b => hd_02 (Eq.trans z.symm b))

    -- Verdict::of answers leaves, overtaken or signs, unless the write is done.
    theorem wl_vo (K : Kernels) (hK : Exact K) (d dn so tb : Bool) :
        verdictOf K d dn so tb = 0 ∨ verdictOf K d dn so tb = 2 ∨ verdictOf K d dn so tb = 3 ∨ dn = true := by
      unfold verdictOf
      rw [hK.2.2.2.2.2.1 dn so tb, hK.2.2.2.2.2.2.1 dn so tb]
      cases d <;> cases dn <;> cases so <;> cases tb <;> decide

    theorem wl_pv_undeferred (K : Kernels) (w : WWorld) (n : Nat) (h : passVerdict K w n ≠ 0) :
        deferredW w n = false :=
      ww_ne_true (fun t => h (by
        have hz := ww_pv_wait K w n h
        have e : passVerdict K w n = verdictOf K (deferredW w n) (decide (w.log n = w.val (w.waitOf n)))
            (w.bs (w.waitOf n) (w.log n)) (decide (w.val (w.waitOf n) = 0)) := if_neg hz
        rw [e]
        unfold verdictOf
        rw [if_pos t]))

    -- A pass's verdict 1 (done): the log holds the waiting write's value.
    theorem wl_pv_done (K : Kernels) (hK : Exact K) (w : WWorld) (n i : Nat) (hw : w.waitOf n = i)
        (h0 : passVerdict K w n ≠ 0) (h2 : passVerdict K w n ≠ 2) (h3 : passVerdict K w n ≠ 3) :
        w.log n = w.val i := by
      have hz := ww_pv_wait K w n h0
      have e : passVerdict K w n = verdictOf K (deferredW w n) (decide (w.log n = w.val (w.waitOf n)))
          (w.bs (w.waitOf n) (w.log n)) (decide (w.val (w.waitOf n) = 0)) := if_neg hz
      have v := wl_vo K hK (deferredW w n) (decide (w.log n = w.val (w.waitOf n)))
        (w.bs (w.waitOf n) (w.log n)) (decide (w.val (w.waitOf n) = 0))
      rw [← e] at v
      have dn : decide (w.log n = w.val (w.waitOf n)) = true := ww_or_cases v (fun a => absurd a h0)
        (fun b => ww_or_cases b (fun a => absurd a h2) (fun c => ww_or_cases c (fun a => absurd a h3) (fun x => x)))
      rw [← hw]
      exact hd_dec dn

    theorem wl_lw_fate1 (K : Kernels) (S : Nat → Bool) (w : WWorld) (i : Nat) (h : (landW K S w).fate i = 1) :
        w.fate i = 1 := by
      rw [ww_lw_fate_def] at h
      exact ww_by_cases ((ww_c1 K S w i || ww_c2 S w i) = true)
        (fun c => by rw [if_pos c] at h; exact absurd h (by decide))
        (fun c => by rw [if_neg c] at h; exact h)

    theorem wl_landW (K : Kernels) (S : Nat → Bool) (w : WWorld) (hL : LaterInv w) : LaterInv (landW K S w) :=
      { unsigned := fun i d hp hn hs h => hL.unsigned i d hp hn hs (wl_lw_fate1 K S w i h)
        kept := fun i h => hL.kept i (wl_lw_fate1 K S w i h) }

    -- publish: its new writes are born unsigned (fateStored: a signed write is born).
    theorem wl_pub_old (K : Kernels) (S : Nat → Bool) (w : WWorld) (hi : WaitInv w) (k i : Nat)
        (h : (landW K S w).fate i = 1) : ¬ (w.next ≤ i ∧ i < w.next + k) ∧ w.fate i = 1 :=
      have f := wl_lw_fate1 K S w i h
      ⟨fun r => hd_ltIrr _ _ r.1 (hi.fateStored i f).1, f⟩

    theorem wl_pub_kept (K : Kernels) (w : WWorld) (hi : WaitInv w) (hL : LaterInv w) (S : Nat → Bool) (v k : Nat)
        (names vals : Nat → Nat) (bases : Nat → Nat → Bool) (i : Nat)
        (h : (pushW (landW K S w) v k names vals bases).fate i = 1)
        (h1 : (pushW (landW K S w) v k names vals bases).hdirect ((pushW (landW K S w) v k names vals bases).nm i) ≠ 0)
        (h2 : (pushW (landW K S w) v k names vals bases).hdirect ((pushW (landW K S w) v k names vals bases).nm i) ≤ i) :
        (pushW (landW K S w) v k names vals bases).log ((pushW (landW K S w) v k names vals bases).nm i)
          = (pushW (landW K S w) v k names vals bases).val i := by
      have o := wl_pub_old K S w hi k i h
      have e := ww_push_old (landW K S w) v k names vals bases i o.1
      have en : (pushW (landW K S w) v k names vals bases).nm i = w.nm i := e.1
      have eq : (pushW (landW K S w) v k names vals bases).val i = w.val i := (e.2).1
      have h1' : (w.hdirect (w.nm i) ≠ 0) := by rw [← en]; exact h1
      have h2' : (w.hdirect (w.nm i) ≤ i) := by rw [← en]; exact h2
      show w.log ((pushW (landW K S w) v k names vals bases).nm i) = (pushW (landW K S w) v k names vals bases).val i
      rw [en, eq]
      exact hL.kept i o.2 h1' h2'

    theorem wl_publish (K : Kernels) (w : WWorld) (hi : WaitInv w) (hL : LaterInv w) (S : Nat → Bool) (v k : Nat)
        (names vals : Nat → Nat) (bases : Nat → Nat → Bool) : LaterInv (pushW (landW K S w) v k names vals bases) :=
      { unsigned := fun i d hp hn hs h =>
          have o := wl_pub_old K S w hi k i h
          hL.unsigned i d hp (Eq.trans (ww_push_old (landW K S w) v k names vals bases i o.1).1.symm hn) hs o.2
        kept := wl_pub_kept K w hi hL S v k names vals bases }

    theorem wl_dropped (w : WWorld) (v : Nat) (hL : LaterInv w) : LaterInv (ww_dropW w v) :=
      have back : ∀ i, (ww_dropW w v).fate i = 1 → w.fate i = 1 := fun i h => ww_or_cases (ww_drop_cases w v i)
        (fun a => absurd (Eq.trans a.2.1.symm h) (by decide)) (fun b => Eq.trans b.2.symm h)
      { unsigned := fun i d hp hn hs h => hL.unsigned i d hp hn hs (back i h)
        kept := fun i h => hL.kept i (back i h) }

    -- signPass: a write it signs or finds done is of a name no direct draft in
    -- flight writes (deferredW); one it signs ends the kept value's hold, and one
    -- it finds done has the log's value.
    theorem wl_signBody (K : Kernels) (hK : Exact K) (w : WWorld) (hD : DirectInv w) (hL : LaterInv w) :
        LaterInv (signBody K w) :=
      { unsigned := fun i d hp hn hs h => ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
          (fun c => ww_by_cases (passVerdict K w (w.nm i) = 0)
            (fun h0 => hL.unsigned i d hp hn hs (Eq.trans (ww_sb_fate_w0 K w i c.1 c.2 h0).symm h))
            (fun h0 => wl_deferred_held w (w.nm i) d (wl_pv_undeferred K w (w.nm i) h0)
              (hD.dBorn d (wl_phase_ne0 w d hp)) hp hn.symm))
          (fun c => hL.unsigned i d hp hn hs (Eq.trans (ww_sb_fate_other K w i c).symm h))
        kept := fun i h h1 h2 => ww_by_cases (passVerdict K w (w.nm i) = 3)
          (fun h3 => absurd (show (signBody K w).hdirect (w.nm i) = 0 from if_pos h3) h1)
          (fun h3 => by
            have eh : (signBody K w).hdirect (w.nm i) = w.hdirect (w.nm i) := if_neg h3
            have el : (signBody K w).log (w.nm i) = w.log (w.nm i) := if_neg h3
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : ((signBody K w).hdirect (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show (signBody K w).log (w.nm i) = w.val i
            rw [el]
            exact ww_by_cases (¬ (i = 0) ∧ w.waitOf (w.nm i) = i)
              (fun c => ww_by_cases (passVerdict K w (w.nm i) = 0)
                (fun h0 => hL.kept i (Eq.trans (ww_sb_fate_w0 K w i c.1 c.2 h0).symm h) h1' h2')
                (fun h0 => ww_by_cases (passVerdict K w (w.nm i) = 2)
                  (fun h22 => absurd (Eq.trans (ww_sb_fate_w2 K w i c.1 c.2 h0 h22).symm h) (by decide))
                  (fun h22 => wl_pv_done K hK w (w.nm i) i c.2 h0 h22 h3)))
              (fun c => hL.kept i (Eq.trans (ww_sb_fate_other K w i c).symm h) h1' h2')) }

    theorem wl_other (w : WWorld) (n x : Nat) (hL : LaterInv w) :
        LaterInv { w with log := upd w.log n x, holder := upd w.holder n 0, hdirect := upd w.hdirect n 0 } :=
      { unsigned := hL.unsigned
        kept := fun i h h1 h2 => ww_by_cases (w.nm i = n)
          (fun e => absurd (show upd w.hdirect n 0 (w.nm i) = 0 from if_pos e) h1)
          (fun e => by
            have eh : upd w.hdirect n 0 (w.nm i) = w.hdirect (w.nm i) := if_neg e
            have el : upd w.log n x (w.nm i) = w.log (w.nm i) := if_neg e
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : (upd w.hdirect n 0 (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show upd w.log n x (w.nm i) = w.val i
            rw [el]
            exact hL.kept i h h1' h2') }

    -- directDraft: stamped after every born write, and a signed write is born.
    theorem wl_draftD (w : WWorld) (hi : WaitInv w) (hL : LaterInv w) (n x : Nat) :
        LaterInv { w with dnext := w.dnext + 1, dnm := upd w.dnm w.dnext n, dval := upd w.dval w.dnext x,
                          dstamp := upd w.dstamp w.dnext w.next, dphase := upd w.dphase w.dnext 1 } :=
      { unsigned := fun i d hp hn hs h => ww_by_cases (d = w.dnext)
          (fun e => by
            have t : (upd w.dstamp w.dnext w.next d ≤ i) := hs
            rw [e, hd_updS] at t
            exact hd_ltIrr _ _ t (hi.fateStored i h).1)
          (fun e => by
            have tp : (w.dphase d = 1 ∨ w.dphase d = 2) := ww_or_cases hp
              (fun a => Or.inl (Eq.trans (hd_updO w.dphase w.dnext d 1 e).symm a))
              (fun b => Or.inr (Eq.trans (hd_updO w.dphase w.dnext d 1 e).symm b))
            have tn : (w.nm i = w.dnm d) := Eq.trans hn (hd_updO w.dnm w.dnext d n e)
            have ts : (w.dstamp d ≤ i) := by
              have u : (upd w.dstamp w.dnext w.next d ≤ i) := hs
              rw [hd_updO w.dstamp w.dnext d w.next e] at u
              exact u
            exact hL.unsigned i d tp tn ts h)
        kept := hL.kept }

    theorem wl_holdD (w : WWorld) (hL : LaterInv w) (d : Nat) (g : w.dphase d = 1) :
        LaterInv { w with dphase := upd w.dphase d 2 } :=
      { unsigned := fun i e hp hn hs h => ww_by_cases (e = d)
          (fun ed => by
            rw [← ed] at g
            exact hL.unsigned i e (Or.inl g) hn hs h)
          (fun ed => hL.unsigned i e (ww_or_cases hp
            (fun a => Or.inl (Eq.trans (hd_updO w.dphase d e 2 ed).symm a))
            (fun b => Or.inr (Eq.trans (hd_updO w.dphase d e 2 ed).symm b))) hn hs h)
        kept := hL.kept }

    -- keep: no write of its name born after its draft is signed or done
    -- (unsigned), so its kept value takes the place of no later edit.
    theorem wl_keep (w : WWorld) (hL : LaterInv w) (d : Nat) (g : w.dphase d = 2) : LaterInv (keepBody w d) :=
      { unsigned := fun i e hp hn hs h => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 1 ∨ upd w.dphase d 3 e = 2) := hp
            rw [ed, hd_updS] at t
            exact ww_or_cases t (fun a => absurd a (by decide)) (fun b => absurd b (by decide)))
          (fun ed => hL.unsigned i e (ww_or_cases hp
            (fun a => Or.inl (Eq.trans (hd_updO w.dphase d e 3 ed).symm a))
            (fun b => Or.inr (Eq.trans (hd_updO w.dphase d e 3 ed).symm b))) hn hs h)
        kept := fun i h h1 h2 => ww_by_cases (w.nm i = w.dnm d)
          (fun en => by
            have t : (upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) ≤ i) := h2
            rw [en, hd_updS] at t
            exact absurd h (hL.unsigned i d (Or.inr g) en t))
          (fun en => by
            have eh : upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) = w.hdirect (w.nm i) := if_neg en
            have el : upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.log (w.nm i) := if_neg en
            have h1' : (w.hdirect (w.nm i) ≠ 0) := fun z => h1 (Eq.trans eh z)
            have h2' : (w.hdirect (w.nm i) ≤ i) := by
              have t : (upd w.hdirect (w.dnm d) (w.dstamp d) (w.nm i) ≤ i) := h2
              rw [eh] at t
              exact t
            show upd w.log (w.dnm d) (w.dval d) (w.nm i) = w.val i
            rw [el]
            exact hL.kept i h h1' h2') }

    theorem wl_unhold (w : WWorld) (hL : LaterInv w) (d : Nat) : LaterInv { w with dphase := upd w.dphase d 3 } :=
      { unsigned := fun i e hp hn hs h => ww_by_cases (e = d)
          (fun ed => by
            have t : (upd w.dphase d 3 e = 1 ∨ upd w.dphase d 3 e = 2) := hp
            rw [ed, hd_updS] at t
            exact ww_or_cases t (fun a => absurd a (by decide)) (fun b => absurd b (by decide)))
          (fun ed => hL.unsigned i e (ww_or_cases hp
            (fun a => Or.inl (Eq.trans (hd_updO w.dphase d e 3 ed).symm a))
            (fun b => Or.inr (Eq.trans (hd_updO w.dphase d e 3 ed).symm b))) hn hs h)
        kept := hL.kept }

    theorem wl_step (K : Kernels) (hK : Exact K) (w : WWorld) (hi : WaitInv w) (hD : DirectInv w) (hL : LaterInv w)
        (e : WEvent) : LaterInv (wstep K w e) := by
      cases e with
      | publish v k names vals bases opened =>
        exact ww_by_cases (publishOk K w v k names vals bases opened = true)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_pos hg]
            exact wl_publish K w hi hL opened v k names vals bases)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_neg hg]
            exact hL)
      | stored v =>
        exact ww_by_cases (w.refused v = true)
          (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_pos hr]; exact hL)
          (fun hr => by
            rw [show wstep K w (WEvent.stored v) = _ from if_neg hr]
            exact ⟨hL.unsigned, hL.kept⟩)
      | dropped v =>
        exact ww_by_cases (w.stored v = true)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_pos hs]; exact hL)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_neg hs]; exact wl_dropped w v hL)
      | land S =>
        exact ww_by_cases ((!(w.unread) && allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i))) = true)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_pos hg]; exact wl_landW K S w hL)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_neg hg]; exact hL)
      | signPass opens =>
        exact ww_by_cases (K.passSigns w.due opens (!w.unread) = true)
          (fun hps => by
            show LaterInv (signW K opens w)
            rw [show signW K opens w = signBody K w from if_pos hps]
            exact wl_signBody K hK w hD hL)
          (fun hps => by
            show LaterInv (signW K opens w)
            rw [show signW K opens w = w from if_neg hps]
            exact hL)
      | settlePass => exact ⟨hL.unsigned, hL.kept⟩
      | other n x => exact wl_other w n x hL
      | due b => exact ⟨hL.unsigned, hL.kept⟩
      | unreadable b => exact ⟨hL.unsigned, hL.kept⟩
      | directDraft n x =>
        exact ww_by_cases (turnFree w = true)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_pos g]; exact wl_draftD w hi hL n x)
          (fun g => by rw [show wstep K w (WEvent.directDraft n x) = _ from if_neg g]; exact hL)
      | directHold d =>
        exact ww_by_cases (decide (w.dphase d = 1) = true)
          (fun g => by
            rw [show wstep K w (WEvent.directHold d) = _ from if_pos g]
            exact wl_holdD w hL d (hd_dec g))
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_neg g]; exact hL)
      | keep d =>
        exact ww_by_cases (K.draftedKeeps (decide (w.dphase d = 2)) = true)
          (fun g => by
            rw [show wstep K w (WEvent.keep d) = _ from if_pos g]
            exact wl_keep w hL d (hd_dec (drafted_keeps_true K hK _ g)))
          (fun g => by rw [show wstep K w (WEvent.keep d) = _ from if_neg g]; exact hL)
      | unhold d =>
        exact ww_by_cases ((decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true)
          (fun g => by
            rw [show wstep K w (WEvent.unhold d) = _ from if_pos g]
            exact wl_unhold w hL d)
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_neg g]; exact hL)
      | holdUnreadable b => exact ⟨hL.unsigned, hL.kept⟩

    -- WaitInv, DirectInv and LaterInv hold together in every state of every run.
    theorem later_hold (K : Kernels) (hK : Exact K) :
        Satisfies (WaitSpec K) (Always (Lift (fun w => (WaitInv w ∧ DirectInv w) ∧ LaterInv w))) :=
      invariantSatisfies (WaitSpec K) (fun w => (WaitInv w ∧ DirectInv w) ∧ LaterInv w)
        (fun s h => ⟨⟨ww_init s h, wd_init s h⟩, wl_init s h⟩)
        (fun s t hs ht => Exists.elim ht (fun e he =>
          Eq.mpr (congrArg (fun x => (WaitInv x ∧ DirectInv x) ∧ LaterInv x) he)
            ⟨⟨ww_step K hK s hs.1.1 e, wd_step K hK s hs.1.1 hs.1.2 e⟩, wl_step K hK s hs.1.1 hs.1.2 hs.2 e⟩))

    -- W-e: an earlier edit's keep never takes the place of a later edit a pass signed.
    theorem later_kept (K : Kernels) (hK : Exact K) : Satisfies (WaitSpec K) (Always (Lift LaterKept)) := by
      intro b runs n
      have hi : (WaitInv (drop b n 0) ∧ DirectInv (drop b n 0)) ∧ LaterInv (drop b n 0) := later_hold K hK b runs n
      exact hi.2.kept

    -- W-b: a step changes a name's value only by signing its waiting write over
    -- one of its bases, never as a take-back, or as another Mac's write.
    theorem ww_cas_sign (K : Kernels) (hK : Exact K) (w : WWorld) (opens : Bool) :
        CasStep w (wstep K w (WEvent.signPass opens)) :=
      ww_by_cases (K.passSigns w.due opens (!w.unread) = true)
        (fun hps => by
          show CasStep w (signW K opens w)
          rw [show signW K opens w = signBody K w from if_pos hps]
          intro n
          exact ww_by_cases (passVerdict K w n = 3)
            (fun h3 => by
              have p := ww_pv3 K hK w n h3
              exact Or.inr (Or.inr ⟨p.1, p.2.2.2.1, p.2.2.2.2, Eq.trans (ww_sb_log_def K w n) (if_pos h3)⟩))
            (fun h3 => Or.inl (Eq.trans (ww_sb_log_def K w n) (if_neg h3))))
        (fun hps => by
          show CasStep w (signW K opens w)
          rw [show signW K opens w = w from if_neg hps]
          exact fun n => Or.inl rfl)

    theorem ww_cas (K : Kernels) (hK : Exact K) (w : WWorld) (e : WEvent) : CasStep w (wstep K w e) := by
      cases e with
      | publish v k names vals bases opened =>
        exact ww_by_cases (publishOk K w v k names vals bases opened = true)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_pos hg]
            exact fun n => Or.inl rfl)
          (fun hg => by
            rw [show wstep K w (WEvent.publish v k names vals bases opened) = _ from if_neg hg]
            exact fun n => Or.inl rfl)
      | stored v =>
        exact ww_by_cases (w.refused v = true)
          (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_pos hr]; exact fun n => Or.inl rfl)
          (fun hr => by rw [show wstep K w (WEvent.stored v) = _ from if_neg hr]; exact fun n => Or.inl rfl)
      | dropped v =>
        exact ww_by_cases (w.stored v = true)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_pos hs]; exact fun n => Or.inl rfl)
          (fun hs => by rw [show wstep K w (WEvent.dropped v) = _ from if_neg hs]; exact fun n => Or.inl rfl)
      | land S =>
        exact ww_by_cases ((!(w.unread) && allN w.next (fun i => !(w.pend i && S (w.ver i)) || w.stored (w.ver i))) = true)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_pos hg]; exact fun n => Or.inl rfl)
          (fun hg => by rw [show wstep K w (WEvent.land S) = _ from if_neg hg]; exact fun n => Or.inl rfl)
      | signPass opens => exact ww_cas_sign K hK w opens
      | settlePass => exact fun n => Or.inl rfl
      | other m x =>
        exact fun n => ww_by_cases (n = m)
          (fun e => Or.inr (Or.inl (show (if n = m then 0 else w.holder n) = 0 from if_pos e)))
          (fun e => Or.inl (show (if n = m then x else w.log n) = w.log n from if_neg e))
      | due b => exact fun n => Or.inl rfl
      | unreadable b => exact fun n => Or.inl rfl
      | directDraft m x =>
        exact ww_by_cases (turnFree w = true)
          (fun g => by rw [show wstep K w (WEvent.directDraft m x) = _ from if_pos g]; exact fun n => Or.inl rfl)
          (fun g => by rw [show wstep K w (WEvent.directDraft m x) = _ from if_neg g]; exact fun n => Or.inl rfl)
      | directHold d =>
        exact ww_by_cases (decide (w.dphase d = 1) = true)
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_pos g]; exact fun n => Or.inl rfl)
          (fun g => by rw [show wstep K w (WEvent.directHold d) = _ from if_neg g]; exact fun n => Or.inl rfl)
      | keep d =>
        exact ww_by_cases (K.draftedKeeps (decide (w.dphase d = 2)) = true)
          (fun g => by
            rw [show wstep K w (WEvent.keep d) = _ from if_pos g]
            exact fun n => ww_by_cases (n = w.dnm d)
              (fun e => Or.inr (Or.inl (show (if n = w.dnm d then 0 else w.holder n) = 0 from if_pos e)))
              (fun e => Or.inl (show (if n = w.dnm d then w.dval d else w.log n) = w.log n from if_neg e)))
          (fun g => by rw [show wstep K w (WEvent.keep d) = _ from if_neg g]; exact fun n => Or.inl rfl)
      | unhold d =>
        exact ww_by_cases ((decide (w.dphase d = 1) || decide (w.dphase d = 2)) = true)
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_pos g]; exact fun n => Or.inl rfl)
          (fun g => by rw [show wstep K w (WEvent.unhold d) = _ from if_neg g]; exact fun n => Or.inl rfl)
      | holdUnreadable b => exact fun n => Or.inl rfl

    theorem cas_step (K : Kernels) (hK : Exact K) : Satisfies (WaitSpec K) (Always (Act CasStep)) :=
      actionSatisfies (WaitSpec K) CasStep
        (fun s t ht => Exists.elim ht (fun e he => Eq.mpr (congrArg (CasStep s) he) (ww_cas K hK s e)))
        (fun s n => Or.inl rfl)

    -- ===========================================================================
    -- 10. The rules A3 and W3 replaced, each a run the kernel evaluates. A write
    --     is lost where it is not accounted for though its version is stored.
    -- ===========================================================================

    def accountedB (w : WWorld) (i : Nat) : Bool :=
      w.pend i || decide (w.waitOf (w.nm i) = i) || decide (w.fate i = 1) || decide (w.fate i = 2)
        || decide (w.fate i = 3)

    def one1 (x : Nat) : Nat → Nat := fun _ => x
    def baseIs (x : Nat) : Nat → Nat → Bool := fun _ y => y == x
    def opensNone : Nat → Bool := fun _ => false
    def only (v : Nat) : Nat → Bool := fun u => u == v

    -- A3's bound (pending-by-count): a publish past the bound drops the oldest set,
    -- whatever its version. Here the bound is one set: the shape of eight.
    def evictOldest (w : WWorld) : WWorld :=
      let o := lastN w.next (fun i => w.pend i && allN i (fun j => !(w.pend j)))
      { w with pend := fun i => w.pend i && !(decide (i = o) && !(decide (o = 0))),
               fate := fun i => if decide (i = o) && !(decide (o = 0)) then 4 else w.fate i }

    def wstepByCount (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if publishOk K w v k names vals bases opened = true then pushW (evictOldest (landW K opened w)) v k names vals bases
          else w
      | e => wstep K w e

    def wrunByCount (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunByCount K (wstepByCount K w e) es

    def twoPublishes : List WEvent :=
      [ WEvent.publish 1 1 (one1 5) (one1 11) (baseIs 0) opensNone,
        WEvent.publish 2 1 (one1 6) (one1 7) (baseIs 0) opensNone,
        WEvent.stored 1, WEvent.land (only 1) ]

    theorem every_set_stays_until_its_proof : accountedB (wrun spec ww0 twoPublishes) 1 = true := rfl
    theorem v1_is_stored : (wrun spec ww0 twoPublishes).stored 1 = true := rfl
    theorem the_bound_drops_a_stored_versions_write : accountedB (wrunByCount spec ww0 twoPublishes) 1 = false := rfl

    -- W3's fail-closed read (unread-as-none): a set that does not open was read as
    -- none, and its file removed.
    def wstepUnreadAsNone (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.land S =>
          if w.unread then
            { w with pend := fun _ => false, fate := fun i => if w.pend i then 4 else w.fate i }
          else wstep K w (WEvent.land S)
      | e => wstep K w e

    def wrunUnreadAsNone (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunUnreadAsNone K (wstepUnreadAsNone K w e) es

    def unreadSet : List WEvent :=
      [ WEvent.publish 1 1 (one1 5) (one1 11) (baseIs 0) opensNone,
        WEvent.unreadable true, WEvent.stored 1, WEvent.land (only 1) ]

    theorem an_unread_set_stays : accountedB (wrun spec ww0 unreadSet) 1 = true := rfl
    theorem it_lands_once_it_reads :
        (wrun spec ww0 (unreadSet ++ [WEvent.unreadable false, WEvent.land (only 1)])).waitOf 5 = 1 := rfl
    theorem an_unread_set_read_as_none_loses_it : accountedB (wrunUnreadAsNone spec ww0 unreadSet) 1 = false := rfl

    -- A3 518a0b5838 #2 (wait-at-once): writes kept as waiting before the PUT are
    -- signed though no server took their version.
    def wstepWaitAtOnce (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if publishOk K w v k names vals bases opened = true then
            let p := pushW w v k names vals bases
            { p with pend := fun i => if w.next ≤ i ∧ i < w.next + k then false else p.pend i,
                     waitOf := fun n => if decide (k = 1) && decide (n = names 0) then w.next else w.waitOf n }
          else w
      | e => wstep K w e

    def wrunWaitAtOnce (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunWaitAtOnce K (wstepWaitAtOnce K w e) es

    def failedPublish : List WEvent :=
      [ WEvent.publish 1 1 (one1 5) (one1 11) (baseIs 0) opensNone, WEvent.dropped 1, WEvent.signPass true ]

    theorem pending_until_stored_signs_nothing : (wrun spec ww0 failedPublish).log 5 = 0 := rfl
    theorem waiting_at_once_signs_a_refused_edit : (wrunWaitAtOnce spec ww0 failedPublish).log 5 = 11 := rfl

    -- A3 657194f2e1 #1 (land-drops-all-sets): landing one set dropped every other;
    -- v1, stored on another server, lost its write of name 6.
    def wstepDropAll (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.land S =>
          let l := wstep K w (WEvent.land S)
          if anyN w.next (fun i => w.pend i && S (w.ver i)) then
            { l with pend := fun _ => false, fate := fun i => if l.pend i then 4 else l.fate i }
          else l
      | e => wstep K w e

    def wrunDropAll (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunDropAll K (wstepDropAll K w e) es

    def names56 : Nat → Nat := fun a => if a = 0 then 5 else 6
    def vals117 : Nat → Nat := fun a => if a = 0 then 11 else 7

    def twoServers : List WEvent :=
      [ WEvent.publish 1 2 names56 vals117 (baseIs 0) opensNone,
        WEvent.publish 2 1 (one1 5) (one1 12) (baseIs 0) opensNone,
        WEvent.stored 1, WEvent.stored 2, WEvent.land (only 2), WEvent.land (only 1) ]

    theorem land_by_version_keeps_only6 : accountedB (wrun spec ww0 twoServers) 2 = true := rfl
    theorem the_later_write_of_5_waits : (wrun spec ww0 twoServers).waitOf 5 = 3 := rfl
    theorem dropping_the_other_sets_loses_only6 : accountedB (wrunDropAll spec ww0 twoServers) 2 = false := rfl

    -- The model's counterexample to a later write winning, now gone (race-undeferred):
    -- v1's set lands and a pass runs before v2's set, the person's later save of
    -- the same name from the same text, lands. The pass leaves v1's write for v2's
    -- set, so the later value is signed; without that, the pass signs v1's and
    -- drops v2's as overtaken by this Mac's own earlier write.
    def signBetween : List WEvent :=
      [ WEvent.publish 1 1 (one1 5) (one1 11) (baseIs 0) opensNone,
        WEvent.publish 2 1 (one1 5) (one1 12) (baseIs 0) opensNone,
        WEvent.stored 1, WEvent.stored 2, WEvent.land (only 1), WEvent.signPass true, WEvent.settlePass,
        WEvent.land (only 2), WEvent.signPass true, WEvent.settlePass ]

    def signUndeferred (K : Kernels) (opens : Bool) (w : WWorld) : WWorld :=
      signW K opens { w with pend := fun _ => false }
    def wstepUndeferred (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.signPass opens =>
          let s := signUndeferred K opens w
          { s with pend := w.pend }
      | e => wstep K w e

    def wrunUndeferred (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunUndeferred K (wstepUndeferred K w e) es

    theorem the_later_write_wins : (wrun spec ww0 signBetween).log 5 = 12 := rfl
    theorem nothing_is_overtaken : (wrun spec ww0 signBetween).fate 3 = 0 := rfl
    theorem undeferred_keeps_the_earlier_write : (wrunUndeferred spec ww0 signBetween).log 5 = 11 := rfl
    theorem undeferred_drops_the_later_as_overtaken_by_its_own_mac :
        (wrunUndeferred spec ww0 signBetween).fate 2 = 2 ∧ (wrunUndeferred spec ww0 signBetween).overBy 2 = 1 := ⟨rfl, rfl⟩

    -- A pass over a set that does not open, read as none (pass-unread): the set's
    -- names are deferred by nothing, so v1's write is signed under v2's later edit,
    -- and v2's is dropped as overtaken by this Mac's own write.
    def wstepPassUnread (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.signPass opens =>
          if w.unread then
            let s := signW K opens { w with unread := false, pend := fun _ => false }
            { s with unread := true, pend := w.pend }
          else wstep K w (WEvent.signPass opens)
      | e => wstep K w e

    def wrunPassUnread (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunPassUnread K (wstepPassUnread K w e) es

    def passOverUnread : List WEvent :=
      [ WEvent.publish 1 1 (one1 5) (one1 11) (baseIs 0) opensNone,
        WEvent.publish 2 1 (one1 5) (one1 12) (baseIs 0) opensNone,
        WEvent.stored 1, WEvent.stored 2, WEvent.land (only 1), WEvent.unreadable true,
        WEvent.signPass true, WEvent.settlePass, WEvent.unreadable false,
        WEvent.land (only 2), WEvent.signPass true, WEvent.settlePass ]

    theorem a_pass_over_an_unread_set_signs_nothing : (wrun spec ww0 passOverUnread).log 5 = 12 := rfl
    theorem read_as_none_it_reverts_the_later_edit : (wrunPassUnread spec ww0 passOverUnread).log 5 = 11 := rfl

    -- The direct draft's rules (draft_turn, held_values, keep_drafted_until,
    -- draft_document_diff_with's held refusal). Each run: this Mac drafts name 5
    -- to 11 directly (direct draft 1), then a publish writes name 5 to 12 as a
    -- waiting write (write 1); the shipped rules sign 12, each mutant loses it to
    -- this Mac's own earlier value 11 (overtaken with overD 1 = 1, a draft from
    -- before write 1). A run offers the mutant's publish first (bases: the log's
    -- 0), then the shipped one's (bases also 11); version 1 takes the one each
    -- rule set accepts.
    def basesOf (x y : Nat) : Nat → Nat → Bool := fun _ z => z == x || z == y
    def mutantPublish : WEvent := WEvent.publish 1 1 (one1 5) (one1 12) (baseIs 0) opensNone
    def shippedPublish : WEvent := WEvent.publish 1 1 (one1 5) (one1 12) (basesOf 0 11) opensNone
    def landKeepSign : List WEvent :=
      [ WEvent.stored 1, WEvent.land (only 1), WEvent.keep 1, WEvent.signPass true ]

    -- BASES_WITHOUT_HELD: a waiting write's bases leave out the held draft's value.
    def wstepBasesWithoutHeld (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if (pubBase K w.holdUnread w v k names vals bases opened && turnFree w) = true then
            pushW (landW K opened w) v k names vals bases
          else w
      | e => wstep K w e
    def wrunBasesWithoutHeld (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunBasesWithoutHeld K (wstepBasesWithoutHeld K w e) es

    def heldBeforeWrite : List WEvent :=
      [ WEvent.directDraft 5 11, WEvent.directHold 1, mutantPublish, shippedPublish ] ++ landKeepSign

    theorem held_values_are_among_the_bases : (wrun spec ww0 heldBeforeWrite).log 5 = 12 := rfl
    theorem bases_without_the_held_value_lose_the_later_edit :
        (wrunBasesWithoutHeld spec ww0 heldBeforeWrite).log 5 = 11
          ∧ (wrunBasesWithoutHeld spec ww0 heldBeforeWrite).fate 1 = 2
          ∧ (wrunBasesWithoutHeld spec ww0 heldBeforeWrite).overD 1 = 1 := ⟨rfl, rfl, rfl⟩

    -- DRAFTS_AT_ONCE: a publish drafts while a direct draft is between its draft
    -- and its hold, so it reads no held value of it.
    def wstepDraftsAtOnce (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if (pubBase K w.holdUnread w v k names vals bases opened && heldIn w k names bases) = true then
            pushW (landW K opened w) v k names vals bases
          else w
      | e => wstep K w e
    def wrunDraftsAtOnce (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunDraftsAtOnce K (wstepDraftsAtOnce K w e) es

    def draftInTurn : List WEvent :=
      [ WEvent.directDraft 5 11, mutantPublish, WEvent.directHold 1, shippedPublish ] ++ landKeepSign

    theorem a_publish_waits_for_the_drafts_turn : (wrun spec ww0 draftInTurn).log 5 = 12 := rfl
    theorem drafting_at_once_loses_the_later_edit :
        (wrunDraftsAtOnce spec ww0 draftInTurn).log 5 = 11
          ∧ (wrunDraftsAtOnce spec ww0 draftInTurn).fate 1 = 2
          ∧ (wrunDraftsAtOnce spec ww0 draftInTurn).overD 1 = 1 := ⟨rfl, rfl, rfl⟩

    -- KEEPS_SUPERSEDED: a version's records are kept though its hold is gone (a
    -- descent superseded it, or a refusal took it), after a publish drafted over
    -- what the log holds without them.
    def wstepKeepsSuperseded (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.keep d => if decide (w.dphase d = 0) = true then w else keepBody w d
      | e => wstep K w e
    def wrunKeepsSuperseded (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunKeepsSuperseded K (wstepKeepsSuperseded K w e) es

    def keepAfterUnhold : List WEvent :=
      [ WEvent.directDraft 5 11, WEvent.directHold 1, WEvent.unhold 1, mutantPublish ] ++ landKeepSign

    theorem a_gone_hold_keeps_nothing : (wrun spec ww0 keepAfterUnhold).log 5 = 12 := rfl
    theorem keeping_a_superseded_hold_loses_the_later_edit :
        (wrunKeepsSuperseded spec ww0 keepAfterUnhold).log 5 = 11
          ∧ (wrunKeepsSuperseded spec ww0 keepAfterUnhold).fate 1 = 2
          ∧ (wrunKeepsSuperseded spec ww0 keepAfterUnhold).overD 1 = 1 := ⟨rfl, rfl, rfl⟩

    -- An unread hold read as none (W3, held-unread): a held draft that does not
    -- read contributes no value to the bases, and the draft goes on.
    def wstepHeldUnreadAsNone (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.publish v k names vals bases opened =>
          if (pubBase K false w v k names vals bases opened
              && (turnFree w && (w.holdUnread || heldIn w k names bases))) = true then
            pushW (landW K opened w) v k names vals bases
          else w
      | e => wstep K w e
    def wrunHeldUnreadAsNone (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunHeldUnreadAsNone K (wstepHeldUnreadAsNone K w e) es

    def unreadHold : List WEvent :=
      [ WEvent.directDraft 5 11, WEvent.directHold 1, WEvent.holdUnreadable true, mutantPublish,
        WEvent.holdUnreadable false, shippedPublish ] ++ landKeepSign

    theorem an_unread_hold_refuses_the_draft : (wrun spec ww0 unreadHold).log 5 = 12 := rfl
    theorem an_unread_hold_read_as_none_loses_the_later_edit :
        (wrunHeldUnreadAsNone spec ww0 unreadHold).log 5 = 11
          ∧ (wrunHeldUnreadAsNone spec ww0 unreadHold).fate 1 = 2
          ∧ (wrunHeldUnreadAsNone spec ww0 unreadHold).overD 1 = 1 := ⟨rfl, rfl, rfl⟩

    -- HELD_UNDEFERRED: a pass that defers only the names of pending writes signs a
    -- later edit (12) while this Mac's earlier direct draft of its name (11) is in
    -- flight; the draft's keep then takes the later edit's place.
    def wstepHeldUndeferred (K : Kernels) (w : WWorld) : WEvent → WWorld
      | WEvent.signPass opens =>
          let s := signW K opens { w with dphase := fun _ => 0 }
          { s with dphase := w.dphase }
      | e => wstep K w e
    def wrunHeldUndeferred (K : Kernels) (w : WWorld) : List WEvent → WWorld
      | [] => w
      | e :: es => wrunHeldUndeferred K (wstepHeldUndeferred K w e) es

    def passBeforeKeep : List WEvent :=
      [ WEvent.directDraft 5 11, WEvent.directHold 1, shippedPublish, WEvent.stored 1, WEvent.land (only 1),
        WEvent.signPass true, WEvent.settlePass, WEvent.keep 1, WEvent.signPass true, WEvent.settlePass ]

    theorem a_pass_waits_for_the_draft_in_flight :
        (wrun spec ww0 passBeforeKeep).log 5 = 12 ∧ (wrun spec ww0 passBeforeKeep).fate 1 = 1 := ⟨rfl, rfl⟩
    theorem signing_under_a_draft_in_flight_loses_the_later_edit :
        (wrunHeldUndeferred spec ww0 passBeforeKeep).log 5 = 11
          ∧ (wrunHeldUndeferred spec ww0 passBeforeKeep).fate 1 = 1
          ∧ (wrunHeldUndeferred spec ww0 passBeforeKeep).hdirect 5 = 1 := ⟨rfl, rfl, rfl⟩

    -- ===========================================================================
    -- 11. The answer sheets. Each folds a fixed grid of inputs through the spec
    --     kernels, the folds no_lost_edit.rs runs one for one, and packs the
    --     answers into words (Clean's numerals fit 64 bits). The Rust tests
    --     `no_lost_edit::tests` recompute each from the Rust folds and read the
    --     words below from this file. Until TG12 checks the Rust folds' kernels in
    --     every build, these sheets are what hold the folds to the island; TG12
    --     removes them.
    -- ===========================================================================

    def pack (b : Nat) : List Nat → Nat
      | [] => 0
      | x :: xs => x + b * pack b xs

    def gridFrom (f : Nat → Nat) (start : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => f start :: gridFrom f (start + 1) n

    -- Words of `per` answers each, base b, the first answer lowest; `count` answers.
    def wordsFrom (b per : Nat) (f : Nat → Nat) (start : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => pack b (gridFrom f start per) :: wordsFrom b per f (start + per) n

    def bitOf (b : Bool) : Nat := if b = true then 1 else 0

    -- Gate.of, 12 cases: k%2 named, (k/2)%3 the held sequence below, at or above
    -- the head's (1, 2 or 3 against 2), (k/6)%2 some server took the head.
    def gateCase (k : Nat) : Nat :=
      gateOf spec (decide (k % 2 = 1)) ((k / 2) % 3 + 1) 2 (decide ((k / 6) % 2 = 1))
    def gateSheet : List Nat := [pack 4 (gridFrom gateCase 0 12)]

    -- settle_reach, 86 cases: 0 unknown; then reaches of 0 to 3 URLs (URL x is x),
    -- URL x's digit (base 4) its one-server bit and its took bit.
    def digit4 (c x : Nat) : Nat := if x = 0 then c % 4 else if x = 1 then (c / 4) % 4 else (c / 16) % 4
    def urlsOf : Nat → List Nat
      | 0 => []
      | Nat.succ n => urlsOf n ++ [n]
    def maskOf : List Nat → Nat
      | [] => 0
      | x :: l => (if x = 0 then 1 else if x = 1 then 2 else 4) + maskOf l
    def reachLen (k : Nat) : Nat := if k < 2 then 0 else if k < 6 then 1 else if k < 22 then 2 else 3
    def reachCombo (k : Nat) : Nat := if k < 2 then 0 else if k < 6 then k - 2 else if k < 22 then k - 6 else k - 22
    def reachCode (len : Nat) : Option Reach → Nat
      | none => 0
      | some Reach.unknown => 1
      | some (Reach.known ys) => if ys.length = len then 1 else 2 + maskOf ys
    def reachCase (k : Nat) : Nat :=
      let c := reachCombo k
      let xs := urlsOf (reachLen k)
      let one := fun x => decide (digit4 c x % 2 = 1)
      let took := keepIf (fun x => decide (digit4 c x / 2 = 1)) xs
      reachCode (reachLen k) (settleReach spec one took (if k = 0 then Reach.unknown else Reach.known xs))
    def reachSheet : List Nat := wordsFrom 16 16 reachCase 0 6

    -- publish_forgets, 85 cases: answer lists of 0 to 3, each answer's digit (base
    -- 4) its stored bit and its refusal bit.
    def answersOf (c : Nat) : Nat → List (Bool × Bool)
      | 0 => []
      | Nat.succ n => answersOf c n ++ [(decide (digit4 c n % 2 = 1), decide (digit4 c n / 2 = 1))]
    def forgetsLen (k : Nat) : Nat := if k < 1 then 0 else if k < 5 then 1 else if k < 21 then 2 else 3
    def forgetsCombo (k : Nat) : Nat := if k < 1 then 0 else if k < 5 then k - 1 else if k < 21 then k - 5 else k - 21
    def forgetsCase (k : Nat) : Nat := bitOf (forgets spec (answersOf (forgetsCombo k) (forgetsLen k)))
    def forgetsSheet : List Nat := wordsFrom 2 64 forgetsCase 0 2

    -- Verdict::of, 16 cases (k's bits: deferred, done, stands over, take-back), and
    -- pass_signs, 8 cases (rotation due, opens the newest, every set reads).
    def verdictCase (k : Nat) : Nat :=
      verdictOf spec (decide (k % 2 = 1)) (decide ((k / 2) % 2 = 1)) (decide ((k / 4) % 2 = 1)) (decide ((k / 8) % 2 = 1))
    def passCase (k : Nat) : Nat :=
      bitOf (spec.passSigns (decide (k % 2 = 1)) (decide ((k / 2) % 2 = 1)) (decide ((k / 4) % 2 = 1)))
    def verdictSheet : List Nat := [pack 4 (gridFrom verdictCase 0 16), pack 2 (gridFrom passCase 0 8)]

    -- land, 512 cases: three sets, set s's digit (base 8) its stored bit and
    -- whether it writes name 0 and name 1; set s's write of name n is write
    -- 1 + 2s + n. The answer: per write, whether it stays pending (6 bits), then
    -- per name the set whose write waits now, plus one (0: none), 2 bits each.
    def landWorld (k : Nat) : WWorld :=
      let c := fun s => (k / (if s = 0 then 1 else if s = 1 then 8 else 64)) % 8
      { ww0 with
        next := 7,
        nm := fun i => (i - 1) % 2,
        ver := fun i => (i - 1) / 2 + 1,
        pend := fun i => decide (1 ≤ i) && (decide (i ≤ 6)) && decide ((c ((i - 1) / 2) / (if (i - 1) % 2 = 0 then 2 else 4)) % 2 = 1),
        stored := fun v => decide (1 ≤ v) && decide (v ≤ 3) && decide (c (v - 1) % 2 = 1) }
    def waitSet (i : Nat) : Nat := if i = 0 then 0 else (i - 1) / 2 + 1
    def landCase (k : Nat) : Nat :=
      let w := landWorld k
      let l := landW spec w.stored w
      bitOf (l.pend 1) + 2 * bitOf (l.pend 2) + 4 * bitOf (l.pend 3) + 8 * bitOf (l.pend 4)
        + 16 * bitOf (l.pend 5) + 32 * bitOf (l.pend 6) + 64 * (waitSet (l.waitOf 0) + 4 * waitSet (l.waitOf 1))
    def landSheet : List Nat := wordsFrom 1024 6 landCase 0 86

    -- The words each sheet is, by the kernel's evaluation.
    theorem gateSheet_is : gateSheet =
      [0x0000000000122111] := rfl
    theorem reachSheet_is : reachSheet =
      [0x1141114111011101, 0x1181118111033341, 0x1181118111677781, 0x1181118111677781,
       0x5545554555677781, 0x1181118111033345] := rfl
    theorem forgetsSheet_is : forgetsSheet =
      [0x8000000000008009, 0x8000000000000000] := rfl
    theorem verdictSheet_is : verdictSheet =
      [0x0000000012121312, 0x0000000000000040] := rfl
    theorem landSheet_is : landSheet =
      [0x0400021000100000, 0x0100010000050003, 0x0010045000340002, 0x0510074100611005,
       0x0600822008020080, 0x0120090200860082, 0x0802005200b4200a, 0x0902018020090201,
       0x04300e1300d0300c, 0x0a0280a02805300f, 0x000000a0280a0280, 0x0500034000210001,
       0x0400021000100000, 0x0110050100450003, 0x0200805100741006, 0x0600826008220080,
       0x04200a1200902008, 0x090201802005200b, 0x00300c9020180200, 0x05300f4300e1300d,
       0x0a0280a0280a0280, 0x01401104010a0280, 0x0040105401344012, 0x0540134401214011,
       0x0450161501505014, 0x0240902409055017, 0x0060186409264092, 0x05601b4601a16019,
       0x0842109421184210, 0x01701d0701c94211, 0x0a42905701f4701e, 0x0a4290a4290a4290,
       0x0700c2300c0300c0, 0x0300c0300c0700c2, 0x0300c0700c2700c2, 0x0700c2700c2300c0,
       0x0700c2300c0300c0, 0x0320c8320c8700c2, 0x0b02c0720ca720ca, 0x0b02c0b02c0b02c0,
       0x0720ca320c8320c8, 0x0b02c0b02c0720ca, 0x008020b02c0b02c0, 0x0580234802218021,
       0x0480221802108020, 0x0190250902458023, 0x0280a05902749026, 0x0680a2680a2280a0,
       0x04a02a1a0290a028, 0x098221882205a02b, 0x00b02c9822188220, 0x05b02f4b02e1b02d,
       0x0a82a0a82a0a82a0, 0x0d0301c0300a82a0, 0x0c0300d0301c0300, 0x0d0301c0300d0301,
       0x0c1304d1305c1304, 0x0e0380e0380d1305, 0x0c0300e0380e0380, 0x0d0301c0300d0301,
       0x0c0300d0301c0300, 0x0d1305c1304d0301, 0x0e0380d1305c1304, 0x0e0380e0380e0380,
       0x04c0321c0310c030, 0x01c0310c0305c033, 0x00d0345c0334c032, 0x05d0374d0361d035,
       0x06c0b22c0b02c0b0, 0x01e0390e0386c0b2, 0x08c2305e03b4e03a, 0x09c2318c2309c231,
       0x04f03e1f03d0f03c, 0x0ac2b0ac2b05f03f, 0x0f03c0ac2b0ac2b0, 0x0f03c0f03c0f03c0,
       0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0,
       0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0, 0x0f03c0f03c0f03c0,
       0x0f03c0f03c0f03c0, 0x01000100000f03c0] := rfl

    -- The direct draft's three decisions over all their arguments, one bit each:
    -- drafted_keeps, 2 cases (its hold is there); spine_settles, 4 (k's bits: it
    -- drafted records, they were kept); draft_refuses, 8 (k's bits: it writes, a
    -- pending set does not open, a held draft does not read).
    def draftCase (k : Nat) : Nat :=
      if k < 2 then bitOf (spec_drafted_keeps (decide (k = 1)))
      else if k < 6 then
        bitOf (spec_spine_settles (decide ((k - 2) % 2 = 1)) (decide (((k - 2) / 2) % 2 = 1)))
      else
        bitOf (spec_draft_refuses (decide ((k - 6) % 2 = 1)) (decide (((k - 6) / 2) % 2 = 1))
          (decide (((k - 6) / 4) % 2 = 1)))
    def draftSheet : List Nat := [pack 2 (gridFrom draftCase 0 14)]
    theorem draftSheet_is : draftSheet = [0x0000000000002a36] := rfl


    -- ===========================================================================
    -- 12. THE SERVER'S COMPARE-AND-SET (server.rs takes_over, the rule a PUT and
    --     a version at the vault's store file keep alike, and transport.rs
    --     write_path_held, a publish to a vault at a path: both ask Takeover::of).
    --     The Worlds above judge every arrival by takesOver: here that is the
    --     decision both run, the five kernels and a refused pin answer every
    --     envelope exactly once, and the replaced pin, which takes a version with
    --     no sequence check, is named as the environment premise it is, with the
    --     run that loses an edit where it fails. Outside it: transport.rs
    --     replace_path's put-back over the version it read (an admin's copy put
    --     back over a version no reader accepts: a forgery, a retired or followed
    --     signer, client.rs's put_back), and a path that holds no envelope or
    --     another vault's, which a publish writes.
    -- ===========================================================================

    -- Takeover::of, one for one: 0 takes, 1 holds, 2 the pin refused, 3 not newer,
    -- 4 breaks the chain, 5 off the parents.
    def takeoverOf (p r n s e a h m : Bool) : Nat :=
      if spec_takes_head p r n e a h m = true then 0
      else if spec_holds_version p r n s = true then 1
      else if spec_refuses_not_newer p r n s = true then 3
      else if spec_refuses_chain p r n e = true then 4
      else if spec_refuses_parents p r n e a h m = true then 5
      else 2

    -- How many answers hold: the five kernels and a refused pin.
    def answersHolding (p r n s e a h m : Bool) : Nat :=
      bitOf (spec_takes_head p r n e a h m) + bitOf (spec_holds_version p r n s) + bitOf (!p)
        + bitOf (spec_refuses_not_newer p r n s) + bitOf (spec_refuses_chain p r n e)
        + bitOf (spec_refuses_parents p r n e a h m)

    -- Exactly one answer holds for every envelope and every version held, so the
    -- order Takeover::of asks in (the code's refusal order) changes nothing.
    theorem takeover_partitions (p r n s e a h m : Bool) : answersHolding p r n s e a h m = 1 := by
      cases p <;> cases r <;> cases n <;> cases s <;> cases e <;> cases a <;> cases h <;> cases m <;> rfl

    -- The server keeps the envelope: it takes it as its head, or holds it already.
    def serverKeeps (p r n s e a h m : Bool) : Bool :=
      spec_takes_head p r n e a h m || spec_holds_version p r n s

    theorem keeps_not_same (newer named p e a : Bool) :
        takesOver false newer named (p && e) = serverKeeps p false (!newer) false e a true named := by
      cases newer <;> cases named <;> cases p <;> cases e <;> cases a <;> rfl

    theorem keeps_same (named e a : Bool) :
        takesOver true false named (true && e) = serverKeeps true false (!false) true e a true named := by
      cases named <;> cases e <;> cases a <;> rfl

    -- THE WORLDS' ENVIRONMENT IS THE SERVER'S DECISION. On the cases the Worlds
    -- cover (a pin that was not replaced, a parented vault), takesOver with the pin
    -- and chain checks folded into its pinOk is exactly what the server keeps,
    -- wherever the version held is not newer than itself and passes its own pin
    -- check (the server took it under that pin, and its bytes carry the same signer
    -- and chain).
    theorem takes_over_is_the_kernel (same newer named p e a : Bool)
        (hsame : same = true → newer = false ∧ p = true) :
        takesOver same newer named (p && e) = serverKeeps p false (!newer) same e a true named := by
      cases same with
      | false => exact keeps_not_same newer named p e a
      | true =>
        cases newer with
        | true => exact Bool.noConfusion (hsame rfl).1
        | false =>
          cases p with
          | false => exact Bool.noConfusion (hsame rfl).2
          | true => exact keeps_same named e a

    theorem dec_lt_self (x : Nat) : decide (x < x) = false := by
      cases h : decide (x < x) with
      | false => rfl
      | true => exact absurd (of_decide_eq_true h) (Nat.lt_irrefl x)

    theorem dec_eq_true_of {x y : Nat} (h : decide (x = y) = true) : x = y := of_decide_eq_true h

    -- accept, as stArrive and stRepublish ask it, is the server's decision: the held
    -- version is its head, an envelope is newer by sequence and the same by identity,
    -- and the parents named are the version's spine.
    theorem accept_is_the_kernel (w : World) (s v : Nat) (p e a : Bool) (hp : w.head s = v → p = true) :
        accept w s v (p && e) =
          serverKeeps p false (!decide (w.seq (w.head s) < w.seq v)) (decide (w.head s = v)) e a true
            (elem (w.head s) (w.spine v)) :=
      takes_over_is_the_kernel (decide (w.head s = v)) (decide (w.seq (w.head s) < w.seq v))
        (elem (w.head s) (w.spine v)) p e a
        (fun h => by
          have heq : w.head s = v := dec_eq_true_of h
          exact ⟨Eq.mpr (congrArg (fun y => decide (w.seq y < w.seq v) = false) heq) (dec_lt_self (w.seq v)),
            hp heq⟩)

    -- A replaced pin takes the envelope with no sequence, chain or parents check.
    theorem replaced_takes_unchecked (n e a h m : Bool) : spec_takes_head true true n e a h m = true := rfl

    -- REPLACED PINS, the environment premise. Past (section 4) says a server that
    -- holds a version of v's sequence or later, other than v, never holds v again,
    -- and the settle rule rests on it: that is each server's head moving only
    -- forward, which takesOver keeps (accept_is_the_kernel). A server judges under a
    -- replaced pin (envelope.rs check_pin_with, Pin::Replaced) only while the line it
    -- holds gives way to the key its build names: a retired signer, or a retired key
    -- that endorsed a key the build does not know. The version it takes then is of
    -- the build's own line, and a release retires every key of the chain before the
    -- key it names (the ALAB_RETIRED line client.rs alab_release prints), so that
    -- version gives way to nothing: from then on the server judges by takesOver until
    -- its build changes, and a new build is a new server process, which ends every
    -- PUT in flight and refuses any version whose signer it retires. So a replaced
    -- pin judges only an arrival at a server that has not yet taken a version of the
    -- line, whose head is still the version the line started from (0 here). The
    -- held-draft World extended with such arrivals (stepReplaced) keeps HeldInv and
    -- loses no edit (held_safe_replaced, no_lost_edit_replaced); server/takes_over_tests.rs
    -- checks each clause on the real takes_over, and the run below shows the premise
    -- carries weight: a replaced pin at a server that took a head brings back the
    -- version a settled hold was of, and its edit is lost.
    def ReplacedOnlyAtTheStart (givesWay : Nat → Bool) (w : World) : Prop :=
      ∀ s, givesWay s = true → w.head s = 0

    -- Where a replaced pin judges: a server givesWay names, still at its line's start.
    def replacedHere (givesWay : Nat → Bool) (w : World) (s : Nat) : Bool :=
      givesWay s && decide (w.head s = 0)

    -- What server s keeps of v: as its replaced pin allows there
    -- (replaced_takes_unchecked), else takesOver.
    def judged (givesWay : Nat → Bool) (w : World) (s v : Nat) (ok : Bool) : Bool :=
      if replacedHere givesWay w s = true then ok else accept w s v ok

    def stArriveReplaced (givesWay : Nat → Bool) (w : World) (v x : Nat) (ok : Bool) : World :=
      match w.via v x with
      | none => w
      | some s =>
        if w.inflight v x then
          { w with inflight := upd2 w.inflight v x false,
                   res := upd2 w.res v x (some (judged givesWay w s v ok)),
                   head := if judged givesWay w s v ok = true then upd w.head s v else w.head }
        else w

    def stepReplaced (K : Kernels) (one moves givesWay : Nat → Bool) (w : World) : Event → World
      | Event.arrive v x ok => stArriveReplaced givesWay w v x ok
      | e => step K one moves w e

    def runReplaced (K : Kernels) (one moves givesWay : Nat → Bool) (w : World) : List Event → World
      | [] => w
      | e :: es => runReplaced K one moves givesWay (stepReplaced K one moves givesWay w e) es

    def HeldSpecReplaced (K : Kernels) (one moves givesWay : Nat → Bool) : StateMachine World :=
      StateMachine.mk (fun w => w = w0) (fun w w' => ∃ e, w' = stepReplaced K one moves givesWay w e)

    -- The guard is the premise: in a state where it holds, the extended step's
    -- arrival is a replaced pin's wherever givesWay names the server.
    theorem replaced_here_is_the_premise (givesWay : Nat → Bool) (w : World)
        (hp : ReplacedOnlyAtTheStart givesWay w) (s : Nat) : replacedHere givesWay w s = givesWay s := by
      unfold replacedHere
      cases hg : givesWay s with
      | false => rfl
      | true => rw [hd_dect (hp s hg)]

    -- Every drafted version is of sequence 1 or more, over version 0's 0.
    def SeqGround (w : World) : Prop := w.seq 0 = 0 ∧ ∀ v, ¬ (w.phase v = 0) → 1 ≤ w.seq v

    theorem sg_one (n : Nat) : 1 ≤ n + 1 := by omega
    theorem sg_ite (c : Bool) (t e : World) (ht : c = true → SeqGround t) (he : SeqGround e) :
        SeqGround (if c = true then t else e) := by
      cases c
      · exact he
      · exact ht rfl
    theorem sg_of (w w' : World) (h : SeqGround w) (es : w'.seq = w.seq)
        (ep : ∀ u, ¬ (w'.phase u = 0) → ¬ (w.phase u = 0)) : SeqGround w' := by
      refine ⟨?_, ?_⟩
      · rw [es]; exact h.1
      · intro u hu
        rw [es]
        exact h.2 u (ep u hu)
    theorem sg_set (w w' : World) (v n : Nat) (h : SeqGround w) (hv : ¬ (w.phase v = 0)) (es : w'.seq = w.seq)
        (ep : w'.phase = upd w.phase v n) : SeqGround w' :=
      sg_of w w' h es (fun u hu z => by
        by_cases e : u = v
        · rw [e] at z; exact hv z
        · rw [ep, hd_updO _ _ _ _ e] at hu; exact hu z)

    theorem sg_draft (w : World) (v o : Nat) (sp : List Nat) (m d : Bool) (r : List Nat) (h : SeqGround w) :
        SeqGround (stDraft w v o sp m d r) := by
      unfold stDraft
      refine sg_ite _ _ _ ?_ h
      intro g
      have hv0 : ¬ (v = 0) := hd_ndec (hd_andL (hd_andR g))
      refine ⟨?_, ?_⟩
      · show upd w.seq v (w.seq o + 1) 0 = 0
        rw [hd_updO _ _ _ _ (fun e => hv0 e.symm)]
        exact h.1
      · intro u hu
        show 1 ≤ upd w.seq v (w.seq o + 1) u
        by_cases e : u = v
        · rw [e, hd_updS]; exact sg_one (w.seq o)
        · rw [hd_updO _ _ _ _ e]
          exact h.2 u (fun z => hu (by show upd w.phase v 1 u = 0; rw [hd_updO _ _ _ _ e]; exact z))

    theorem sg_step (K : Kernels) (one moves : Nat → Bool) (w : World) (e : Event) (h : SeqGround w) :
        SeqGround (step K one moves w e) := by
      cases e with
      | draft v o sp m d r => exact sg_draft w v o sp m d r h
      | hold v unk =>
        show SeqGround (stHold w v unk)
        unfold stHold
        exact sg_ite _ _ _ (fun g => sg_set w _ v 2 h (fun z => hd_01 (Eq.trans z.symm (hd_dec g))) rfl rfl) h
      | send v x =>
        show SeqGround (stSend w v x)
        unfold stSend
        exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h
      | arrive v x ok =>
        show SeqGround (stArrive w v x ok)
        unfold stArrive
        cases w.via v x with
        | none => exact h
        | some s => exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h
      | refusedAll v =>
        show SeqGround (stRefused K w v)
        unfold stRefused
        exact sg_ite _ _ _ (fun g => sg_set w _ v 3 h (fun z => hd_02 (Eq.trans z.symm (hd_dec (hd_andL g)))) rfl rfl) h
      | keep v =>
        show SeqGround (stKeep K w v)
        unfold stKeep
        exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h
      | landed v tk =>
        show SeqGround (stLanded K one w v tk)
        unfold stLanded
        exact sg_ite _ _ _ (fun g => sg_set w _ v 3 h (fun z => hd_02 (Eq.trans z.symm (hd_dec (hd_andL g)))) rfl rfl) h
      | adopt v s =>
        show SeqGround (stAdopt K one w v s)
        unfold stAdopt
        exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h
      | republish v s =>
        show SeqGround (stRepublish w v s)
        unfold stRepublish
        exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h
      | move x s =>
        show SeqGround (stMove moves w x s)
        unfold stMove
        exact sg_ite _ _ _ (fun _ => sg_of w _ h rfl (fun u z => z)) h

    -- A replaced pin takes v at the start of a server's line: newer than version 0,
    -- so the server was past nothing of v (what hd_accept says of takesOver).
    theorem hd_arriveR (one givesWay : Nat → Bool) (w : World) (v x : Nat) (ok : Bool) (hi : HeldInv one w)
        (hs : SeqGround w) : HeldInv one (stArriveReplaced givesWay w v x ok) ∧ SeqGround (stArriveReplaced givesWay w v x ok) := by
      unfold stArriveReplaced
      cases hvx : w.via v x with
      | none => exact ⟨hi, hs⟩
      | some s0 =>
        refine ⟨?_, sg_ite _ _ _ (fun _ => sg_of w _ hs rfl (fun u z => z)) hs⟩
        refine hd_ite one _ _ _ ?_ hi
        intro hinf
        by_cases ha : judged givesWay w s0 v ok = true
        · have hacc : w.head s0 = v ∨ w.seq (w.head s0) < w.seq v ∧ ¬ Past w s0 v := by
            by_cases hr : replacedHere givesWay w s0 = true
            · have h0 : w.head s0 = 0 := hd_dec (hd_andR hr)
              have hp2 : 2 ≤ w.phase v := (hi.2.2.2.2.2.2.2.1 v x (fun e => hd_sn (Eq.trans hvx.symm e))).1
              have h1 : 1 ≤ w.seq v := hs.2 v (hd_ne02 _ hp2)
              have hlt : w.seq (w.head s0) < w.seq v := by rw [h0, hs.1]; exact h1
              exact Or.inr ⟨hlt, fun p => hd_ltIrr _ _ p.1 hlt⟩
            · have ea : accept w s0 v ok = true := by
                have t : judged givesWay w s0 v ok = accept w s0 v ok := if_neg hr
                rw [← t]; exact ha
              cases hd_accept w s0 v ok ea with
              | inl e => exact Or.inl e
              | inr e => exact Or.inr ⟨e, hd_acceptPast w s0 v ok ea⟩
          have nP : ¬ Past w s0 v := fun p => Or.elim hacc (fun e => p.2 e) (fun e => e.2 p)
          have hac : w.head s0 = v ∨ w.seq (w.head s0) < w.seq v :=
            Or.elim hacc (fun e => Or.inl e) (fun e => Or.inr e.1)
          exact hd_arriveT one w _ v x s0 hi hvx hinf hac nP rfl rfl rfl rfl rfl rfl
            (by show (if judged givesWay w s0 v ok = true then upd w.head s0 v else w.head) = upd w.head s0 v; rw [if_pos ha])
            rfl rfl rfl
            (by show upd2 w.res v x (some (judged givesWay w s0 v ok)) = upd2 w.res v x (some true); rw [ha])
            rfl rfl
        · have ha' := not_true_false ha
          exact hd_arriveF one w _ v x s0 hi hvx hinf rfl rfl rfl rfl rfl rfl
            (by show (if judged givesWay w s0 v ok = true then upd w.head s0 v else w.head) = w.head; rw [if_neg ha])
            rfl rfl rfl
            (by show upd2 w.res v x (some (judged givesWay w s0 v ok)) = upd2 w.res v x (some false); rw [ha'])
            rfl rfl

    def HeldInvR (one : Nat → Bool) (w : World) : Prop := HeldInv one w ∧ SeqGround w

    theorem hr_plain (K : Kernels) (hK : Exact K) (one moves : Nat → Bool) (hf : OneIsFixed one moves) (w : World)
        (e : Event) (hi : HeldInvR one w) : HeldInvR one (step K one moves w e) :=
      ⟨held_inv_step K hK one moves hf w _ hi.1 ⟨e, rfl⟩, sg_step K one moves w e hi.2⟩

    theorem held_inv_step_replaced (K : Kernels) (hK : Exact K) (one moves givesWay : Nat → Bool)
        (hf : OneIsFixed one moves) (w w' : World) (hi : HeldInvR one w)
        (hn : ∃ e, w' = stepReplaced K one moves givesWay w e) : HeldInvR one w' :=
      Exists.elim hn (fun e he => by
        rw [he]
        cases e with
        | arrive v x ok => exact hd_arriveR one givesWay w v x ok hi.1 hi.2
        | draft v o sp m d r => exact hr_plain K hK one moves hf w (Event.draft v o sp m d r) hi
        | hold v unk => exact hr_plain K hK one moves hf w (Event.hold v unk) hi
        | send v x => exact hr_plain K hK one moves hf w (Event.send v x) hi
        | refusedAll v => exact hr_plain K hK one moves hf w (Event.refusedAll v) hi
        | keep v => exact hr_plain K hK one moves hf w (Event.keep v) hi
        | landed v tk => exact hr_plain K hK one moves hf w (Event.landed v tk) hi
        | adopt v s => exact hr_plain K hK one moves hf w (Event.adopt v s) hi
        | republish v s => exact hr_plain K hK one moves hf w (Event.republish v s) hi
        | move x s => exact hr_plain K hK one moves hf w (Event.move x s) hi)

    -- HELD DRAFTS ARE SAFE WITH REPLACED PINS: every run of the held-draft World
    -- whose servers may judge an arrival by a replaced pin at the start of their
    -- line keeps HeldInv in every state.
    theorem held_safe_replaced (K : Kernels) (hK : Exact K) (one moves givesWay : Nat → Bool)
        (hf : OneIsFixed one moves) :
        Satisfies (HeldSpecReplaced K one moves givesWay) (Always (Lift (HeldInvR one))) :=
      invariantSatisfies (HeldSpecReplaced K one moves givesWay) (HeldInvR one)
        (fun s hs => by
          have e : s = w0 := hs
          rw [e]
          exact ⟨held_inv_init one, rfl, fun v h => absurd rfl h⟩)
        (fun s t hi hn => held_inv_step_replaced K hK one moves givesWay hf s t hi hn)

    theorem no_lost_edit_replaced (K : Kernels) (hK : Exact K) (one moves givesWay : Nat → Bool)
        (hf : OneIsFixed one moves) (carries : Nat → Nat → Prop) :
        Satisfies (HeldSpecReplaced K one moves givesWay)
          (Always (Lift (fun w => TextInheritance carries w → EditSafe carries one w))) :=
      fun b runs n ht => by
        have h : HeldInvR one (drop b n 0) := held_safe_replaced K hK one moves givesWay hf b runs n
        exact edit_safe_of carries one _ h.1.1 ht

    -- The arrival the premise forbids: a replaced pin wherever givesWay names the
    -- server, whatever it holds.
    def stArriveAnywhere (givesWay : Nat → Bool) (w : World) (v x : Nat) (ok : Bool) : World :=
      match w.via v x with
      | none => w
      | some s =>
        if w.inflight v x then
          { w with inflight := upd2 w.inflight v x false,
                   res := upd2 w.res v x (some (if givesWay s then ok else accept w s v ok)),
                   head := if (if givesWay s then ok else accept w s v ok) then upd w.head s v else w.head }
        else w

    def stepAnywhere (K : Kernels) (one moves givesWay : Nat → Bool) (w : World) : Event → World
      | Event.arrive v x ok => stArriveAnywhere givesWay w v x ok
      | e => step K one moves w e

    def runAnywhere (K : Kernels) (one moves givesWay : Nat → Bool) (w : World) : List Event → World
      | [] => w
      | e :: es => runAnywhere K one moves givesWay (stepAnywhere K one moves givesWay w e) es

    -- v1's PUT to s1 is in flight; v2, drafted from the same text, lands on s1 and
    -- settles v1's hold by reach; then v1 arrives.
    def landedFirst : List Event :=
      [ Event.draft 1 0 [0] true true [1], Event.hold 1 false, Event.send 1 1,
        Event.draft 2 0 [0] true true [1], Event.hold 2 false, Event.send 2 1, Event.arrive 2 1 true,
        Event.keep 2, Event.landed 2 [1] ]
    def lateArrival : List Event := landedFirst ++ [Event.arrive 1 1 true]

    def givesWayAtOne : Nat → Bool := fun s => s == 1

    theorem the_settled_hold_goes : (run spec oneAll movesNone w0 lateArrival).hold 1 = none := rfl
    -- s1 took v2 before v1 arrived: the premise fails exactly there.
    theorem s1_had_taken_v2 : (run spec oneAll movesNone w0 landedFirst).head 1 = 2 := rfl
    theorem a_forward_head_refuses_it : lostAt (run spec oneAll movesNone w0 lateArrival) 1 1 = false := rfl
    -- A replaced pin at s1's start (its first arrival, v2's) and then takesOver: v1 is refused.
    theorem a_replaced_pin_at_the_start_refuses_it :
        lostAt (runReplaced spec oneAll movesNone givesWayAtOne w0 lateArrival) 1 1 = false := rfl
    theorem a_replaced_pin_after_a_take_loses_v1 :
        lostAt (runAnywhere spec oneAll movesNone givesWayAtOne w0 lateArrival) 1 1 = true := rfl

    -- takes_over, 256 cases: k's bits pin ok, replaced, not newer, the same version,
    -- extends, names parents, the held vault parented, among the parents.
    def takeoverCase (k : Nat) : Nat :=
      takeoverOf (decide (k % 2 = 1)) (decide ((k / 2) % 2 = 1)) (decide ((k / 4) % 2 = 1))
        (decide ((k / 8) % 2 = 1)) (decide ((k / 16) % 2 = 1)) (decide ((k / 32) % 2 = 1))
        (decide ((k / 64) % 2 = 1)) (decide ((k / 128) % 2 = 1))
    def takeoverSheet : List Nat := wordsFrom 8 16 takeoverCase 0 16

    -- The words the sheet is, by the kernel's evaluation; no_lost_edit::tests
    -- recomputes them from Takeover::of and reads them here.
    theorem takeoverSheet_is : takeoverSheet =
      [0x000008a0a209a0a2, 0x000008a08209a082, 0x000008a0a209a0a2, 0x000008a0aa09a0aa,
       0x000008a0a209a0a2, 0x000008a0aa09a0aa, 0x000008a0a209a0a2, 0x000008a0aa09a0aa,
       0x000008a0a209a0a2, 0x000008a08209a082, 0x000008a0a209a0a2, 0x000008a08209a082,
       0x000008a0a209a0a2, 0x000008a08209a082, 0x000008a0a209a0a2, 0x000008a08209a082] := rfl

    -- ===========================================================================
    -- 13. THE UNVERSIONED HOLD (records.drafted), the one hold builds from
    --     2026-10-28 to 2026-11-05 kept: the records alone of a version it did not
    --     name. No build writes it now and nothing rewrites it, so it holds the
    --     records `orig` it was written with, or is gone. A head whose stamp it
    --     completes takes it in (adopt_held_draft: the store takes what it takes of
    --     it, since Store::ingest skips a record Store::insert refuses), a settled
    --     head may remove it (settle_held_drafts_locked), and other ingests grow the
    --     records store. Its law: every record it was written with is in the file or
    --     in the store, and nothing removes it otherwise. The store here is what
    --     Store::holds counts, its complete records: unversioned_goes asks that, not
    --     Store::contains, which counts a record waiting on a parent too, and the
    --     store drops waiting records to make room (store.rs make_room). The
    --     premise is that a complete record never leaves the store: no event below
    --     takes one out (the log refuses a complete record past its held total, and a
    --     records file recovers within it). held_draft_tests'
    --     a_record_the_store_holds_only_waiting_keeps_the_unversioned_hold shows the
    --     loss the rule by Store::contains had.
    -- ===========================================================================

    structure LKernels where
      adopts : Bool → Bool → Bool
      goes : Bool → Bool → Bool

    def LExact (K : LKernels) : Prop :=
      (∀ a b, K.adopts a b = spec_hold_adopts a b) ∧ (∀ a b, K.goes a b = spec_unversioned_goes a b)

    def lspec : LKernels := LKernels.mk spec_hold_adopts spec_unversioned_goes

    theorem lspec_exact : LExact lspec := ⟨fun _ _ => rfl, fun _ _ => rfl⟩

    theorem goes_true (K : LKernels) (hK : LExact K) (a b : Bool) (h : K.goes a b = true) : b = true := by
      rw [hK.2 a b] at h
      revert h
      cases a <;> cases b <;> decide

    structure LWorld where
      file : Bool
      store : Nat → Bool

    -- adopt ingested completes took: a head reads back; the ingest raised no error,
    -- its stamp is complete after it, and the store took the records `took`.
    -- settle reads: a settled head; the file reads and decodes, and the store loads.
    -- ingest r: a sync or a kept publish brings record r into the store.
    inductive LEvent where
      | adopt : Bool → Bool → List Nat → LEvent
      | settle : Bool → LEvent
      | ingest : Nat → LEvent

    def allStored (st : Nat → Bool) : List Nat → Bool
      | [] => true
      | r :: l => st r && allStored st l

    def addAll (st : Nat → Bool) (l : List Nat) : Nat → Bool := fun r => st r || elem r l

    def lAdopt (K : LKernels) (orig : List Nat) (w : LWorld) (ing comp : Bool) (took : List Nat) : LWorld :=
      if (w.file && K.adopts ing comp) = true then
        { file := !(K.goes true (allStored (addAll w.store took) orig)), store := addAll w.store took }
      else w

    def lSettle (K : LKernels) (orig : List Nat) (w : LWorld) (reads : Bool) : LWorld :=
      if (w.file && K.goes reads (allStored w.store orig)) = true then { w with file := false } else w

    def lIngest (w : LWorld) (r : Nat) : LWorld := { w with store := upd w.store r true }

    def lstep (K : LKernels) (orig : List Nat) (w : LWorld) : LEvent → LWorld
      | LEvent.adopt ing comp took => lAdopt K orig w ing comp took
      | LEvent.settle reads => lSettle K orig w reads
      | LEvent.ingest r => lIngest w r

    def lrun (K : LKernels) (orig : List Nat) (w : LWorld) : List LEvent → LWorld
      | [] => w
      | e :: es => lrun K orig (lstep K orig w e) es

    def lw0 (st : Nat → Bool) : LWorld := { file := true, store := st }

    def LegacySpec (K : LKernels) (orig : List Nat) (st : Nat → Bool) : StateMachine LWorld :=
      StateMachine.mk (fun w => w = lw0 st) (fun w w' => ∃ e, w' = lstep K orig w e)

    -- THE LAW: every record the file was written with is in the file or in the store.
    def LegacyKept (orig : List Nat) (w : LWorld) : Prop :=
      ∀ r, elem r orig = true → w.file = true ∨ w.store r = true

    -- Nothing removes the file otherwise: a step that removes it leaves the store
    -- holding every record it was written with.
    def GoesOnlyStored (orig : List Nat) (w w' : LWorld) : Prop :=
      w.file = true → w'.file = false → ∀ r, elem r orig = true → w'.store r = true

    theorem allStored_elem (st : Nat → Bool) (l : List Nat) : allStored st l = true →
        (r : Nat) → elem r l = true → st r = true := by
      induction l with
      | nil => intro _ r hr; exact ft hr
      | cons a l ih =>
        intro h r hr
        exact (if hra : r = a then Eq.mpr (congrArg (fun y => st y = true) hra) (hd_andL h)
          else ih (hd_andR h) r (Eq.trans (elem_cons_neg r a l hra).symm hr))

    theorem l_adopt_goes (K : LKernels) (hK : LExact K) (orig : List Nat) (w : LWorld) (ing comp : Bool)
        (took : List Nat) (r : Nat) (hr : elem r orig = true) (hw : (w.file && K.adopts ing comp) = true) :
        (lAdopt K orig w ing comp took).file = true ∨ (lAdopt K orig w ing comp took).store r = true := by
      unfold lAdopt
      rw [if_pos hw]
      cases hg : K.goes true (allStored (addAll w.store took) orig) with
      | false => exact Or.inl rfl
      | true => exact Or.inr (allStored_elem _ orig (goes_true K hK _ _ hg) r hr)

    theorem l_adopt_kept (K : LKernels) (hK : LExact K) (orig : List Nat) (w : LWorld) (ing comp : Bool)
        (took : List Nat) (hi : LegacyKept orig w) : LegacyKept orig (lAdopt K orig w ing comp took) := by
      intro r hr
      by_cases hw : (w.file && K.adopts ing comp) = true
      · exact l_adopt_goes K hK orig w ing comp took r hr hw
      · have e : lAdopt K orig w ing comp took = w := by unfold lAdopt; rw [if_neg hw]
        rw [e]
        exact hi r hr

    theorem l_settle_kept (K : LKernels) (hK : LExact K) (orig : List Nat) (w : LWorld) (reads : Bool)
        (hi : LegacyKept orig w) : LegacyKept orig (lSettle K orig w reads) := by
      intro r hr
      by_cases hw : (w.file && K.goes reads (allStored w.store orig)) = true
      · have e : lSettle K orig w reads = { w with file := false } := by unfold lSettle; rw [if_pos hw]
        rw [e]
        exact Or.inr (allStored_elem w.store orig (goes_true K hK _ _ (hd_andR hw)) r hr)
      · have e : lSettle K orig w reads = w := by unfold lSettle; rw [if_neg hw]
        rw [e]
        exact hi r hr

    theorem l_upd_true (f : Nat → Bool) (k x : Nat) (h : f x = true) : upd f k true x = true := by
      by_cases e : x = k
      · rw [e, hd_updS]
      · rw [hd_updO _ _ _ _ e]; exact h

    theorem l_ingest_kept (orig : List Nat) (w : LWorld) (r : Nat) (hi : LegacyKept orig w) :
        LegacyKept orig (lIngest w r) := by
      intro x hx
      cases hi x hx with
      | inl hf => exact Or.inl hf
      | inr hs => exact Or.inr (l_upd_true w.store r x hs)

    theorem l_step_kept (K : LKernels) (hK : LExact K) (orig : List Nat) (w : LWorld) (e : LEvent)
        (hi : LegacyKept orig w) : LegacyKept orig (lstep K orig w e) := by
      cases e with
      | adopt ing comp took => exact l_adopt_kept K hK orig w ing comp took hi
      | settle reads => exact l_settle_kept K hK orig w reads hi
      | ingest r => exact l_ingest_kept orig w r hi

    theorem l_init_kept (orig : List Nat) (st : Nat → Bool) : LegacyKept orig (lw0 st) :=
      fun _ _ => Or.inl rfl

    -- THE LAW, for any exact kernels, any records the file was written with and any
    -- store it started beside: in every state, each record is in the file or the store.
    theorem legacy_kept (K : LKernels) (hK : LExact K) (orig : List Nat) (st : Nat → Bool) :
        Satisfies (LegacySpec K orig st) (Always (Lift (LegacyKept orig))) :=
      invariantSatisfies (LegacySpec K orig st) (LegacyKept orig)
        (fun s hs => by
          have e : s = lw0 st := hs
          rw [e]
          exact l_init_kept orig st)
        (fun s t hi hn => Exists.elim hn (fun e he => by rw [he]; exact l_step_kept K hK orig s e hi))

    -- A step that keeps the file, or finds it gone, removes nothing.
    theorem l_step_goes (K : LKernels) (hK : LExact K) (orig : List Nat) (w : LWorld) (e : LEvent) :
        GoesOnlyStored orig w (lstep K orig w e) := by
      intro hf hg r hr
      cases e with
      | adopt ing comp took =>
        by_cases hw : (w.file && K.adopts ing comp) = true
        · cases l_adopt_goes K hK orig w ing comp took r hr hw with
          | inl h1 => exact absurd (Eq.trans h1.symm hg) (fun c => Bool.noConfusion c)
          | inr h2 => exact h2
        · have e : lAdopt K orig w ing comp took = w := by unfold lAdopt; rw [if_neg hw]
          have hg' : w.file = false := Eq.trans (congrArg LWorld.file e).symm hg
          exact absurd (Eq.trans hf.symm hg') (fun c => Bool.noConfusion c)
      | settle reads =>
        by_cases hw : (w.file && K.goes reads (allStored w.store orig)) = true
        · have e : lSettle K orig w reads = { w with file := false } := by unfold lSettle; rw [if_pos hw]
          show (lSettle K orig w reads).store r = true
          rw [e]
          exact allStored_elem w.store orig (goes_true K hK _ _ (hd_andR hw)) r hr
        · have e : lSettle K orig w reads = w := by unfold lSettle; rw [if_neg hw]
          have hg' : w.file = false := Eq.trans (congrArg LWorld.file e).symm hg
          exact absurd (Eq.trans hf.symm hg') (fun c => Bool.noConfusion c)
      | ingest x =>
        have hg' : w.file = false := hg
        exact absurd (Eq.trans hf.symm hg') (fun c => Bool.noConfusion c)

    theorem goes_only_stored (K : LKernels) (hK : LExact K) (orig : List Nat) (st : Nat → Bool) :
        Satisfies (LegacySpec K orig st) (Always (Act (GoesOnlyStored orig))) :=
      actionSatisfies (LegacySpec K orig st) (GoesOnlyStored orig)
        (fun s t ht => Exists.elim ht (fun e he => by rw [he]; exact l_step_goes K hK orig s e))
        (fun s hf hg => absurd (Eq.trans hf.symm hg) (fun c => Bool.noConfusion c))

    -- The removals the law forbids, each a run the kernel evaluates. A record is lost
    -- where the file is gone and the store lacks one it was written with.
    def recordLost (orig : List Nat) (w : LWorld) : Bool := !w.file && !allStored w.store orig

    def noneStored : Nat → Bool := fun _ => false

    -- The file removed once it reads, whatever the store holds.
    def lEarly : LKernels := { lspec with goes := fun reads _ => reads }

    theorem a_settle_keeps_the_unstored_file : recordLost [1] (lrun lspec [1] (lw0 noneStored) [LEvent.settle true]) = false := rfl
    theorem early_removal_loses_a_record : recordLost [1] (lrun lEarly [1] (lw0 noneStored) [LEvent.settle true]) = true := rfl

    -- The adoption before this step: the file removed once the head's stamp is
    -- complete, whatever record of it the store refused. The store takes record 1
    -- of records 1 and 2.
    def lAdoptRemoves (K : LKernels) (w : LWorld) (ing comp : Bool) (took : List Nat) : LWorld :=
      if (w.file && K.adopts ing comp) = true then { file := false, store := addAll w.store took } else w

    def partialAdopt : List LEvent := [LEvent.adopt true true [1]]

    theorem an_adoption_keeps_what_the_store_refused :
        recordLost [1, 2] (lrun lspec [1, 2] (lw0 noneStored) partialAdopt) = false := rfl
    theorem removing_at_adoption_loses_the_refused_record :
        recordLost [1, 2] (lAdoptRemoves lspec (lw0 noneStored) true true [1]) = true := rfl
    theorem a_complete_adoption_removes_it :
        (lrun lspec [1, 2] (lw0 noneStored) [LEvent.adopt true true [1, 2]]).file = false := rfl

    -- hold_adopts, then unversioned_goes, 4 cases each (k's bits: the first argument,
    -- the second), packed as section 11 packs its sheets.
    def legacyCase (k : Nat) : Nat :=
      if k < 4 then bitOf (spec_hold_adopts (decide (k % 2 = 1)) (decide ((k / 2) % 2 = 1)))
      else bitOf (spec_unversioned_goes (decide (k % 2 = 1)) (decide ((k / 2) % 2 = 1)))
    def legacySheet : List Nat := [pack 2 (gridFrom legacyCase 0 8)]
    theorem legacySheet_is : legacySheet = [0x0000000000000088] := rfl

    end AtermMeshNoLostEdit
}
