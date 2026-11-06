// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// P5, the relay's aterm half: the verdict of `relay.rs` and its laws, proved for every size by the
// kernel on every Trust build. The spec kernels are what relay.rs's contract! kernels state they
// compute; the folds are `Verdict::of`'s, one for one, over PF1's `anyN`; the World of section 4
// asks the kernels as parameters, and `relay_binding_island.rs` instantiates it with the compiled
// Rust. `relay::tests` recomputes section 8's answer sheet from the Rust and reads its words from
// this file.
clean {
    -- The verdict's scalar kernels. Each `contract!` kernel of
    -- crates/aterm-mesh-core/src/relay.rs states `ensures result ==
    -- spec_<name>(..) by <name>_is_exact`, and the binding island proves its
    -- compiled body equal to the definition here. They sit outside the namespace
    -- so a clause can name them.

    -- spec_origin: this Mac's own key, or a current key with no rival in its class.
    def spec_origin (isMe current rival : Bool) : Bool := isMe || (current && !rival)
    -- spec_key_revoked: not this Mac's, and gone (Removed or Retired) or contested.
    def spec_key_revoked (isMe current rival gone : Bool) : Bool :=
      !isMe && (gone || (current && rival))
    -- spec_principal_revoked: some key of the principal is revoked, and no origin holds it.
    def spec_principal_revoked (keyRevoked originHolds : Bool) : Bool := keyRevoked && !originHolds

    namespace AtermMeshRelay

    open AtermMeshTemporal

    -- ===========================================================================
    -- 1. A read, the kernels as parameters, and Verdict::of's folds.
    -- ===========================================================================

    -- Keys 0..n of a read as relay.rs's `Key`s: class, principal, current, gone;
    -- `me` is this Mac's key.
    structure View where
      n    : Nat
      me   : Nat
      cls  : Nat → Nat
      prin : Nat → Nat
      cur  : Nat → Bool
      gone : Nat → Bool

    -- Standings::is_current never seats a gone key.
    def Wf (v : View) : Prop := ∀ k, v.cur k = true → v.gone k = false

    structure Kernels where
      origin : Bool → Bool → Bool → Bool
      keyRevoked : Bool → Bool → Bool → Bool → Bool
      principalRevoked : Bool → Bool → Bool

    def Exact (K : Kernels) : Prop :=
      (∀ a b c, K.origin a b c = spec_origin a b c)
        ∧ (∀ a b c d, K.keyRevoked a b c d = spec_key_revoked a b c d)
        ∧ (∀ a b, K.principalRevoked a b = spec_principal_revoked a b)

    def spec : Kernels := Kernels.mk spec_origin spec_key_revoked spec_principal_revoked

    theorem spec_exact : Exact spec := ⟨fun _ _ _ => rfl, fun _ _ _ _ => rfl, fun _ _ => rfl⟩

    def isMe (v : View) (k : Nat) : Bool := decide (k = v.me)
    def rivalOf (v : View) (k j : Nat) : Bool :=
      !(decide (j = k)) && ((v.cur j || isMe v j) && decide (v.cls j = v.cls k))
    def rival (v : View) (k : Nat) : Bool := AtermMeshRevocation.anyN v.n (rivalOf v k)
    def originW (K : Kernels) (v : View) (k : Nat) : Bool := K.origin (isMe v k) (v.cur k) (rival v k)
    def revokedW (K : Kernels) (v : View) (k : Nat) : Bool :=
      K.keyRevoked (isMe v k) (v.cur k) (rival v k) (v.gone k)
    def revpW (K : Kernels) (v : View) (p : Nat) : Bool :=
      AtermMeshRevocation.anyN v.n (fun k => decide (v.prin k = p) && revokedW K v k)
    def holdsW (K : Kernels) (v : View) (p : Nat) : Bool :=
      AtermMeshRevocation.anyN v.n (fun k => decide (v.prin k = p) && originW K v k)
    def prinRevokedW (K : Kernels) (v : View) (p : Nat) : Bool :=
      K.principalRevoked (revpW K v p) (holdsW K v p)

    -- ===========================================================================
    -- 2. The kernels' facts, over every argument, for any exact K.
    -- ===========================================================================

    theorem kr_empty (K : Kernels) (hK : Exact K) (m c v g : Bool) (hc : c = false) (hg : g = false) :
        K.keyRevoked m c v g = false := by
      rw [hK.2.1 m c v g]; revert hc hg; cases m <;> cases c <;> cases v <;> cases g <;> decide
    theorem ko_empty (K : Kernels) (hK : Exact K) (m c v : Bool) (hc : c = false) :
        K.origin m c v = m := by
      rw [hK.1 m c v]; revert hc; cases m <;> cases c <;> cases v <;> decide
    theorem ko_not_revoked (K : Kernels) (hK : Exact K) (m c v g : Bool) (hc : c = true → g = false)
        (ho : K.origin m c v = true) : K.keyRevoked m c v g = false := by
      rw [hK.2.1 m c v g]; rw [hK.1 m c v] at ho
      revert hc ho; cases m <;> cases c <;> cases v <;> cases g <;> decide
    theorem ko_other (K : Kernels) (hK : Exact K) (m c v : Bool) (hm : m = false)
        (ho : K.origin m c v = true) : c = true ∧ v = false := by
      rw [hK.1 m c v] at ho; revert hm ho; cases m <;> cases c <;> cases v <;> decide
    theorem ko_me (K : Kernels) (hK : Exact K) (m c v : Bool) (hm : m = true) : K.origin m c v = true := by
      rw [hK.1 m c v]; revert hm; cases m <;> cases c <;> cases v <;> decide
    theorem kr_me (K : Kernels) (hK : Exact K) (m c v g : Bool) (hm : m = true) :
        K.keyRevoked m c v g = false := by
      rw [hK.2.1 m c v g]; revert hm; cases m <;> cases c <;> cases v <;> cases g <;> decide
    theorem kr_gone (K : Kernels) (hK : Exact K) (m c v g : Bool) (hm : m = false) (hg : g = true) :
        K.keyRevoked m c v g = true := by
      rw [hK.2.1 m c v g]; revert hm hg; cases m <;> cases c <;> cases v <;> cases g <;> decide
    theorem kr_contested (K : Kernels) (hK : Exact K) (m c v g : Bool) (hm : m = false) (hc : c = true)
        (hv : v = true) : K.keyRevoked m c v g = true ∧ K.origin m c v = false := by
      rw [hK.2.1 m c v g, hK.1 m c v]; revert hm hc hv
      cases m <;> cases c <;> cases v <;> cases g <;> decide
    theorem kp_unrevoked (K : Kernels) (hK : Exact K) (x y : Bool) (hx : x = false) :
        K.principalRevoked x y = false := by
      rw [hK.2.2 x y]; revert hx; cases x <;> cases y <;> decide
    theorem kp_held (K : Kernels) (hK : Exact K) (x y : Bool) (hy : y = true) :
        K.principalRevoked x y = false := by
      rw [hK.2.2 x y]; revert hy; cases x <;> cases y <;> decide
    theorem kp_revoked (K : Kernels) (hK : Exact K) (x y : Bool) (hx : x = true) (hy : y = false) :
        K.principalRevoked x y = true := by
      rw [hK.2.2 x y]; revert hx hy; cases x <;> cases y <;> decide

    -- ===========================================================================
    -- 3. What the folds give.
    -- ===========================================================================

    theorem isMe_self (v : View) : isMe v v.me = true := AtermMeshRevocation.dtrue rfl
    theorem isMe_other (v : View) (k : Nat) (hk : k ≠ v.me) : isMe v k = false :=
      AtermMeshRevocation.dfalse hk

    theorem seated_or_me (v : View) (j : Nat) (hc : v.cur j = true ∨ j = v.me) :
        (v.cur j || isMe v j) = true := by
      cases hc with
      | inl h => rw [h]
      | inr h =>
        have hm : isMe v j = true := AtermMeshRevocation.dtrue h
        rw [hm]; exact Bool.or_true _

    -- Another current key (this Mac's counted) of k's class is k's rival.
    theorem rival_intro (v : View) (k j : Nat) (hj : j < v.n) (hne : j ≠ k)
        (hc : v.cur j = true ∨ j = v.me) (hcls : v.cls j = v.cls k) : rival v k = true := by
      have hr : rivalOf v k j = true := by
        unfold rivalOf
        rw [AtermMeshRevocation.dfalse hne, seated_or_me v j hc, AtermMeshRevocation.dtrue hcls]
      exact AtermMeshRevocation.anyN_intro (rivalOf v k) v.n j hj hr

    -- An origin holds its own principal.
    theorem holds_intro (K : Kernels) (v : View) (k : Nat) (hk : k < v.n) (ho : originW K v k = true) :
        holdsW K v (v.prin k) = true := by
      have hp : (decide (v.prin k = v.prin k) && originW K v k) = true := by
        rw [AtermMeshRevocation.dtrue (rfl : v.prin k = v.prin k), ho]
      exact AtermMeshRevocation.anyN_intro _ v.n k hk hp

    -- A revoked key's principal has a revoked key.
    theorem revp_intro (K : Kernels) (v : View) (k : Nat) (hk : k < v.n) (hr : revokedW K v k = true) :
        revpW K v (v.prin k) = true := by
      have hp : (decide (v.prin k = v.prin k) && revokedW K v k) = true := by
        rw [AtermMeshRevocation.dtrue (rfl : v.prin k = v.prin k), hr]
      exact AtermMeshRevocation.anyN_intro _ v.n k hk hp

    -- With no key revoked, no principal has one.
    theorem revp_none (K : Kernels) (v : View) (p : Nat) (h : ∀ k, revokedW K v k = false) :
        revpW K v p = false :=
      AtermMeshRevocation.anyN_false _ v.n (fun k _ => by rw [h k]; exact Bool.and_false _)

    -- ===========================================================================
    -- 4. TlaStandings: the revocation pass (Mesh::apply), for every read.
    -- ===========================================================================

    structure World where
      view    : View
      revoked : Nat → Bool   -- each broker's revoked keys
      rprin   : Nat → Bool   -- each broker's revoked principals
      origin  : Nat → Bool   -- set_origin_keys

    -- What a broker does with one flag across a pass: revoke what entered, lift what left.
    def step (was now old : Bool) : Bool :=
      if now = true ∧ was = false then true else if was = true ∧ now = false then false else old

    theorem step_exact (was now : Bool) : step was now was = now := by
      cases was <;> cases now <;> decide

    -- Before the first read: nothing seated, nothing revoked, this Mac its own origin.
    def Init (w : World) : Prop :=
      Wf w.view ∧ (∀ k, w.view.cur k = false ∧ w.view.gone k = false) ∧
      (∀ k, w.revoked k = false) ∧ (∀ p, w.rprin p = false) ∧ (∀ k, w.origin k = isMe w.view k)

    -- One read, its verdict diffed against the last one's, on every broker; this Mac's key stays.
    def Apply (K : Kernels) (w w' : World) : Prop :=
      Wf w'.view ∧ w'.view.me = w.view.me ∧
      (∀ k, w'.revoked k = step (revokedW K w.view k) (revokedW K w'.view k) (w.revoked k)) ∧
      (∀ p, w'.rprin p = step (prinRevokedW K w.view p) (prinRevokedW K w'.view p) (w.rprin p)) ∧
      (∀ k, w'.origin k = originW K w'.view k)

    def SpecW (K : Kernels) : StateMachine World := StateMachine.mk Init (Apply K)

    -- The law: broker state is the verdict of the read applied, never of the path to it.
    def Inv (K : Kernels) (w : World) : Prop :=
      Wf w.view ∧ (∀ k, w.revoked k = revokedW K w.view k) ∧
      (∀ p, w.rprin p = prinRevokedW K w.view p) ∧ (∀ k, w.origin k = originW K w.view k)

    theorem init_inv (K : Kernels) (hK : Exact K) (w : World) (h : Init w) : Inv K w := by
      have hk : ∀ k, revokedW K w.view k = false := fun k =>
        kr_empty K hK (isMe w.view k) (w.view.cur k) (rival w.view k) (w.view.gone k)
          (h.2.1 k).1 (h.2.1 k).2
      refine ⟨h.1, ?_, ?_, ?_⟩
      · intro k
        rw [h.2.2.1 k, hk k]
      · intro p
        rw [h.2.2.2.1 p]
        exact (kp_unrevoked K hK _ _ (revp_none K w.view p hk)).symm
      · intro k
        rw [h.2.2.2.2 k]
        exact (ko_empty K hK (isMe w.view k) (w.view.cur k) (rival w.view k) (h.2.1 k).1).symm

    theorem apply_inv (K : Kernels) (w w' : World) (hi : Inv K w) (ha : Apply K w w') : Inv K w' := by
      refine ⟨ha.1, ?_, ?_, ?_⟩
      · intro k
        rw [ha.2.2.1 k, hi.2.1 k, step_exact]
      · intro p
        rw [ha.2.2.2.1 p, hi.2.2.1 p, step_exact]
      · intro k
        exact ha.2.2.2.2 k

    theorem standings_safe (K : Kernels) (hK : Exact K) : Satisfies (SpecW K) (Always (Lift (Inv K))) :=
      invariantSatisfies (SpecW K) (Inv K) (init_inv K hK) (apply_inv K)

    -- ===========================================================================
    -- 5. The laws of one verdict.
    -- ===========================================================================

    -- NoCurrentSeatRevoked: no origin stands revoked, by its key or by its principal.
    theorem no_origin_revoked (K : Kernels) (hK : Exact K) (w : World) (hi : Inv K w) (k : Nat)
        (hk : k < w.view.n) (ho : w.origin k = true) :
        w.revoked k = false ∧ w.rprin (w.view.prin k) = false := by
      have ho' : originW K w.view k = true := by rw [← hi.2.2.2 k]; exact ho
      refine ⟨?_, ?_⟩
      · rw [hi.2.1 k]
        exact ko_not_revoked K hK _ _ _ _ (hi.1 k) ho'
      · rw [hi.2.2.1 (w.view.prin k)]
        exact kp_held K hK _ _ (holds_intro K w.view k hk ho')

    -- This Mac is always its own origin, and never revoked by its key.
    theorem me_is_origin (K : Kernels) (hK : Exact K) (v : View) : originW K v v.me = true :=
      ko_me K hK _ _ _ (isMe_self v)

    theorem me_not_revoked (K : Kernels) (hK : Exact K) (v : View) : revokedW K v v.me = false :=
      kr_me K hK _ _ _ _ (isMe_self v)

    -- An origin other than this Mac's is current and has no rival.
    theorem origin_other (K : Kernels) (hK : Exact K) (v : View) (k : Nat) (hk : k ≠ v.me)
        (ho : originW K v k = true) : v.cur k = true ∧ rival v k = false :=
      ko_other K hK _ _ _ (isMe_other v k hk) ho

    theorem bool_ne (h : false = true) : False := Bool.noConfusion h

    -- At most one origin per class: two keys a broker would file as one are never both origins.
    theorem one_origin_per_class (K : Kernels) (hK : Exact K) (v : View) (a b : Nat)
        (ha : a < v.n) (hb : b < v.n) (oa : originW K v a = true) (ob : originW K v b = true)
        (hcls : v.cls a = v.cls b) : a = b := by
      by_cases hab : a = b
      · exact hab
      · by_cases hbm : b = v.me
        · have ham : a ≠ v.me := fun e => hab (e.trans hbm.symm)
          have hno : rival v a = false := (origin_other K hK v a ham oa).2
          have hyes : rival v a = true :=
            rival_intro v a b hb (fun e => hab e.symm) (Or.inr hbm) hcls.symm
          rw [hno] at hyes
          exact (bool_ne hyes).elim
        · have hno : rival v b = false := (origin_other K hK v b hbm ob).2
          have hca : v.cur a = true ∨ a = v.me := by
            by_cases ham : a = v.me
            · exact Or.inr ham
            · exact Or.inl (origin_other K hK v a ham oa).1
          have hyes : rival v b = true := rival_intro v b a ha hab hca hcls
          rw [hno] at hyes
          exact (bool_ne hyes).elim

    -- A removed or retired key, other than this Mac's, is revoked by its key.
    theorem gone_revoked (K : Kernels) (hK : Exact K) (v : View) (k : Nat) (hk : k ≠ v.me)
        (hg : v.gone k = true) : revokedW K v k = true :=
      kr_gone K hK _ _ _ _ (isMe_other v k hk) hg

    -- Two current keys of one class: the one that is not this Mac's is revoked, never an origin.
    theorem contested_revoked (K : Kernels) (hK : Exact K) (v : View) (a b : Nat) (hb : b < v.n)
        (hab : b ≠ a) (ham : a ≠ v.me) (hca : v.cur a = true) (hcb : v.cur b = true ∨ b = v.me)
        (hcls : v.cls b = v.cls a) : revokedW K v a = true ∧ originW K v a = false :=
      kr_contested K hK _ _ _ _ (isMe_other v a ham) hca (rival_intro v a b hb hab hcb hcls)

    -- ===========================================================================
    -- 6. Negative controls, each evaluated by the kernel or derived for every
    --    World: the rules PF5 replaced answer otherwise.
    -- ===========================================================================

    -- Before PF5: an origin is any current key, a key is revoked when gone, and a
    -- principal with a revoked key, held or not.
    def prePF5 : Kernels :=
      Kernels.mk (fun m c _ => m || c) (fun m _ _ g => !m && g) (fun x _ => x)

    -- Keys 1 and 2 are current and of one class (they collide); key 3 is removed and
    -- shares key 1's principal.
    def collide : View :=
      { n := 4, me := 0,
        cls := fun k => if k = 0 then 0 else 5,
        prin := fun k => if k = 3 then 1 else k,
        cur := fun k => decide (k = 1) || decide (k = 2),
        gone := fun k => decide (k = 3) }

    theorem pre_pf5_takes_two_origins_of_one_class :
        And (originW prePF5 collide 1 = true) (originW prePF5 collide 2 = true) := ⟨rfl, rfl⟩

    theorem the_verdict_takes_neither :
        And (originW spec collide 1 = false) (originW spec collide 2 = false) := ⟨rfl, rfl⟩

    -- Key 1 alone in its class this time: an origin whose principal a removed key shares.
    def underOrigin : View := { collide with cls := fun k => k }

    theorem pre_pf5_revokes_an_origins_principal :
        And (originW prePF5 underOrigin 1 = true) (prinRevokedW prePF5 underOrigin 1 = true) :=
      ⟨rfl, rfl⟩

    theorem the_verdict_keeps_it :
        And (originW spec underOrigin 1 = true) (prinRevokedW spec underOrigin 1 = false) :=
      ⟨rfl, rfl⟩

    -- The pass before PF5 lifted a principal with any one key that left the revoked
    -- set (unrevoke_identity). Whenever one key of a principal leaves while another
    -- enters and no origin holds it, that leaves the principal lifted where the
    -- verdict revokes it.
    def lifted (K : Kernels) (v v' : View) (p : Nat) : Prop :=
      ∃ k, v.prin k = p ∧ v'.prin k = p ∧ revokedW K v k = true ∧ revokedW K v' k = false

    def PerKeyLift (K : Kernels) (w w' : World) : Prop :=
      ∀ p, lifted K w.view w'.view p → w'.rprin p = false

    theorem per_key_lift_breaks_the_law (K : Kernels) (hK : Exact K) (w w' : World)
        (hm : PerKeyLift K w w') (a b : Nat) (hb : b < w'.view.n)
        (left : lifted K w.view w'.view (w'.view.prin b))
        (entered : revokedW K w'.view b = true)
        (unheld : holdsW K w'.view (w'.view.prin b) = false) : ¬ Inv K w' := by
      intro hi
      have h0 : w'.rprin (w'.view.prin b) = false := hm _ left
      have h1 : w'.rprin (w'.view.prin b) = true := by
        rw [hi.2.2.1 (w'.view.prin b)]
        exact kp_revoked K hK _ _ (revp_intro K w'.view b hb entered) unheld
      rw [h0] at h1
      exact bool_ne h1

    -- ===========================================================================
    -- 7. TlaHold: a record the neighbour answered unknown (astream's
    --    `FederationBridge::hold_while`, asked through `Mesh::still_seats`).
    -- ===========================================================================

    structure Hold where
      dseat     : Bool   -- the neighbour seats the origin
      sseat     : Bool   -- this Mac seats the origin
      held      : Bool
      delivered : Bool
      letGo     : Bool

    def HInit (s : Hold) : Prop := s.held = true ∧ s.delivered = false ∧ s.letGo = false

    -- Either side's standings change at will.
    def Seat (s t : Hold) : Prop := ∃ d e, t = { s with dseat := d, sseat := e }

    -- One burst or idle mark: let go first if this Mac no longer seats the origin
    -- (`Held::let_go`), else offer the record; the neighbour takes it if it seats it.
    def Probe (s t : Hold) : Prop :=
      (s.held = true ∧ s.sseat = false ∧ t = { s with held := false, letGo := true }) ∨
      (s.held = true ∧ s.sseat = true ∧ s.dseat = true ∧ t = { s with held := false, delivered := true }) ∨
      (s.held = true ∧ s.sseat = true ∧ s.dseat = false ∧ t = s) ∨
      (s.held = false ∧ t = s)

    def HNext (s t : Hold) : Prop := Seat s t ∨ Probe s t

    def HSpec : StateMachine Hold := StateMachine.mk HInit HNext

    -- A record is let go only from a state where this Mac did not seat its origin.
    theorem probe_safe (s t : Hold) (hp : Probe s t) (hs : s.letGo = false) (ht : t.letGo = true) :
        s.sseat = false := by
      rcases hp with ⟨_, hss, _⟩ | ⟨_, _, _, e⟩ | ⟨_, _, _, e⟩ | ⟨_, e⟩
      · exact hss
      · have hl : t.letGo = s.letGo := by rw [e]
        rw [hl, hs] at ht; exact absurd ht (by decide)
      · have hl : t.letGo = s.letGo := by rw [e]
        rw [hl, hs] at ht; exact absurd ht (by decide)
      · have hl : t.letGo = s.letGo := by rw [e]
        rw [hl, hs] at ht; exact absurd ht (by decide)

    -- While this Mac seats the origin at every step, nothing is let go.
    theorem never_let_go (b : Behavior Hold) (runs : Runs HSpec b) (hs : ∀ n, (b n).sseat = true) :
        ∀ n, (b n).letGo = false := by
      intro n
      induction n with
      | zero => exact runs.1.2.2
      | succ k ih =>
        rcases runs.2 k with hn | stut
        · rcases hn with ⟨d, e, ht⟩ | hp
          · rw [ht]; exact ih
          · cases hl : (b (Nat.succ k)).letGo
            · rfl
            · have := probe_safe (b k) (b (Nat.succ k)) hp ih hl
              rw [hs k] at this; exact absurd this (by decide)
        · rw [stut]; exact ih

    theorem held_or_delivered (b : Behavior Hold) (runs : Runs HSpec b) (hs : ∀ n, (b n).sseat = true) :
        ∀ n, (b n).held = true ∨ (b n).delivered = true := by
      intro n
      induction n with
      | zero => exact Or.inl runs.1.1
      | succ k ih =>
        rcases runs.2 k with hn | stut
        · rcases hn with ⟨d, e, ht⟩ | hp
          · rw [ht]; exact ih
          · rcases hp with ⟨_, hss, _⟩ | ⟨_, _, _, e⟩ | ⟨_, _, _, e⟩ | ⟨hh, e⟩
            · rw [hs k] at hss; exact absurd hss (by decide)
            · exact Or.inr ((congrArg Hold.delivered e).trans rfl)
            · rw [e]; exact ih
            · rw [e]; exact ih
        · rw [stut]; exact ih

    -- Liveness: if this Mac seats the origin throughout, the neighbour seats it from
    -- some point on, and the relay probes infinitely often (each burst and idle
    -- mark), the record is delivered.
    theorem held_record_is_delivered (b : Behavior Hold) (runs : Runs HSpec b)
        (hs : ∀ n, (b n).sseat = true) (n0 : Nat) (hd : ∀ n, n0 ≤ n → (b n).dseat = true)
        (fair : ∀ n, ∃ m, n ≤ m ∧ Probe (b m) (b (Nat.succ m))) :
        ∃ n, (b n).delivered = true := by
      have ⟨m, hm, hp⟩ := fair n0
      rcases held_or_delivered b runs hs m with hh | hdel
      · refine ⟨Nat.succ m, ?_⟩
        rcases hp with ⟨_, hss, _⟩ | ⟨_, _, _, e⟩ | ⟨_, _, hds, _⟩ | ⟨hf, _⟩
        · rw [hs m] at hss; exact absurd hss (by decide)
        · exact (congrArg Hold.delivered e).trans rfl
        · rw [hd m hm] at hds; exact absurd hds (by decide)
        · rw [hh] at hf; exact absurd hf (by decide)
      · exact ⟨m, hdel⟩

    -- ===========================================================================
    -- 8. The answer sheet. One byte per view of a fixed grid of 1024 views (this
    --    Mac's key and two others, each current or not and gone or not; key 1 of
    --    this Mac's class or its own, of this Mac's principal or its own; key 2 of
    --    this Mac's class or key 1's own, of key 1's own principal or its own). The
    --    Rust test `relay::tests::answer_sheet_matches_the_island` recomputes it
    --    from `Verdict::of` and reads the words below from this file. Until TG12
    --    checks the Rust folds' kernels in every build, this sheet is what holds the
    --    folds to the island; TG12 removes it.
    -- ===========================================================================

    def flag (k b : Nat) : Bool := decide ((k / b) % 2 = 1)

    -- View k of the grid, ten bits: bits 0-1 this Mac's current and gone, bits 2-5
    -- key 1's current, gone, class and principal, bits 6-9 key 2's.
    def gridView (k : Nat) : View :=
      { n := 3, me := 0,
        cls := fun i => if i = 1 then (k / 16) % 2 else if i = 2 then (k / 256) % 2 else 0,
        prin := fun i => if i = 1 then (k / 32) % 2 else if i = 2 then 1 + (k / 512) % 2 else 0,
        cur := fun i => if i = 0 then flag k 1 else if i = 1 then flag k 4 else flag k 64,
        gone := fun i => if i = 0 then flag k 2 else if i = 1 then flag k 8 else flag k 128 }

    def bit (b : Bool) (v : Nat) : Nat := if b = true then v else 0

    def gridByte (v : View) : Nat :=
      bit (originW spec v 1) 1 + bit (originW spec v 2) 2 + bit (revokedW spec v 1) 4
        + bit (revokedW spec v 2) 8 + bit (prinRevokedW spec v 1) 16
        + bit (prinRevokedW spec v 2) 32 + bit (rival v 1) 64 + bit (rival v 2) 128

    -- Eight views to a word (Clean's numerals fit 64 bits), view 8w's byte first.
    def wordOf (w : Nat) : Nat → Nat
      | 0 => 0
      | Nat.succ n => 256 * wordOf w n + gridByte (gridView (8 * w + n))

    def sheetFrom (w : Nat) : Nat → List Nat
      | 0 => []
      | Nat.succ n => wordOf w 8 :: sheetFrom (w + 1) n

    -- Views 0 to 1023, as 128 words.
    def sheet : List Nat := sheetFrom 0 128

    theorem sheet_is : sheet =
      [0xc0c0c0c0c4c4c4c4, 0xc4c4c4c4c4c4c4c4, 0x8080808081818181, 0x8484848485858585,
       0xc0c0c0c0d4d4d4d4, 0xd4d4d4d4d4d4d4d4, 0x8080808081818181, 0x9494949485858585,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989899999999, 0x9c9c9c9c9d9d9d9d,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989889898989, 0x9c9c9c9c8d8d8d8d,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989899999999, 0x9c9c9c9c9d9d9d9d,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989889898989, 0x9c9c9c9c8d8d8d8d,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989899999999, 0x9c9c9c9c9d9d9d9d,
       0xd8d8d8d8dcdcdcdc, 0xdcdcdcdcdcdcdcdc, 0x9898989889898989, 0x9c9c9c9c8d8d8d8d,
       0x4040404044444444, 0x4444444444444444, 0x0000000081818181, 0x0404040485858585,
       0x4040404054545454, 0x5454545454545454, 0x0000000081818181, 0x1414141485858585,
       0x4242424246464646, 0x4646464646464646, 0x42424242dcdcdcdc, 0x46464646dcdcdcdc,
       0x4242424246464646, 0x4646464646464646, 0x42424242dcdcdcdc, 0x46464646dcdcdcdc,
       0x585858585c5c5c5c, 0x5c5c5c5c5c5c5c5c, 0x1818181899999999, 0x1c1c1c1c9d9d9d9d,
       0x585858585c5c5c5c, 0x5c5c5c5c5c5c5c5c, 0x1818181889898989, 0x1c1c1c1c8d8d8d8d,
       0x4a4a4a4a4e4e4e4e, 0x4e4e4e4e4e4e4e4e, 0x4a4a4a4adcdcdcdc, 0x4e4e4e4edcdcdcdc,
       0x4a4a4a4a4e4e4e4e, 0x4e4e4e4e4e4e4e4e, 0x4a4a4a4adcdcdcdc, 0x4e4e4e4edcdcdcdc,
       0xc0c0c0c0c4c4c4c4, 0xc4c4c4c4c4c4c4c4, 0x8080808081818181, 0x8484848485858585,
       0xc0c0c0c0d4d4d4d4, 0xd4d4d4d4d4d4d4d4, 0x8080808081818181, 0x9494949485858585,
       0xe8e8e8e8ecececec, 0xecececececececec, 0xa8a8a8a8a9a9a9a9, 0xacacacacadadadad,
       0xe8e8e8e8fcfcfcfc, 0xfcfcfcfcfcfcfcfc, 0xa8a8a8a8a9a9a9a9, 0xbcbcbcbcadadadad,
       0xe8e8e8e8ecececec, 0xecececececececec, 0xa8a8a8a8a9a9a9a9, 0xacacacacadadadad,
       0xe8e8e8e8fcfcfcfc, 0xfcfcfcfcfcfcfcfc, 0xa8a8a8a8a9a9a9a9, 0xbcbcbcbcadadadad,
       0xe8e8e8e8ecececec, 0xecececececececec, 0xa8a8a8a8a9a9a9a9, 0xacacacacadadadad,
       0xe8e8e8e8fcfcfcfc, 0xfcfcfcfcfcfcfcfc, 0xa8a8a8a8a9a9a9a9, 0xbcbcbcbcadadadad,
       0x4040404044444444, 0x4444444444444444, 0x0000000081818181, 0x0404040485858585,
       0x4040404054545454, 0x5454545454545454, 0x0000000081818181, 0x1414141485858585,
       0x4242424246464646, 0x4646464646464646, 0x42424242ecececec, 0x46464646ecececec,
       0x4242424256565656, 0x5656565656565656, 0x42424242fcfcfcfc, 0x56565656fcfcfcfc,
       0x686868686c6c6c6c, 0x6c6c6c6c6c6c6c6c, 0x28282828a9a9a9a9, 0x2c2c2c2cadadadad,
       0x686868687c7c7c7c, 0x7c7c7c7c7c7c7c7c, 0x28282828a9a9a9a9, 0x3c3c3c3cadadadad,
       0x4a4a4a4a4e4e4e4e, 0x4e4e4e4e4e4e4e4e, 0x4a4a4a4aecececec, 0x4e4e4e4eecececec,
       0x4a4a4a4a5e5e5e5e, 0x5e5e5e5e5e5e5e5e, 0x4a4a4a4afcfcfcfc, 0x5e5e5e5efcfcfcfc] := rfl

    end AtermMeshRelay
}
